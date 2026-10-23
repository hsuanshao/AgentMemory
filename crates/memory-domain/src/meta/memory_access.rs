//! Data-plane access policy: which memory scope an authenticated user may
//! read or write. Pure function of (request, caller, membership, agent); the
//! application layer loads the inputs.

use super::{Agent, AgentStatus, TeamMember, TeamRole, Visibility};
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
/// active membership → active agent → private agent → role rule.
///
/// A private agent (`visibility: private`) is its owner's: its shared
/// layers (scenes, persona, skills) are distilled from the owner's facts,
/// so only the owner and the team's admins may use it. The returned scope is the only one the
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
        let agent = facts.agent.expect("checked above");
        if agent.status != AgentStatus::Active {
            return Err(DomainError::permission_denied("agent is archived"));
        }
        if agent.visibility == Visibility::Private
            && agent.owner_user_id != facts.caller_user_id
            && !member.is_admin()
        {
            return Err(DomainError::permission_denied(
                "private agent: owner and team admins only",
            ));
        }
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

/// Who may read and decide what skill extraction proposed from a
/// conversation (`author_user_id`'s, through `facts.agent`). The proposals
/// are drawn from that conversation, so this follows the memory rules, not
/// the operational view: a system admin; an admin of the team; or the
/// conversation's own user, on an agent they may use (a private agent is
/// its owner's). Team leaders oversee operational state but do not read
/// members' memory, so they are not reviewers. Nobody reviews for an
/// archived agent.
pub fn authorize_proposal_review(
    facts: &ScopeFacts<'_>,
    team_id: &str,
    author_user_id: &str,
) -> DomainResult<()> {
    let agent = facts
        .agent
        .filter(|a| a.team_id == team_id)
        .ok_or_else(|| DomainError::permission_denied("agent does not belong to the team"))?;
    if agent.status != AgentStatus::Active {
        return Err(DomainError::permission_denied("agent is archived"));
    }
    if facts.is_system_admin {
        return Ok(());
    }
    let member = facts
        .membership
        .filter(|m| m.is_active() && m.team_id == team_id && m.user_id == facts.caller_user_id)
        .ok_or_else(|| DomainError::permission_denied("not an active member of the team"))?;
    if member.is_admin() {
        return Ok(());
    }
    if author_user_id != facts.caller_user_id {
        return Err(DomainError::permission_denied(
            "skill proposals: the conversation's user, team admins or system admins only",
        ));
    }
    if agent.visibility == Visibility::Private && agent.owner_user_id != facts.caller_user_id {
        return Err(DomainError::permission_denied(
            "private agent: owner and team admins only",
        ));
    }
    Ok(())
}

fn non_empty(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::meta::MemberStatus;
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

    fn private(owner: &str) -> Agent {
        Agent {
            owner_user_id: owner.into(),
            visibility: Visibility::Private,
            ..agent("team-1")
        }
    }

    #[test]
    fn a_private_agent_is_its_owners_and_team_admins() {
        for action in [MemoryAction::Read, MemoryAction::Write] {
            // Another member: denied.
            let (a, m) = (private("bob"), member(TeamRole::Member));
            assert!(denied(authorize_memory_scope(
                &req(None),
                &facts(Some(&m), Some(&a)),
                action
            )));
            let leader = member(TeamRole::Leader);
            assert!(denied(authorize_memory_scope(
                &req(None),
                &facts(Some(&leader), Some(&a)),
                action
            )));
            // The owner, a team admin and a system admin: allowed.
            let own = private("alice");
            assert!(
                authorize_memory_scope(&req(None), &facts(Some(&m), Some(&own)), action).is_ok()
            );
            let admin = member(TeamRole::Admin);
            assert!(
                authorize_memory_scope(&req(None), &facts(Some(&admin), Some(&a)), action).is_ok()
            );
            let mut sys = facts(None, Some(&a));
            sys.is_system_admin = true;
            assert!(authorize_memory_scope(&req(None), &sys, action).is_ok());
        }
        // A team agent stays open to every member.
        let (a, m) = (agent("team-1"), member(TeamRole::Member));
        assert!(authorize_memory_scope(
            &req(None),
            &facts(Some(&m), Some(&a)),
            MemoryAction::Write
        )
        .is_ok());
    }

    #[test]
    fn an_archived_agent_is_refused() {
        let a = Agent {
            status: AgentStatus::Inactive,
            ..agent("team-1")
        };
        for role in [TeamRole::Member, TeamRole::Admin] {
            let m = member(role);
            assert!(denied(authorize_memory_scope(
                &req(None),
                &facts(Some(&m), Some(&a)),
                MemoryAction::Read
            )));
        }
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

    #[test]
    fn skill_proposals_are_reviewed_like_memory_not_like_operational_state() {
        let ok = |r: DomainResult<()>| r.is_ok();
        let refused =
            |r: DomainResult<()>| matches!(r, Err(e) if e.kind == ErrorKind::PermissionDenied);
        let team_agent = agent("team-1");
        let private = Agent {
            visibility: Visibility::Private,
            ..agent("team-1")
        };
        let (m, l, a) = (
            member(TeamRole::Member),
            member(TeamRole::Leader),
            member(TeamRole::Admin),
        );
        // The conversation's own user, on a team agent.
        assert!(ok(authorize_proposal_review(
            &facts(Some(&m), Some(&team_agent)),
            "team-1",
            "alice"
        )));
        // Another member, and a leader, are not reviewers.
        assert!(refused(authorize_proposal_review(
            &facts(Some(&m), Some(&team_agent)),
            "team-1",
            "bob"
        )));
        assert!(refused(authorize_proposal_review(
            &facts(Some(&l), Some(&team_agent)),
            "team-1",
            "bob"
        )));
        assert!(refused(authorize_proposal_review(
            &facts(Some(&l), Some(&private)),
            "team-1",
            "owner"
        )));
        // A private agent: its owner and team admins.
        assert!(refused(authorize_proposal_review(
            &facts(Some(&m), Some(&private)),
            "team-1",
            "alice"
        )));
        assert!(ok(authorize_proposal_review(
            &facts(Some(&a), Some(&private)),
            "team-1",
            "owner"
        )));
        // Archived: nobody, not even a system admin.
        let archived = Agent {
            status: AgentStatus::Inactive,
            ..agent("team-1")
        };
        let mut sys = facts(None, Some(&archived));
        sys.is_system_admin = true;
        assert!(refused(authorize_proposal_review(&sys, "team-1", "alice")));
        sys.agent = Some(&private);
        assert!(ok(authorize_proposal_review(&sys, "team-1", "owner")));
        // An agent of another team.
        assert!(refused(authorize_proposal_review(
            &facts(Some(&a), Some(&agent("team-2"))),
            "team-1",
            "alice"
        )));
    }
}
