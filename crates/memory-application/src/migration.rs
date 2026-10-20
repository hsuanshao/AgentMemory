//! Upgrade helper: data written before the data plane required a real team
//! and agent sits in the `default` buckets, which no client can reach any
//! more. This moves it to a team/agent/user the admin picks.

use std::sync::Arc;

use memory_domain::error::{DomainError, DomainResult};
use memory_domain::profile::ProfileScope;
use memory_domain::scope::{DepartmentId, MemoryScope, SegmentId};
use serde::{Deserialize, Serialize};

use crate::ports::memory::{MovedRows, ProfileFileRepository, ScopeMigrationStore};
use crate::ports::meta::MetaStore;
use crate::ports::pipeline::PipelineStateRepository;

/// Upper bound on scheduling records scanned per department.
const STATE_SCAN_LIMIT: usize = 100_000;

#[derive(Debug, Deserialize)]
pub struct MigrateDefaultInput {
    pub team_id: String,
    pub agent_id: String,
    pub user_id: String,
    /// Source bucket; each part defaults to `default` (upstream behaviour).
    /// Lets pre-0.1.1 data written under named agents/users move too.
    #[serde(default)]
    pub from_team_id: Option<String>,
    #[serde(default)]
    pub from_agent_id: Option<String>,
    #[serde(default)]
    pub from_user_id: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct MigrationReport {
    pub dry_run: bool,
    pub to: MemoryScope,
    pub moved: MovedRows,
    /// Pipeline cursors re-homed, so L1/L2 do not re-process moved data.
    pub session_states: usize,
    pub profile_states: usize,
}

pub struct LegacyScopeMigration {
    meta: Arc<dyn MetaStore>,
    profiles: Arc<dyn ProfileFileRepository>,
    rows: Arc<dyn ScopeMigrationStore>,
    state: Arc<dyn PipelineStateRepository>,
}

impl LegacyScopeMigration {
    pub fn new(
        meta: Arc<dyn MetaStore>,
        profiles: Arc<dyn ProfileFileRepository>,
        rows: Arc<dyn ScopeMigrationStore>,
        state: Arc<dyn PipelineStateRepository>,
    ) -> Self {
        Self {
            meta,
            profiles,
            rows,
            state,
        }
    }

    /// The target must be a scope the data plane would authorize.
    async fn validate(
        &self,
        department: &DepartmentId,
        input: &MigrateDefaultInput,
    ) -> DomainResult<MemoryScope> {
        let to = MemoryScope {
            team_id: SegmentId::parse("team_id", &input.team_id)?,
            agent_id: SegmentId::parse("agent_id", &input.agent_id)?,
            user_id: SegmentId::parse("user_id", &input.user_id)?,
            task_id: None,
        };
        self.meta
            .get_team(department, &input.team_id)
            .await?
            .ok_or_else(|| DomainError::not_found("team", &input.team_id))?;
        let agent = self
            .meta
            .get_agent(department, &input.agent_id)
            .await?
            .ok_or_else(|| DomainError::not_found("agent", &input.agent_id))?;
        if agent.team_id != input.team_id {
            return Err(DomainError::invalid("agent does not belong to the team"));
        }
        self.meta
            .get_member(department, &input.team_id, &input.user_id)
            .await?
            .filter(|m| m.is_active())
            .ok_or_else(|| DomainError::invalid("user is not an active member of the team"))?;
        Ok(to)
    }

    async fn ensure_fresh_target(
        &self,
        department: &DepartmentId,
        to: &MemoryScope,
    ) -> DomainResult<()> {
        if self
            .profiles
            .count(department, Some(&ProfileScope::from(to)), "")
            .await?
            > 0
        {
            return Err(DomainError::conflict(
                "target_has_profile",
                "the target agent already has L2/L3 files; migrate into a fresh agent",
            ));
        }
        Ok(())
    }

    /// Rows still in the legacy buckets. The count does not depend on the
    /// target, so any well-formed scope will do.
    pub async fn pending(&self, department: &DepartmentId) -> DomainResult<MovedRows> {
        let probe = MemoryScope::from_optional(None, None, None, None)?;
        self.rows
            .reassign_default(department, &probe, &probe, true)
            .await
    }

    pub async fn migrate_default(
        &self,
        department: &DepartmentId,
        input: MigrateDefaultInput,
    ) -> DomainResult<MigrationReport> {
        let to = self.validate(department, &input).await?;
        let from = MemoryScope::from_optional(
            input.from_team_id.as_deref(),
            input.from_agent_id.as_deref(),
            input.from_user_id.as_deref(),
            None,
        )?;
        let from_profile = ProfileScope::from(&from);
        let pending = self
            .rows
            .reassign_default(department, &from, &to, true)
            .await?;

        let sessions: Vec<_> = self
            .state
            .list_sessions(department, STATE_SCAN_LIMIT)
            .await?
            .into_iter()
            .filter(|s| {
                s.scope.team_id == from.team_id
                    && s.scope.agent_id == from.agent_id
                    && s.scope.user_id == from.user_id
            })
            .collect();
        let profiles: Vec<_> = self
            .state
            .list_profiles(department, STATE_SCAN_LIMIT)
            .await?
            .into_iter()
            .filter(|p| p.profile == from_profile)
            .collect();
        // Nothing left to move (e.g. a re-run): succeed without touching the
        // target, so the upgrade can be repeated safely.
        if pending == MovedRows::default() && sessions.is_empty() && profiles.is_empty() {
            return Ok(MigrationReport {
                dry_run: input.dry_run,
                to,
                moved: pending,
                session_states: 0,
                profile_states: 0,
            });
        }
        // The target agent must have no L2/L3 yet: scenes are versioned
        // files, and merging two sets of them is not something to do silently.
        self.ensure_fresh_target(department, &to).await?;
        let moved = if input.dry_run {
            pending
        } else {
            self.rows
                .reassign_default(department, &from, &to, false)
                .await?
        };
        let report = MigrationReport {
            dry_run: input.dry_run,
            to: to.clone(),
            moved,
            session_states: sessions.len(),
            profile_states: profiles.len(),
        };
        if input.dry_run {
            return Ok(report);
        }
        // Cursors are storage sequence numbers, which the move keeps, so
        // the copied state resumes exactly where the old one stopped.
        for mut session in sessions {
            session.scope = MemoryScope {
                task_id: session.scope.task_id.clone(),
                ..to.clone()
            };
            self.state.put_session(&session).await?;
        }
        for mut profile in profiles {
            profile.profile = ProfileScope::from(&to);
            self.state.put_profile(&profile).await?;
            self.state.delete_profile(department, &from_profile).await?;
        }
        Ok(report)
    }
}
