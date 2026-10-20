//! Which part of a department's operational state a caller may see: the
//! pipeline state of each scope, generation logs and dead letters.
//!
//! Operational state names a team, and for per-user work (L0/L1) a user.
//! It never carries memory content, which stays behind the data-plane rules
//! in `memory_access`. The rules:
//!
//! * the operator (server API key alone) and system admins see the whole
//!   department;
//! * a team **admin** or **leader** sees everything of their team;
//! * any other member sees the team's shared work (L2/L3, `user` absent)
//!   and their own per-user work.

use std::collections::BTreeSet;

use super::{TeamMember, TeamRole};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeVisibility {
    Department,
    Teams(TeamView),
}

/// A caller who is neither the operator nor a system admin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamView {
    pub user_id: String,
    /// Teams the caller oversees (role admin or leader).
    pub overseen: BTreeSet<String>,
    /// Every team the caller is an active member of, overseen ones included.
    pub member_of: BTreeSet<String>,
}

impl TeamView {
    /// Built from the caller's memberships; inactive ones are ignored.
    pub fn new(user_id: &str, memberships: &[TeamMember]) -> Self {
        let active = || memberships.iter().filter(|m| m.is_active());
        Self {
            user_id: user_id.to_owned(),
            overseen: active()
                .filter(|m| matches!(m.role, TeamRole::Admin | TeamRole::Leader))
                .map(|m| m.team_id.clone())
                .collect(),
            member_of: active().map(|m| m.team_id.clone()).collect(),
        }
    }

    pub fn sees(&self, team_id: &str, user_id: Option<&str>) -> bool {
        self.overseen.contains(team_id) || self.can_read_memory(team_id, user_id)
    }

    /// Whether the data plane lets the caller open that scope's memory:
    /// shared layers of their teams, and their own per-user layers.
    pub fn can_read_memory(&self, team_id: &str, user_id: Option<&str>) -> bool {
        self.member_of.contains(team_id) && user_id.is_none_or(|u| u == self.user_id)
    }
}

impl ScopeVisibility {
    /// Operational state of `team_id`, per-user when `user_id` is given.
    pub fn sees(&self, team_id: &str, user_id: Option<&str>) -> bool {
        match self {
            Self::Department => true,
            Self::Teams(view) => view.sees(team_id, user_id),
        }
    }

    pub fn can_read_memory(&self, team_id: &str, user_id: Option<&str>) -> bool {
        match self {
            Self::Department => true,
            Self::Teams(view) => view.can_read_memory(team_id, user_id),
        }
    }

    pub fn teams(&self) -> Option<&TeamView> {
        match self {
            Self::Department => None,
            Self::Teams(view) => Some(view),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::MemberStatus;

    fn member(team: &str, role: TeamRole, status: MemberStatus) -> TeamMember {
        TeamMember {
            id: format!("m-{team}"),
            team_id: team.into(),
            user_id: "u-me".into(),
            role,
            status,
            joined_at: String::new(),
        }
    }

    fn view() -> ScopeVisibility {
        ScopeVisibility::Teams(TeamView::new(
            "u-me",
            &[
                member("t-lead", TeamRole::Leader, MemberStatus::Active),
                member("t-admin", TeamRole::Admin, MemberStatus::Active),
                member("t-member", TeamRole::Member, MemberStatus::Active),
                member("t-left", TeamRole::Leader, MemberStatus::Removed),
            ],
        ))
    }

    #[test]
    fn leaders_and_admins_see_their_whole_team_but_cannot_read_others_memory() {
        let v = view();
        for team in ["t-lead", "t-admin"] {
            assert!(v.sees(team, Some("u-other")));
            assert!(v.sees(team, None));
            assert!(!v.can_read_memory(team, Some("u-other")));
            assert!(v.can_read_memory(team, None));
        }
    }

    #[test]
    fn members_see_shared_work_and_their_own() {
        let v = view();
        assert!(v.sees("t-member", None));
        assert!(v.sees("t-member", Some("u-me")));
        assert!(!v.sees("t-member", Some("u-other")));
        assert!(v.can_read_memory("t-member", Some("u-me")));
    }

    #[test]
    fn other_and_former_teams_are_hidden() {
        let v = view();
        for team in ["t-left", "t-elsewhere"] {
            assert!(!v.sees(team, None));
            assert!(!v.sees(team, Some("u-me")));
        }
    }

    #[test]
    fn the_department_view_sees_everything() {
        assert!(ScopeVisibility::Department.sees("any", Some("anyone")));
        assert!(ScopeVisibility::Department.can_read_memory("any", Some("anyone")));
    }
}
