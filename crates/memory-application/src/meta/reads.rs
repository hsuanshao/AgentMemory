//! Caller-checked reads: team-scoped data is visible to active members of
//! that team (and system admins) only.

use memory_domain::error::DomainResult;
use memory_domain::meta::{Agent, AgentBinding, Task, TaskAgent, Team};
use memory_domain::scope::DepartmentId;

use super::{
    denied, AgentAssetSummary, Caller, ListAgentsInput, ListTasksInput, MetaPage, MetaService,
    Pagination,
};

impl MetaService {
    pub(crate) async fn require_reader(
        &self,
        department: &DepartmentId,
        team_id: &str,
        caller: &Caller,
    ) -> DomainResult<()> {
        if caller.is_system_admin {
            return Ok(());
        }
        self.require_member(department, team_id, caller)
            .await
            .map(|_| ())
    }

    fn require_self(caller: &Caller, user_id: &str) -> DomainResult<()> {
        if caller.is_system_admin || caller.user_id == user_id {
            Ok(())
        } else {
            Err(denied("can only list your own items outside a team"))
        }
    }

    pub async fn read_team(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        team_id: &str,
    ) -> DomainResult<Team> {
        let team = self.get_team(department, team_id).await?;
        self.require_reader(department, &team.team_id, caller)
            .await?;
        Ok(team)
    }

    pub async fn read_agent(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        agent_id: &str,
    ) -> DomainResult<Agent> {
        let agent = self.get_agent(department, agent_id).await?;
        self.require_reader(department, &agent.team_id, caller)
            .await?;
        Ok(agent)
    }

    pub async fn read_task(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        task_id: &str,
    ) -> DomainResult<Task> {
        let task = self.get_task(department, task_id).await?;
        self.require_reader(department, &task.team_id, caller)
            .await?;
        Ok(task)
    }

    pub async fn read_agents(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: ListAgentsInput,
    ) -> DomainResult<MetaPage<Agent>> {
        match (&input.team_id, &input.owner_user_id) {
            (Some(team), _) => self.require_reader(department, team, caller).await?,
            (None, Some(owner)) => Self::require_self(caller, owner)?,
            (None, None) => {}
        }
        self.list_agents(department, input).await
    }

    pub async fn read_tasks(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: ListTasksInput,
    ) -> DomainResult<MetaPage<Task>> {
        match (&input.team_id, &input.creator_user_id) {
            (Some(team), _) => self.require_reader(department, team, caller).await?,
            (None, Some(creator)) => Self::require_self(caller, creator)?,
            (None, None) => {}
        }
        self.list_tasks(department, input).await
    }

    pub async fn read_task_agents(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        task_id: &str,
        pagination: Pagination,
    ) -> DomainResult<MetaPage<TaskAgent>> {
        self.read_task(department, caller, task_id).await?;
        self.list_task_agents(department, task_id, pagination).await
    }

    pub async fn read_bindings(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        agent_id: &str,
        pagination: Pagination,
    ) -> DomainResult<MetaPage<AgentBinding>> {
        self.read_agent(department, caller, agent_id).await?;
        self.list_bindings(department, agent_id, pagination).await
    }

    pub async fn read_binding_summary(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        agent_ids: &[String],
        asset_id: Option<&str>,
    ) -> DomainResult<MetaPage<AgentAssetSummary>> {
        for id in agent_ids {
            self.read_agent(department, caller, id).await?;
        }
        self.binding_summary(department, agent_ids, asset_id).await
    }
}
