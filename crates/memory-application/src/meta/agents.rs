//! Agents, tasks, task-agent links and participation logs.

use memory_domain::error::{DomainError, DomainResult};
use memory_domain::meta::{
    chat_memory_asset_id, Agent, AgentStatus, ParticipationLog, Task, TaskAgent, TaskStatus,
    Visibility,
};
use memory_domain::scope::DepartmentId;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

use super::{denied, page_of, require_non_empty, Caller, MetaPage, MetaService, Pagination};
use crate::common::{BatchDeleteResult, FailedId};
use crate::ports::meta::{AgentFilter, ParticipationFilter, TaskFilter};

#[derive(Debug, Deserialize)]
pub struct CreateAgentInput {
    pub team_id: String,
    pub owner_user_id: String,
    pub name: String,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub visibility: Option<String>,
    pub metadata_json: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAgentInput {
    pub agent_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub visibility: Option<String>,
    pub status: Option<String>,
    pub metadata_json: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListAgentsInput {
    pub team_id: Option<String>,
    pub owner_user_id: Option<String>,
    pub status: Option<String>,
    pub name: Option<String>,
    #[serde(flatten)]
    pub pagination: Pagination,
}

#[derive(Debug, Deserialize)]
pub struct LinkedAgent {
    pub agent_id: String,
    pub role_in_task: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTaskInput {
    pub team_id: String,
    pub creator_user_id: String,
    pub title: String,
    pub description: Option<String>,
    pub source_type: Option<String>,
    pub source_url: Option<String>,
    pub status: Option<String>,
    pub auto_assign_floating_assets: Option<bool>,
    pub risk_level: Option<String>,
    pub metadata_json: Option<Value>,
    #[serde(default)]
    pub linked_agents: Vec<LinkedAgent>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskInput {
    pub task_id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub source_url: Option<String>,
    pub risk_level: Option<String>,
    pub metadata_json: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListTasksInput {
    pub team_id: Option<String>,
    pub creator_user_id: Option<String>,
    pub status: Option<String>,
    #[serde(flatten)]
    pub pagination: Pagination,
}

#[derive(Debug, Deserialize)]
pub struct AppendParticipationInput {
    pub team_id: String,
    pub task_id: String,
    pub agent_id: String,
    pub user_id: String,
    pub created_at: Option<String>,
    pub source: Option<String>,
    pub metadata_json: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ListParticipationInput {
    pub team_id: String,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    #[serde(default)]
    pub dedupe: bool,
    #[serde(flatten)]
    pub pagination: Pagination,
}

impl MetaService {
    pub async fn create_agent(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: CreateAgentInput,
    ) -> DomainResult<Agent> {
        require_non_empty("name", &input.name)?;
        if caller.is_system_admin {
            self.get_team(department, &input.team_id).await?;
        } else {
            let member = self
                .require_member(department, &input.team_id, caller)
                .await?;
            if input.owner_user_id != caller.user_id && !member.is_admin() {
                return Err(denied("only team admins create agents for others"));
            }
        }
        let now = self.now();
        let agent = Agent {
            agent_id: self.entity_id("agt"),
            team_id: input.team_id,
            owner_user_id: input.owner_user_id,
            name: input.name,
            description: input.description,
            prompt: input.prompt,
            visibility: input
                .visibility
                .as_deref()
                .map(Visibility::parse)
                .transpose()?
                .unwrap_or(Visibility::Team),
            status: AgentStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            metadata_json: input.metadata_json.unwrap_or_else(|| json!({})),
        };
        self.store.insert_agent(department, &agent).await?;
        if let Err(err) = self
            .ensure_chat_memory_asset(department, &agent.team_id, &agent.agent_id)
            .await
        {
            warn!(error = %err, agent = %agent.agent_id, "could not register chat-memory asset");
        }
        Ok(agent)
    }

    pub async fn get_agent(
        &self,
        department: &DepartmentId,
        agent_id: &str,
    ) -> DomainResult<Agent> {
        self.store
            .get_agent(department, agent_id)
            .await?
            .ok_or_else(|| DomainError::not_found("agent", agent_id))
    }

    async fn owned_agent(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        agent_id: &str,
    ) -> DomainResult<Agent> {
        let agent = self.get_agent(department, agent_id).await?;
        if agent.owner_user_id != caller.user_id && !caller.is_system_admin {
            return Err(denied("only the agent owner can do this"));
        }
        Ok(agent)
    }

    pub async fn update_agent(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: UpdateAgentInput,
    ) -> DomainResult<Agent> {
        let mut agent = self
            .owned_agent(department, caller, &input.agent_id)
            .await?;
        if let Some(name) = input.name {
            require_non_empty("name", &name)?;
            agent.name = name;
        }
        if input.description.is_some() {
            agent.description = input.description;
        }
        if input.prompt.is_some() {
            agent.prompt = input.prompt;
        }
        if let Some(v) = input.visibility {
            agent.visibility = Visibility::parse(&v)?;
        }
        if let Some(s) = input.status {
            agent.status = AgentStatus::parse(&s)?;
        }
        if let Some(m) = input.metadata_json {
            agent.metadata_json = m;
        }
        agent.updated_at = self.now();
        self.store.update_agent(department, &agent).await?;
        Ok(agent)
    }

    pub async fn delete_agents(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let mut allowed = Vec::new();
        let mut failed = Vec::new();
        for id in ids {
            match self.owned_agent(department, caller, id).await {
                Ok(_) => allowed.push(id.clone()),
                Err(e) => failed.push(FailedId {
                    id: id.clone(),
                    reason: e.code,
                }),
            }
        }
        let mut result = self.store.delete_agents(department, &allowed).await?;
        result.failed.extend(failed);
        Ok(result)
    }

    /// Deactivates the agent, wipes its chat-memory content, and removes the
    /// chat-memory asset (with its bindings and ACL rows).
    pub async fn archive_agent(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        agent_id: &str,
    ) -> DomainResult<Agent> {
        let mut agent = self.owned_agent(department, caller, agent_id).await?;
        agent.status = AgentStatus::Inactive;
        agent.updated_at = self.now();
        self.store.update_agent(department, &agent).await?;
        let memory_id = chat_memory_asset_id(&agent.team_id, &agent.agent_id);
        if let Some(cleaner) = self.chat_memory_cleaner() {
            cleaner
                .clear_chat_memory(department, &agent.team_id, &agent.agent_id)
                .await?;
        }
        self.store.delete_assets(department, &[memory_id]).await?;
        Ok(agent)
    }

    pub async fn list_agents(
        &self,
        department: &DepartmentId,
        input: ListAgentsInput,
    ) -> DomainResult<MetaPage<Agent>> {
        if input.team_id.is_none() && input.owner_user_id.is_none() {
            return Err(DomainError::invalid("team_id or owner_user_id is required"));
        }
        let page = input.pagination.page();
        let filter = AgentFilter {
            team_id: input.team_id,
            owner_user_id: input.owner_user_id,
            status: input
                .status
                .as_deref()
                .map(AgentStatus::parse)
                .transpose()?,
            name: input.name,
        };
        let (agents, total) = self.store.list_agents(department, &filter, page).await?;
        Ok(page_of(agents, total, page))
    }

    pub async fn create_task(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: CreateTaskInput,
    ) -> DomainResult<Task> {
        require_non_empty("title", &input.title)?;
        if input.creator_user_id != caller.user_id {
            return Err(denied("creator must be the caller"));
        }
        self.require_member(department, &input.team_id, caller)
            .await?;
        let now = self.now();
        let task = Task {
            task_id: self.entity_id("task"),
            team_id: input.team_id,
            creator_user_id: input.creator_user_id,
            title: input.title,
            description: input.description,
            source_type: input.source_type.unwrap_or_else(|| "manual".into()),
            source_url: input.source_url,
            status: input
                .status
                .as_deref()
                .map(TaskStatus::parse)
                .transpose()?
                .unwrap_or(TaskStatus::Running),
            auto_assign_floating_assets: input.auto_assign_floating_assets.unwrap_or(false),
            risk_level: input.risk_level,
            created_at: now.clone(),
            updated_at: now.clone(),
            metadata_json: input.metadata_json.unwrap_or_else(|| json!({})),
        };
        let mut links = Vec::new();
        for linked in input.linked_agents {
            let agent = self.get_agent(department, &linked.agent_id).await?;
            if agent.team_id != task.team_id {
                return Err(DomainError::permission_denied("agent_team_mismatch"));
            }
            links.push(TaskAgent {
                id: self.entity_id("tka"),
                task_id: task.task_id.clone(),
                agent_id: linked.agent_id,
                role_in_task: linked.role_in_task,
                status: "active".into(),
                created_at: now.clone(),
            });
        }
        self.store.insert_task(department, &task, &links).await?;
        Ok(task)
    }

    pub async fn get_task(&self, department: &DepartmentId, task_id: &str) -> DomainResult<Task> {
        self.store
            .get_task(department, task_id)
            .await?
            .ok_or_else(|| DomainError::not_found("task", task_id))
    }

    async fn created_task(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        task_id: &str,
    ) -> DomainResult<Task> {
        let task = self.get_task(department, task_id).await?;
        if task.creator_user_id != caller.user_id && !caller.is_system_admin {
            return Err(denied("only the task creator can do this"));
        }
        Ok(task)
    }

    pub async fn update_task(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: UpdateTaskInput,
    ) -> DomainResult<Task> {
        let mut task = self
            .created_task(department, caller, &input.task_id)
            .await?;
        if let Some(t) = input.title {
            require_non_empty("title", &t)?;
            task.title = t;
        }
        if input.description.is_some() {
            task.description = input.description;
        }
        if let Some(s) = input.status {
            task.status = TaskStatus::parse(&s)?;
        }
        if input.source_url.is_some() {
            task.source_url = input.source_url;
        }
        if input.risk_level.is_some() {
            task.risk_level = input.risk_level;
        }
        if let Some(m) = input.metadata_json {
            task.metadata_json = m;
        }
        task.updated_at = self.now();
        self.store.update_task(department, &task).await?;
        Ok(task)
    }

    pub async fn archive_task(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        task_id: &str,
    ) -> DomainResult<Task> {
        let mut task = self.created_task(department, caller, task_id).await?;
        task.status = TaskStatus::Completed;
        task.updated_at = self.now();
        self.store.update_task(department, &task).await?;
        Ok(task)
    }

    pub async fn delete_tasks(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let mut allowed = Vec::new();
        let mut failed = Vec::new();
        for id in ids {
            match self.created_task(department, caller, id).await {
                Ok(_) => allowed.push(id.clone()),
                Err(e) => failed.push(FailedId {
                    id: id.clone(),
                    reason: e.code,
                }),
            }
        }
        let mut result = self.store.delete_tasks(department, &allowed).await?;
        result.failed.extend(failed);
        Ok(result)
    }

    pub async fn list_tasks(
        &self,
        department: &DepartmentId,
        input: ListTasksInput,
    ) -> DomainResult<MetaPage<Task>> {
        if input.team_id.is_none() && input.creator_user_id.is_none() {
            return Err(DomainError::invalid(
                "team_id or creator_user_id is required",
            ));
        }
        let page = input.pagination.page();
        let filter = TaskFilter {
            team_id: input.team_id,
            creator_user_id: input.creator_user_id,
            status: input.status.as_deref().map(TaskStatus::parse).transpose()?,
        };
        let (tasks, total) = self.store.list_tasks(department, &filter, page).await?;
        Ok(page_of(tasks, total, page))
    }

    pub async fn link_task_agent(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        task_id: &str,
        agent_id: &str,
        role: Option<String>,
    ) -> DomainResult<TaskAgent> {
        let task = self.created_task(department, caller, task_id).await?;
        let agent = self.get_agent(department, agent_id).await?;
        if agent.team_id != task.team_id {
            return Err(DomainError::permission_denied("agent_team_mismatch"));
        }
        let link = TaskAgent {
            id: self.entity_id("tka"),
            task_id: task_id.into(),
            agent_id: agent_id.into(),
            role_in_task: role,
            status: "active".into(),
            created_at: self.now(),
        };
        self.store.link_agent(department, &link).await?;
        Ok(link)
    }

    pub async fn unlink_task_agent(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        task_id: &str,
        agent_id: &str,
    ) -> DomainResult<bool> {
        self.created_task(department, caller, task_id).await?;
        if !self
            .store
            .unlink_agent(department, task_id, agent_id)
            .await?
        {
            return Err(DomainError::not_found("task_agent", agent_id));
        }
        Ok(true)
    }

    pub async fn list_task_agents(
        &self,
        department: &DepartmentId,
        task_id: &str,
        pagination: Pagination,
    ) -> DomainResult<MetaPage<TaskAgent>> {
        let page = pagination.page();
        let (links, total) = self
            .store
            .list_task_agents(department, task_id, page)
            .await?;
        Ok(page_of(links, total, page))
    }

    pub async fn append_participation(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: AppendParticipationInput,
    ) -> DomainResult<ParticipationLog> {
        let member = self
            .require_member(department, &input.team_id, caller)
            .await?;
        if input.user_id != caller.user_id && !member.is_admin() {
            return Err(denied("can only log your own participation"));
        }
        let now = self.now();
        let log = ParticipationLog {
            id: self.entity_id("plog"),
            team_id: input.team_id,
            task_id: input.task_id,
            agent_id: input.agent_id,
            user_id: input.user_id,
            source: input.source.unwrap_or_else(|| "unknown".into()),
            created_at: input.created_at.unwrap_or(now),
            metadata_json: input.metadata_json.unwrap_or_else(|| json!({})),
        };
        self.store.append_participation(department, &log).await?;
        Ok(log)
    }

    pub async fn list_participation(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: ListParticipationInput,
    ) -> DomainResult<MetaPage<ParticipationLog>> {
        self.require_member(department, &input.team_id, caller)
            .await?;
        let page = input.pagination.page();
        let filter = ParticipationFilter {
            team_id: input.team_id,
            task_id: input.task_id,
            agent_id: input.agent_id,
            user_id: input.user_id,
            created_after: input.created_after,
            created_before: input.created_before,
        };
        let (mut logs, total) = self
            .store
            .list_participation(department, &filter, page)
            .await?;
        if input.dedupe {
            let mut seen = std::collections::HashSet::new();
            logs.retain(|l| {
                seen.insert((l.task_id.clone(), l.agent_id.clone(), l.user_id.clone()))
            });
        }
        Ok(page_of(logs, total, page))
    }
}
