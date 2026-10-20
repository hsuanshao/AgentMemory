//! Custom L1/L2/L3 prompts: CRUD, apply/clear settings, audit log and the
//! agent → team → department resolution used by the pipeline.

use std::sync::Arc;

use chrono::Duration;
use memory_domain::error::{DomainError, DomainResult, ErrorKind};
use memory_domain::memory::Layer;
use memory_domain::scope::DepartmentId;

use super::prompts::CustomStrategy;
use crate::common::Page;
use crate::ports::pipeline::{
    MemoryPrompt, MemoryPromptRepository, PromptLogEntry, PromptSetting, PromptSettingFilter,
    PromptTarget,
};
use crate::ports::system::{Clock, IdGenerator};

pub const MAX_PROMPTS_PER_DEPARTMENT: usize = 500;
const MAX_NAME_CHARS: usize = 100;
const MAX_PROMPT_CHARS: usize = 10_000;
const MAX_LOG_RANGE_DAYS: i64 = 90;
const DEFAULT_LOG_RANGE_DAYS: i64 = 7;

fn not_found() -> DomainError {
    DomainError::new(
        ErrorKind::NotFound,
        "MEMORY_PROMPT_NOT_FOUND",
        "memory prompt not found",
    )
}

pub enum PromptAction {
    Apply { memory_prompt_id: String },
    Clear,
}

pub struct ApplyTarget {
    pub team_id: Option<String>,
    pub agent_ids: Vec<String>,
}

