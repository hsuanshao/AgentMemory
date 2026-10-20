//! One-step upgrade from a pre-0.1.1 install, driven by the operator with the
//! server API key only (no admin user key, which old installs printed once
//! and may have lost): pick the system admin, make sure a team and an agent
//! exist, move the legacy `default` buckets there and, on request, issue a
//! fresh user key for the clients. Safe to repeat: names are looked up
//! before anything is created and an empty legacy bucket is a no-op.

use std::sync::Arc;

use memory_domain::error::{DomainError, DomainResult};
use memory_domain::meta::{UserStatus, UserType};
use memory_domain::scope::DepartmentId;
use serde::{Deserialize, Serialize};

use crate::meta::{
    AddMemberInput, Caller, CreateAgentInput, CreateKeyInput, CreateTeamInput, ListAgentsInput,
    MetaService, Pagination,
};
use crate::migration::{LegacyScopeMigration, MigrateDefaultInput, MigrationReport};
use crate::ports::memory::MovedRows;
use crate::ports::meta::UserFilter;

#[derive(Debug, Deserialize)]
pub struct UpgradeInput {
    /// System admin who owns the result; defaults to the oldest one.
    pub admin_username: Option<String>,
    /// Team that receives the legacy data, created when missing.
    pub team_name: Option<String>,
    /// Agent that receives the legacy data; defaults to the agent the
    /// first-run bootstrap creates (`default-agent-<admin>`).
    pub agent_name: Option<String>,
    /// Also issue a new user key for the admin, returned once.
    #[serde(default)]
    pub issue_key: bool,
    /// Report the plan without creating or moving anything.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct UpgradeReport {
    pub dry_run: bool,
    pub user_id: String,
    pub username: String,
    /// `None` only in a dry run whose team does not exist yet.
    pub team_id: Option<String>,
    pub team_name: String,
    pub team_created: bool,
    pub agent_id: Option<String>,
    pub agent_name: String,
    pub agent_created: bool,
    /// Rows found in (dry run) or moved out of the legacy buckets.
    pub moved: MovedRows,
    pub session_states: usize,
    pub profile_states: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_key: Option<String>,
}

pub struct LegacyUpgrade {
    meta: Arc<MetaService>,
    migration: Arc<LegacyScopeMigration>,
}

impl LegacyUpgrade {
    pub fn new(meta: Arc<MetaService>, migration: Arc<LegacyScopeMigration>) -> Self {
        Self { meta, migration }
    }

