//! Asset access policy. Pure function of (asset, caller, membership, ACL);
//! the application layer loads the inputs.

use super::{
    AclEntry, Asset, AssetStatus, Effect, Permission, SubjectType, TeamMember, TeamRole, Visibility,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow(String),
    Deny(String),
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow(_))
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Allow(r) | Self::Deny(r) => r,
        }
    }
}

pub struct AccessRequest<'a> {
    pub asset: Option<&'a Asset>,
    pub user_id: &'a str,
    pub agent_id: Option<&'a str>,
    /// The caller's membership in `asset.team_id`, if any.
    pub membership: Option<&'a TeamMember>,
    pub action: Permission,
    pub acl: &'a [AclEntry],
}

/// Permissions a team role holds on non-private assets without any ACL.
pub fn role_defaults(role: TeamRole) -> &'static [Permission] {
    match role {
        TeamRole::Admin => &[
            Permission::Read,
            Permission::Write,
            Permission::Assign,
            Permission::Share,
        ],
        TeamRole::Leader | TeamRole::Member => &[Permission::Read],
    }
}

/// Evaluation order: availability → owner → membership → visibility →
/// role defaults → ACL.
pub fn check_asset_permission(req: &AccessRequest<'_>) -> Decision {
    let Some(asset) = req.asset.filter(|a| a.status != AssetStatus::Archived) else {
        return Decision::Deny("asset_not_available".into());
    };
    if asset.owner_user_id == req.user_id {
        return Decision::Allow("owner".into());
    }
    let Some(member) = req
        .membership
        .filter(|m| m.is_active() && m.team_id == asset.team_id)
    else {
        return Decision::Deny("not_team_member".into());
    };

    match asset.visibility {
        // Strictly the owner's, team admins included.
        Visibility::Private => return Decision::Deny("private".into()),
        Visibility::Restricted if member.role != TeamRole::Admin => {
            return acl_decision(req, member)
                .unwrap_or_else(|| Decision::Deny("restricted".into()));
        }
        Visibility::Task if req.action != Permission::Read && member.role != TeamRole::Admin => {
            return Decision::Deny("task_read_only".into());
        }
        _ => {}
    }

    if role_defaults(member.role).contains(&req.action) {
        return Decision::Allow(format!("role_default:{}", member.role.as_str()));
    }
    acl_decision(req, member).unwrap_or_else(|| Decision::Deny("no_permission".into()))
}

fn subject_matches(entry: &AclEntry, req: &AccessRequest<'_>, member: &TeamMember) -> bool {
    match entry.subject_type {
        SubjectType::User => entry.subject_id == req.user_id,
        SubjectType::TeamRole => entry.subject_id == member.role.as_str(),
        SubjectType::Agent => req.agent_id.is_some_and(|a| a == entry.subject_id),
    }
}

/// Resolves the ACL rows that apply to this request. Returns `None` when no
/// row decides the outcome, letting the caller pick the fallback reason.
///
/// Allow-only: `deny` rows are stored
/// but never consulted.
fn acl_decision(req: &AccessRequest<'_>, member: &TeamMember) -> Option<Decision> {
    req.acl
        .iter()
        .filter(|e| e.asset_id == req.asset.map_or("", |a| a.asset_id.as_str()))
        .filter(|e| e.permission == req.action && subject_matches(e, req, member))
        .find(|e| e.effect == Effect::Allow)
        .map(|e| Decision::Allow(format!("acl:{}", e.id)))
}

/// Whether an asset may be fixed-bound to an agent of `agent_team_id`
/// owned by `agent_owner`.
pub fn can_bind(asset: &Asset, agent_team_id: &str, agent_owner: &str) -> bool {
    let same_team = asset.team_id == agent_team_id;
    match asset.visibility {
        Visibility::Team | Visibility::Agent => same_team,
        Visibility::Private => same_team && asset.owner_user_id == agent_owner,
        Visibility::Task | Visibility::Restricted => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{AssetType, MemberStatus};
    use serde_json::json;

    fn asset(visibility: Visibility) -> Asset {
        Asset {
            asset_id: "ast-1".into(),
            team_id: "team-1".into(),
            asset_type: AssetType::Skill,
            name: "n".into(),
            description: None,
            owner_user_id: "owner".into(),
            source_type: "manual".into(),
            source_ref: None,
            version: 1,
            visibility,
            status: AssetStatus::Active,
            confidence: None,
            expires_at: None,
            last_used_at: None,
            usage_count: 0,
            content_ref: None,
            created_at: String::new(),
            updated_at: String::new(),
            metadata_json: json!({}),
        }
    }

    fn member(role: TeamRole) -> TeamMember {
        TeamMember {
            id: "m".into(),
            team_id: "team-1".into(),
            user_id: "u".into(),
            role,
            status: MemberStatus::Active,
            joined_at: String::new(),
        }
    }

    fn acl(permission: Permission, effect: Effect) -> AclEntry {
        AclEntry {
            id: "acl-1".into(),
            asset_id: "ast-1".into(),
            subject_type: SubjectType::User,
            subject_id: "u".into(),
            permission,
            effect,
            granted_by: "owner".into(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn decide(a: &Asset, m: Option<&TeamMember>, action: Permission, acl: &[AclEntry]) -> Decision {
        check_asset_permission(&AccessRequest {
            asset: Some(a),
            user_id: "u",
            agent_id: None,
            membership: m,
            action,
            acl,
        })
    }

    #[test]
    fn private_assets_are_hidden_even_from_admins() {
        let admin = member(TeamRole::Admin);
        assert!(!decide(
            &asset(Visibility::Private),
            Some(&admin),
            Permission::Read,
            &[]
        )
        .is_allowed());
    }

    #[test]
    fn team_members_read_but_cannot_write_without_acl() {
        let m = member(TeamRole::Member);
        let a = asset(Visibility::Team);
        assert!(decide(&a, Some(&m), Permission::Read, &[]).is_allowed());
        assert!(!decide(&a, Some(&m), Permission::Write, &[]).is_allowed());
        let grant = [acl(Permission::Write, Effect::Allow)];
        assert_eq!(
            decide(&a, Some(&m), Permission::Write, &grant),
            Decision::Allow("acl:acl-1".into())
        );
    }

    #[test]
    fn restricted_requires_acl_for_non_admins() {
        let m = member(TeamRole::Member);
        let a = asset(Visibility::Restricted);
        assert!(!decide(&a, Some(&m), Permission::Read, &[]).is_allowed());
        assert!(decide(
            &a,
            Some(&m),
            Permission::Read,
            &[acl(Permission::Read, Effect::Allow)]
        )
        .is_allowed());
    }

    #[test]
    fn non_members_are_denied() {
        assert_eq!(
            decide(&asset(Visibility::Team), None, Permission::Read, &[]).reason(),
            "not_team_member"
        );
    }

    #[test]
    fn owner_always_allowed() {
        let a = asset(Visibility::Private);
        let req = AccessRequest {
            asset: Some(&a),
            user_id: "owner",
            agent_id: None,
            membership: None,
            action: Permission::Delete,
            acl: &[],
        };
        assert!(check_asset_permission(&req).is_allowed());
    }
}
