//! Metadata plane entities: users, keys, teams, agents, tasks, assets,
//! bindings and ACL entries.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{DomainError, DomainResult};

pub mod access;
pub mod memory_access;
pub mod visibility;

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            pub fn parse(raw: &str) -> DomainResult<Self> {
                match raw {
                    $($text => Ok(Self::$variant),)+
                    other => Err(DomainError::invalid(format!(
                        concat!("invalid ", stringify!($name), ": {}"), other
                    ))),
                }
            }
        }
    };
}

string_enum!(UserType { Normal => "normal", SystemAdmin => "system_admin" });
string_enum!(UserStatus { Active => "active", Inactive => "inactive", Invited => "invited" });
string_enum!(KeyStatus { Active => "active", Revoked => "revoked" });
string_enum!(TeamStatus { Active => "active", Archived => "archived" });
string_enum!(
    /// A member's role in a team. A `leader` uses memory like a member and
    /// also sees how the team's distillation is doing (pipeline state,
    /// generation logs, dead letters), never the members' own memory.
    /// Managing members and agents stays with team admins.
    TeamRole { Admin => "admin", Leader => "leader", Member => "member" }
);
string_enum!(MemberStatus { Active => "active", Removed => "removed" });
string_enum!(AgentStatus { Active => "active", Inactive => "inactive" });
string_enum!(TaskStatus { Running => "running", Completed => "completed" });
string_enum!(AssetType {
    Skill => "skill",
    LlmWiki => "llm_wiki",
    CodeGraph => "code_graph",
    ChatMemory => "chat_memory",
});
string_enum!(Visibility {
    Private => "private",
    Team => "team",
    Restricted => "restricted",
    Agent => "agent",
    Task => "task",
});
string_enum!(AssetStatus {
    Draft => "draft",
    Candidate => "candidate",
    Approved => "approved",
    Active => "active",
    Deprecated => "deprecated",
    Archived => "archived",
    Failed => "failed",
});
string_enum!(InjectionMode { Direct => "direct", Summary => "summary", Tool => "tool", Reference => "reference" });
string_enum!(SubjectType { User => "user", TeamRole => "team_role", Agent => "agent" });
string_enum!(Permission {
    Read => "read",
    Write => "write",
    Delete => "delete",
    Assign => "assign",
    Share => "share",
    Use => "use",
});
string_enum!(Effect { Allow => "allow", Deny => "deny" });

impl AssetStatus {
    /// Statuses hidden from `list-accessible`.
    pub fn is_retired(self) -> bool {
        matches!(self, Self::Archived | Self::Deprecated | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub user_id: String,
    pub username: String,
    pub user_type: UserType,
    pub status: UserStatus,
    pub created_at: String,
    pub updated_at: String,
    pub metadata_json: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserKey {
    pub key_id: String,
    pub user_id: String,
    pub key_value: String,
    pub name: Option<String>,
    pub status: KeyStatus,
    pub is_default: bool,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub created_at: String,
}

pub const USER_KEY_PREFIX: &str = "sk-mem-";

impl UserKey {
    /// `sk-mem-****abcd`: the only form ever shown after creation.
    pub fn masked(&self) -> String {
        let tail: String = self
            .key_value
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{USER_KEY_PREFIX}****{tail}")
    }

    pub fn is_expired(&self, now_iso: &str) -> bool {
        self.expires_at.as_deref().is_some_and(|exp| exp <= now_iso)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Team {
    pub team_id: String,
    pub name: String,
    pub description: Option<String>,
    pub owner_user_id: String,
    pub status: TeamStatus,
    pub created_at: String,
    pub updated_at: String,
    pub metadata_json: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamMember {
    pub id: String,
    pub team_id: String,
    pub user_id: String,
    pub role: TeamRole,
    pub status: MemberStatus,
    pub joined_at: String,
}

impl TeamMember {
    pub fn is_active(&self) -> bool {
        self.status == MemberStatus::Active
    }

    pub fn is_admin(&self) -> bool {
        self.is_active() && self.role == TeamRole::Admin
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub agent_id: String,
    pub team_id: String,
    pub owner_user_id: String,
    pub name: String,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub visibility: Visibility,
    pub status: AgentStatus,
    pub created_at: String,
    pub updated_at: String,
    pub metadata_json: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub task_id: String,
    pub team_id: String,
    pub creator_user_id: String,
    pub title: String,
    pub description: Option<String>,
    pub source_type: String,
    pub source_url: Option<String>,
    pub status: TaskStatus,
    pub auto_assign_floating_assets: bool,
    pub risk_level: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub metadata_json: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskAgent {
    pub id: String,
    pub task_id: String,
    pub agent_id: String,
    pub role_in_task: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParticipationLog {
    pub id: String,
    pub team_id: String,
    pub task_id: String,
    pub agent_id: String,
    pub user_id: String,
    pub source: String,
    pub created_at: String,
    pub metadata_json: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub asset_id: String,
    pub team_id: String,
    pub asset_type: AssetType,
    pub name: String,
    pub description: Option<String>,
    pub owner_user_id: String,
    pub source_type: String,
    pub source_ref: Option<String>,
    pub version: u32,
    pub visibility: Visibility,
    pub status: AssetStatus,
    pub confidence: Option<f64>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub usage_count: u64,
    pub content_ref: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub metadata_json: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentBinding {
    pub id: String,
    pub agent_id: String,
    pub asset_id: String,
    pub asset_type: AssetType,
    pub injection_mode: InjectionMode,
    pub priority: i64,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AclEntry {
    pub id: String,
    pub asset_id: String,
    pub subject_type: SubjectType,
    pub subject_id: String,
    pub permission: Permission,
    pub effect: Effect,
    pub granted_by: String,
    pub created_at: String,
    pub updated_at: String,
}

/// `chat_memory-{team}-{agent}`: one chat-memory asset per agent.
pub fn chat_memory_asset_id(team_id: &str, agent_id: &str) -> String {
    format!("chat_memory-{team_id}-{agent_id}")
}