    pub async fn run(
        &self,
        department: &DepartmentId,
        input: UpgradeInput,
    ) -> DomainResult<UpgradeReport> {
        let (admin, username) = self
            .admin(department, input.admin_username.as_deref())
            .await?;
        let team_name = input.team_name.unwrap_or_else(|| "default-team".into());
        let agent_name = input
            .agent_name
            .unwrap_or_else(|| format!("default-agent-{username}"));
        let mut report = UpgradeReport {
            dry_run: input.dry_run,
            user_id: admin.user_id.clone(),
            username,
            team_id: self.find_team(department, &team_name).await?,
            team_name,
            team_created: false,
            agent_id: None,
            agent_name,
            agent_created: false,
            moved: MovedRows::default(),
            session_states: 0,
            profile_states: 0,
            user_key: None,
        };
        if let Some(team_id) = &report.team_id {
            report.agent_id = self
                .find_agent(department, team_id, &report.agent_name)
                .await?;
        }
        if input.dry_run && (report.team_id.is_none() || report.agent_id.is_none()) {
            // Nothing exists to validate against yet: just count.
            report.moved = self.migration.pending(department).await?;
            return Ok(report);
        }

        let team_id = match report.team_id.clone() {
            Some(id) => id,
            None => {
                report.team_created = true;
                self.meta
                    .create_team(
                        department,
                        &admin,
                        CreateTeamInput {
                            name: report.team_name.clone(),
                            owner_user_id: admin.user_id.clone(),
                            description: Some("Created by the 0.1.1 upgrade".into()),
                            metadata_json: None,
                        },
                    )
                    .await?
                    .team_id
            }
        };
        // A team found by name may predate the admin's membership.
        if !input.dry_run
            && self
                .meta
                .get_member(department, &admin, &team_id, &admin.user_id)
                .await
                .is_err()
        {
            self.meta
                .add_member(
                    department,
                    &admin,
                    AddMemberInput {
                        team_id: team_id.clone(),
                        user_id: admin.user_id.clone(),
                        role: Some("admin".into()),
                    },
                )
                .await?;
        }
        let agent_id = match report.agent_id.clone() {
            Some(id) => id,
            None => {
                report.agent_created = true;
                self.meta
                    .create_agent(
                        department,
                        &admin,
                        CreateAgentInput {
                            team_id: team_id.clone(),
                            owner_user_id: admin.user_id.clone(),
                            name: report.agent_name.clone(),
                            description: Some(
                                "Holds the memories migrated from the default bucket".into(),
                            ),
                            prompt: Some(String::new()),
                            visibility: Some("team".into()),
                            metadata_json: None,
                        },
                    )
                    .await?
                    .agent_id
            }
        };
        report.team_id = Some(team_id.clone());
        report.agent_id = Some(agent_id.clone());

        let migrated: MigrationReport = self
            .migration
            .migrate_default(
                department,
                MigrateDefaultInput {
                    team_id,
                    agent_id,
                    user_id: admin.user_id.clone(),
                    from_team_id: None,
                    from_agent_id: None,
                    from_user_id: None,
                    dry_run: input.dry_run,
                },
            )
            .await?;
        report.moved = migrated.moved;
        report.session_states = migrated.session_states;
        report.profile_states = migrated.profile_states;

        if input.issue_key && !input.dry_run {
            let key = self
                .meta
                .create_key(
                    department,
                    &admin,
                    CreateKeyInput {
                        user_id: None,
                        name: Some("upgrade".into()),
                        expires_at: None,
                    },
                )
                .await?;
            report.user_key = key.key_value;
        }
        Ok(report)
    }

    async fn admin(
        &self,
        department: &DepartmentId,
        username: Option<&str>,
    ) -> DomainResult<(Caller, String)> {
        let filter = UserFilter {
            user_type: Some(UserType::SystemAdmin),
            status: Some(UserStatus::Active),
            ..Default::default()
        };
        let page = Pagination {
            limit: Some(100),
            offset: None,
        };
        let mut admins = self
            .meta
            .list_users_internal(department, filter, page)
            .await?
            .items;
        admins.retain(|u| username.is_none_or(|n| u.username == n));
        admins.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let admin = admins.into_iter().next().ok_or_else(|| match username {
            Some(n) => DomainError::not_found("system admin", n),
            None => DomainError::invalid("no active system admin; run init-admin first"),
        })?;
        Ok((
            Caller {
                user_id: admin.user_id,
                is_system_admin: true,
            },
            admin.username,
        ))
    }

    /// Store name filters are substring matches; the upgrade wants exact.
    async fn find_team(
        &self,
        department: &DepartmentId,
        name: &str,
    ) -> DomainResult<Option<String>> {
        let page = Pagination {
            limit: Some(100),
            offset: None,
        };
        let teams = self
            .meta
            .list_teams(department, None, Some(name), page)
            .await?
            .items;
        Ok(teams
            .into_iter()
            .find(|t| t.name == name)
            .map(|t| t.team_id))
    }

    async fn find_agent(
        &self,
        department: &DepartmentId,
        team_id: &str,
        name: &str,
    ) -> DomainResult<Option<String>> {
        let agents = self
            .meta
            .list_agents(
                department,
                ListAgentsInput {
                    team_id: Some(team_id.to_owned()),
                    owner_user_id: None,
                    status: None,
                    name: Some(name.to_owned()),
                    pagination: Pagination {
                        limit: Some(100),
                        offset: None,
                    },
                },
            )
            .await?
            .items;
        Ok(agents
            .into_iter()
            .find(|a| a.name == name)
            .map(|a| a.agent_id))
    }
}
