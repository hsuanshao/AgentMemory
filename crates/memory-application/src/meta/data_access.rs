//! Data-plane authorization: resolves a user key plus the requested
//! isolation ids into the one memory scope the request may touch.

use memory_domain::error::DomainResult;
use memory_domain::meta::memory_access::{
    authorize_memory_scope, authorize_proposal_review, MemoryAction, RequestedScope, ScopeFacts,
};
use memory_domain::scope::{DepartmentId, MemoryScope};

use super::{Caller, MetaService};

impl MetaService {
    pub async fn authorize_memory(
        &self,
        department: &DepartmentId,
        user_key: &str,
        requested: &RequestedScope,
        action: MemoryAction,
    ) -> DomainResult<MemoryScope> {
        let caller = self.authenticate(department, user_key).await?;
        let team_id = requested.team_id.as_deref().unwrap_or_default();
        let membership = match team_id {
            "" => None,
            id => {
                self.active_membership(department, id, &caller.user_id)
                    .await?
            }
        };
        let agent = match requested.agent_id.as_deref().unwrap_or_default() {
            "" => None,
            id => self.store.get_agent(department, id).await?,
        };
        authorize_memory_scope(
            requested,
            &ScopeFacts {
                caller_user_id: &caller.user_id,
                is_system_admin: caller.is_system_admin,
                membership: membership.as_ref(),
                agent: agent.as_ref(),
            },
            action,
        )
    }

    /// Whether `caller` may read and decide skill proposals drawn from
    /// `author`'s conversation through `agent_id` (see
    /// `authorize_proposal_review`), with the agent as it is now.
    pub async fn authorize_proposal_review(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        team_id: &str,
        agent_id: &str,
        author: &str,
    ) -> DomainResult<()> {
        let membership = self
            .active_membership(department, team_id, &caller.user_id)
            .await?;
        let agent = self
            .store
            .get_agent(department, agent_id)
            .await?
            .filter(|a| a.team_id == team_id);
        authorize_proposal_review(
            &ScopeFacts {
                caller_user_id: &caller.user_id,
                is_system_admin: caller.is_system_admin,
                membership: membership.as_ref(),
                agent: agent.as_ref(),
            },
            team_id,
            author,
        )
    }
}
