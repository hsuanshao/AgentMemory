//! Data-plane authorization: resolves a user key plus the requested
//! isolation ids into the one memory scope the request may touch.

use memory_domain::error::DomainResult;
use memory_domain::meta::memory_access::{
    authorize_memory_scope, MemoryAction, RequestedScope, ScopeFacts,
};
use memory_domain::scope::{InstanceId, MemoryScope};

use super::MetaService;

impl MetaService {
    pub async fn authorize_memory(
        &self,
        instance: &InstanceId,
        user_key: &str,
        requested: &RequestedScope,
        action: MemoryAction,
    ) -> DomainResult<MemoryScope> {
        let caller = self.authenticate(instance, user_key).await?;
        let team_id = requested.team_id.as_deref().unwrap_or_default();
        let membership = match team_id {
            "" => None,
            id => {
                self.active_membership(instance, id, &caller.user_id)
                    .await?
            }
        };
        let agent = match requested.agent_id.as_deref().unwrap_or_default() {
            "" => None,
            id => self.store.get_agent(instance, id).await?,
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
}
