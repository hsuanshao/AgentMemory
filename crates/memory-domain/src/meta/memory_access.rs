//! Data-plane access policy: which memory scope an authenticated user may
//! read or write. Pure function of (request, caller, membership, agent); the
//! application layer loads the inputs.

use super::{Agent, TeamMember, TeamRole};
use crate::error::{DomainError, DomainResult};
use crate::scope::{MemoryScope, SegmentId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryAction {
    Read,
    Write,
}

/// Isolation ids as the client sent them (body first, then headers).
#[derive(Debug, Clone, Default)]
pub struct RequestedScope {
    pub team_id: Option<String>,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub task_id: Option<String>,
}

pub struct ScopeFacts<'a> {
    pub caller_user_id: &'a str,
    pub is_system_admin: bool,
    /// The caller's membership in the requested team, if any.
    pub membership: Option<&'a TeamMember>,
    /// The requested agent, if it exists.
    pub agent: Option<&'a Agent>,
}

/// Whether a team role may perform `action` on memory of the target user.
/// `on_self` is true when the target user is the caller. v1 keeps roles
/// flat: every member reads and writes only their own memory; reaching
/// another user's memory is reserved for system admins.
fn role_allows(_role: TeamRole, _action: MemoryAction, on_self: bool) -> bool {
    on_self
}

/// Evaluation order: team required → agent belongs to team → system admin →
/// active membership → role rule. The returned scope is the only one the
/// handler sees; `user_id` defaults to the caller.
pub fn authorize_memory_scope(
    req: &RequestedScope,
    facts: &ScopeFacts<'_>,
    action: MemoryAction,
) -> DomainResult<MemoryScope> {
    let team_id =
        non_empty(&req.team_id).ok_or_else(|| DomainError::invalid("team_id is required"))?;
    let agent_id =
        non_empty(&req.agent_id).ok_or_else(|| DomainError::invalid("agent_id is required"))?;
    let user_id = non_empty(&req.user_id).unwrap_or(facts.caller_user_id);

    match facts.agent {
        Some(agent) if agent.agent_id == agent_id && agent.team_id == team_id => {}
        _ => {
            return Err(DomainError::permission_denied(
                "agent does not belong to the team",
            ))
        }
    }
    if !facts.is_system_admin {
        let member = facts
            .membership
            .filter(|m| m.is_active() && m.team_id == team_id && m.user_id == facts.caller_user_id)
            .ok_or_else(|| DomainError::permission_denied("not an active member of the team"))?;
        if !role_allows(member.role, action, user_id == facts.caller_user_id) {
            return Err(DomainError::permission_denied(
                "role does not allow this access",
            ));
        }
    }
    Ok(MemoryScope {
        team_id: SegmentId::parse("team_id", team_id)?,
        agent_id: SegmentId::parse("agent_id", agent_id)?,
        user_id: SegmentId::parse("user_id", user_id)?,
        task_id: non_empty(&req.task_id)
            .map(|t| SegmentId::parse("task_id", t))
            .transpose()?,
    })
}

fn non_empty(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::meta::{AgentStatus, MemberStatus, Visibility};
    use serde_json::json;

    fn agent(team: &str) -> Agent {
        Agent {
            agent_id: "agt-1".into(),
            team_id: team.into(),
            owner_user_id: "owner".into(),
            name: "a".into(),
            description: None,
            prompt: None,
            visibility: Visibility::Team,
            status: AgentStatus::Active,
            created_at: String::new(),
            updated_at: String::new(),
            metadata_json: json!({}),
        }
    }

    fn member(role: TeamRole) -> TeamMember {
        TeamMember {
            id: "tm-1".into(),
            team_id: "team-1".into(),
            user_id: "alice".into(),
            role,
            status: MemberStatus::Active,
            joined_at: String::new(),
        }
    }

    fn req(user: Option<&str>) -> RequestedScope {
        RequestedScope {
            team_id: Some("team-1".into()),
            agent_id: Some("agt-1".into()),
            user_id: user.map(Into::into),
            task_id: None,
        }
    }

    fn facts<'a>(m: Option<&'a TeamMember>, a: Option<&'a Agent>) -> ScopeFacts<'a> {
        ScopeFacts {
            caller_user_id: "alice",
            is_system_admin: false,
            membership: m,
            agent: a,
        }
    }

    fn denied(r: DomainResult<MemoryScope>) -> bool {
        matches!(r, Err(e) if e.kind == ErrorKind::PermissionDenied)
    }

    #[test]
    fn team_and_agent_are_required() {
        let a = agent("team-1");
        let f = facts(None, Some(&a));
        let mut r = req(None);
        r.team_id = None;
        assert!(authorize_memory_scope(&r, &f, MemoryAction::Read).is_err());
        let mut r = req(None);
        r.agent_id = Some(" ".into());
        assert!(authorize_memory_scope(&r, &f, MemoryAction::Read).is_err());
    }

    #[test]
    fn non_member_is_denied() {
        let a = agent("team-1");
        assert!(denied(authorize_memory_scope(
            &req(None),
            &facts(None, Some(&a)),
            MemoryAction::Read
        )));
    }

    #[test]
    fn removed_member_is_denied() {
        let (a, mut m) = (agent("team-1"), member(TeamRole::Admin));
        m.status = MemberStatus::Removed;
        assert!(denied(authorize_memory_scope(
            &req(None),
            &facts(Some(&m), Some(&a)),
            MemoryAction::Read
        )));
    }

    #[test]
    fn agent_from_another_team_is_denied_even_for_admin() {
        let a = agent("team-2");
        let mut f = facts(None, Some(&a));
        f.is_system_admin = true;
        assert!(denied(authorize_memory_scope(
            &req(None),
            &f,
            MemoryAction::Read
        )));
        assert!(denied(authorize_memory_scope(
            &req(None),
            &facts(None, None),
            MemoryAction::Read
        )));
    }

    #[test]
    fn member_writes_own_memory_and_user_defaults_to_caller() {
        let (a, m) = (agent("team-1"), member(TeamRole::Member));
        let scope =
            authorize_memory_scope(&req(None), &facts(Some(&m), Some(&a)), MemoryAction::Write)
                .unwrap();
        assert_eq!(scope.key(), "team-1/agt-1/alice");
    }

    #[test]
    fn team_admin_cannot_reach_another_users_memory() {
        let (a, m) = (agent("team-1"), member(TeamRole::Admin));
        assert!(denied(authorize_memory_scope(
            &req(Some("bob")),
            &facts(Some(&m), Some(&a)),
            MemoryAction::Read
        )));
    }

    #[test]
    fn system_admin_may_target_any_user() {
        let a = agent("team-1");
        let mut f = facts(None, Some(&a));
        f.is_system_admin = true;
        let scope = authorize_memory_scope(&req(Some("bob")), &f, MemoryAction::Write).unwrap();
        assert_eq!(scope.user_id.as_str(), "bob");
    }
}
