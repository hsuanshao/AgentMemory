//! Who is asking for operational state, and the names to show it with.

use std::collections::{BTreeSet, HashMap};

use memory_domain::error::DomainResult;
use memory_domain::meta::visibility::{ScopeVisibility, TeamView};
use memory_domain::scope::DepartmentId;

use super::MetaService;
use crate::common::Page;
use crate::ports::meta::UserFilter;

/// Names of the teams, agents and users that rows refer to. An id without a
/// name (deleted since) is simply absent.
#[derive(Debug, Default)]
pub struct DisplayNames {
    pub teams: HashMap<String, String>,
    pub agents: HashMap<String, String>,
    pub users: HashMap<String, String>,
}

impl MetaService {
    /// The operator (server API key, no user key) and system admins see the
    /// whole department; anyone else sees what their memberships allow.
    pub async fn visibility(
        &self,
        department: &DepartmentId,
        user_key: Option<&str>,
    ) -> DomainResult<ScopeVisibility> {
        let Some(key) = user_key else {
            return Ok(ScopeVisibility::Department);
        };
        let caller = self.authenticate(department, key).await?;
        if caller.is_system_admin {
            return Ok(ScopeVisibility::Department);
        }
        let memberships = self
            .store
            .list_memberships(department, &caller.user_id)
            .await?;
        Ok(ScopeVisibility::Teams(TeamView::new(
            &caller.user_id,
            &memberships,
        )))
    }

    pub async fn display_names(
        &self,
        department: &DepartmentId,
        teams: BTreeSet<String>,
        agents: BTreeSet<String>,
        users: BTreeSet<String>,
    ) -> DomainResult<DisplayNames> {
        let mut names = DisplayNames::default();
        for id in teams {
            if let Some(team) = self.store.get_team(department, &id).await? {
                names.teams.insert(id, team.name);
            }
        }
        for id in agents {
            if let Some(agent) = self.store.get_agent(department, &id).await? {
                names.agents.insert(id, agent.name);
            }
        }
        if !users.is_empty() {
            let filter = UserFilter {
                user_ids: Some(users.into_iter().collect()),
                ..Default::default()
            };
            let limit = filter.user_ids.as_ref().map_or(0, Vec::len);
            let (found, _) = self
                .store
                .list_users(department, &filter, Page { limit, offset: 0 })
                .await?;
            names
                .users
                .extend(found.into_iter().map(|u| (u.user_id, u.username)));
        }
        Ok(names)
    }
}