pub struct MemoryPromptService {
    repo: Arc<dyn MemoryPromptRepository>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl MemoryPromptService {
    pub fn new(
        repo: Arc<dyn MemoryPromptRepository>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { repo, clock, ids }
    }

    fn now_ms(&self) -> i64 {
        self.clock.now().timestamp_millis()
    }

    pub async fn create(
        &self,
        department: &DepartmentId,
        name: &str,
        layer: Layer,
        prompt: &str,
    ) -> DomainResult<MemoryPrompt> {
        validate_text(name, prompt)?;
        if self.repo.count(department).await? >= MAX_PROMPTS_PER_DEPARTMENT {
            return Err(DomainError::conflict(
                "PROMPT_LIMIT_EXCEEDED",
                "at most 500 prompts per department",
            ));
        }
        let now = self.now_ms();
        let record = MemoryPrompt {
            memory_prompt_id: self.ids.next("mp-"),
            name: name.to_owned(),
            layer,
            prompt: prompt.to_owned(),
            version: 1,
            status: "active".into(),
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.repo.insert(department, &record).await?;
        Ok(record)
    }

    pub async fn get(&self, department: &DepartmentId, id: &str) -> DomainResult<MemoryPrompt> {
        self.repo
            .get(department, id)
            .await?
            .filter(|p| p.status == "active")
            .ok_or_else(not_found)
    }

    pub async fn list(
        &self,
        department: &DepartmentId,
        layer: Option<Layer>,
        page: Page,
        newest_first: bool,
    ) -> DomainResult<Vec<MemoryPrompt>> {
        self.repo.list(department, layer, page, newest_first).await
    }

    pub async fn update(
        &self,
        department: &DepartmentId,
        id: &str,
        name: Option<&str>,
        prompt: Option<&str>,
    ) -> DomainResult<MemoryPrompt> {
        if name.is_none() && prompt.is_none() {
            return Err(DomainError::invalid("name or prompt is required"));
        }
        let mut record = self.get(department, id).await?;
        if let Some(n) = name {
            record.name = n.to_owned();
        }
        if let Some(p) = prompt {
            record.prompt = p.to_owned();
        }
        validate_text(&record.name, &record.prompt)?;
        record.version += 1;
        record.updated_at_ms = self.now_ms();
        self.repo.update(department, &record).await?;
        Ok(record)
    }

    pub async fn delete(&self, department: &DepartmentId, ids: &[String]) -> DomainResult<usize> {
        for id in ids {
            self.get(department, id).await?;
        }
        self.repo.delete(department, ids).await
    }

    /// Applies or clears the effective prompt for the department, a team, or
    /// specific agents of a team. Returns the number of targets affected.
    pub async fn set(
        &self,
        department: &DepartmentId,
        layer: Layer,
        action: PromptAction,
        target: ApplyTarget,
    ) -> DomainResult<usize> {
        if !target.agent_ids.is_empty() && target.team_id.is_none() {
            return Err(DomainError::invalid("agent_ids requires team_id"));
        }
        let targets = expand_targets(&target);
        let now = self.now_ms();
        let prompt_id = match &action {
            PromptAction::Apply { memory_prompt_id } => {
                let prompt = self.get(department, memory_prompt_id).await?;
                if prompt.layer != layer {
                    return Err(DomainError::invalid(
                        "PROMPT_LAYER_MISMATCH: prompt layer does not match",
                    ));
                }
                Some(memory_prompt_id.clone())
            }
            PromptAction::Clear => None,
        };
        let mut affected = 0;
        for (kind, team, agent) in targets {
            match &prompt_id {
                Some(id) => {
                    let setting = PromptSetting {
                        target_type: kind,
                        team_id: team.clone(),
                        agent_id: agent.clone(),
                        layer,
                        memory_prompt_id: id.clone(),
                        updated_at_ms: now,
                    };
                    self.repo.put_setting(department, &setting).await?;
                    affected += 1;
                }
                None => {
                    affected += self
                        .repo
                        .clear_setting(department, kind, team.as_deref(), agent.as_deref(), layer)
                        .await?
                }
            }
            let entry = PromptLogEntry {
                log_id: self.ids.next("mpl-"),
                action: if prompt_id.is_some() {
                    "apply"
                } else {
                    "clear"
                }
                .into(),
                memory_prompt_id: prompt_id.clone(),
                layer,
                target_type: kind,
                team_id: team,
                agent_id: agent,
                created_at_ms: now,
            };
            self.repo.append_log(department, &entry).await?;
        }
        Ok(affected)
    }

    pub async fn list_settings(
        &self,
        department: &DepartmentId,
        filter: &PromptSettingFilter,
        page: Page,
    ) -> DomainResult<Vec<PromptSetting>> {
        self.repo.list_settings(department, filter, page).await
    }

    pub async fn logs(
        &self,
        department: &DepartmentId,
        filter: &PromptSettingFilter,
        action: Option<&str>,
        range_ms: Option<(i64, i64)>,
        page: Page,
    ) -> DomainResult<Vec<PromptLogEntry>> {
        let (from, to) = match range_ms {
            Some((from, to)) => {
                if to < from || to - from > Duration::days(MAX_LOG_RANGE_DAYS).num_milliseconds() {
                    return Err(DomainError::invalid(
                        "time range must be ordered and at most 90 days",
                    ));
                }
                (from, to)
            }
            None => {
                let now = self.now_ms();
                (
                    now - Duration::days(DEFAULT_LOG_RANGE_DAYS).num_milliseconds(),
                    now,
                )
            }
        };
        self.repo
            .list_logs(department, filter, action, from, to, page)
            .await
    }

    /// The effective custom strategy: agent → team → department; `None`
    /// means "use the built-in prompt unchanged".
    pub async fn resolve(
        &self,
        department: &DepartmentId,
        team_id: &str,
        agent_id: &str,
        layer: Layer,
    ) -> DomainResult<Option<CustomStrategy>> {
        let candidates = [
            (PromptTarget::Agent, Some(team_id), Some(agent_id)),
            (PromptTarget::Team, Some(team_id), None),
            (PromptTarget::Department, None, None),
        ];
        for (kind, team, agent) in candidates {
            let filter = PromptSettingFilter {
                target_type: Some(kind),
                team_id: team.map(str::to_owned),
                agent_id: agent.map(str::to_owned),
                layer: Some(layer),
                memory_prompt_id: None,
            };
            let settings = self
                .repo
                .list_settings(
                    department,
                    &filter,
                    Page {
                        limit: 1,
                        offset: 0,
                    },
                )
                .await?;
            let Some(setting) = settings.into_iter().next() else {
                continue;
            };
            if let Some(p) = self
                .repo
                .get(department, &setting.memory_prompt_id)
                .await?
                .filter(|p| p.status == "active")
            {
                return Ok(Some(CustomStrategy {
                    memory_prompt_id: p.memory_prompt_id,
                    version: p.version,
                    source: format!("{kind:?}").to_lowercase(),
                    prompt: p.prompt,
                }));
            }
        }
        Ok(None)
    }
}

fn validate_text(name: &str, prompt: &str) -> DomainResult<()> {
    if name.trim().is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(DomainError::invalid("name must be 1-100 characters"));
    }
    if prompt.trim().is_empty() || prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(DomainError::invalid("prompt must be 1-10000 characters"));
    }
    Ok(())
}

type Target = (PromptTarget, Option<String>, Option<String>);

fn expand_targets(target: &ApplyTarget) -> Vec<Target> {
    match (&target.team_id, target.agent_ids.is_empty()) {
        (None, _) => vec![(PromptTarget::Department, None, None)],
        (Some(team), true) => vec![(PromptTarget::Team, Some(team.clone()), None)],
        (Some(team), false) => target
            .agent_ids
            .iter()
            .map(|a| (PromptTarget::Agent, Some(team.clone()), Some(a.clone())))
            .collect(),
    }
}
