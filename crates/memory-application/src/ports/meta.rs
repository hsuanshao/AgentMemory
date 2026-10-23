//! Metadata-plane storage ports, one per aggregate.

use async_trait::async_trait;
use memory_domain::error::DomainResult;
use memory_domain::meta::{
    AclEntry, Agent, AgentBinding, AgentStatus, Asset, AssetStatus, AssetType, ParticipationLog,
    Task, TaskAgent, TaskStatus, Team, TeamMember, User, UserKey, UserStatus, UserType, Visibility,
};
use memory_domain::scope::DepartmentId;
use serde::{Deserialize, Serialize};

use crate::common::{BatchDeleteResult, Page};

#[derive(Debug, Clone, Default)]
pub struct UserFilter {
    pub team_id: Option<String>,
    pub user_ids: Option<Vec<String>>,
    pub username: Option<String>,
    pub user_type: Option<UserType>,
    pub status: Option<UserStatus>,
}

#[async_trait]
pub trait UserRepository: Send + Sync {
    /// Inserts the user and its default key atomically.
    async fn create_user(
        &self,
        department: &DepartmentId,
        user: &User,
        default_key: &UserKey,
    ) -> DomainResult<()>;
    async fn get_user(
        &self,
        department: &DepartmentId,
        user_id: &str,
    ) -> DomainResult<Option<User>>;
    async fn find_user_by_name(
        &self,
        department: &DepartmentId,
        username: &str,
    ) -> DomainResult<Option<User>>;
    async fn list_users(
        &self,
        department: &DepartmentId,
        filter: &UserFilter,
        page: Page,
    ) -> DomainResult<(Vec<User>, usize)>;
    async fn count_users(&self, department: &DepartmentId) -> DomainResult<usize>;
    /// Cascades keys, memberships and user ACL rows.
    async fn delete_users(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult>;
}

#[async_trait]
pub trait UserKeyRepository: Send + Sync {
    async fn insert_key(&self, department: &DepartmentId, key: &UserKey) -> DomainResult<()>;
    async fn find_key_by_value(
        &self,
        department: &DepartmentId,
        value: &str,
    ) -> DomainResult<Option<UserKey>>;
    async fn get_key(
        &self,
        department: &DepartmentId,
        key_id: &str,
    ) -> DomainResult<Option<UserKey>>;
    async fn list_keys(
        &self,
        department: &DepartmentId,
        user_id: &str,
        page: Page,
    ) -> DomainResult<(Vec<UserKey>, usize)>;
    async fn update_key(&self, department: &DepartmentId, key: &UserKey) -> DomainResult<()>;
    /// Physically deletes the key; promotes the oldest remaining active key
    /// to default when the default was removed.
    async fn delete_key(&self, department: &DepartmentId, key_id: &str) -> DomainResult<()>;
    async fn touch_key(
        &self,
        department: &DepartmentId,
        key_id: &str,
        at: &str,
    ) -> DomainResult<()>;
}

#[async_trait]
pub trait TeamRepository: Send + Sync {
    /// Inserts the team and makes the owner an admin member.
    async fn create_team(
        &self,
        department: &DepartmentId,
        team: &Team,
        owner_membership: &TeamMember,
    ) -> DomainResult<()>;
    async fn get_team(
        &self,
        department: &DepartmentId,
        team_id: &str,
    ) -> DomainResult<Option<Team>>;
    async fn update_team(&self, department: &DepartmentId, team: &Team) -> DomainResult<()>;
    /// Teams `user_id` is an active member of; `None` lists every team.
    async fn list_teams_of_user(
        &self,
        department: &DepartmentId,
        user_id: Option<&str>,
        name: Option<&str>,
        page: Page,
    ) -> DomainResult<(Vec<Team>, usize)>;
    async fn count_teams(&self, department: &DepartmentId) -> DomainResult<usize>;
    /// Cascades members, agents, tasks, assets, bindings and ACL rows.
    async fn delete_teams(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult>;
}

#[async_trait]
pub trait MemberRepository: Send + Sync {
    async fn upsert_member(
        &self,
        department: &DepartmentId,
        member: &TeamMember,
    ) -> DomainResult<()>;
    async fn get_member(
        &self,
        department: &DepartmentId,
        team_id: &str,
        user_id: &str,
    ) -> DomainResult<Option<TeamMember>>;
    async fn list_members(
        &self,
        department: &DepartmentId,
        team_id: &str,
        page: Page,
    ) -> DomainResult<(Vec<TeamMember>, usize)>;
    async fn list_memberships(
        &self,
        department: &DepartmentId,
        user_id: &str,
    ) -> DomainResult<Vec<TeamMember>>;
    async fn remove_member(
        &self,
        department: &DepartmentId,
        team_id: &str,
        user_id: &str,
    ) -> DomainResult<bool>;
}

#[derive(Debug, Clone, Default)]
pub struct AgentFilter {
    pub team_id: Option<String>,
    pub owner_user_id: Option<String>,
    pub status: Option<AgentStatus>,
    pub name: Option<String>,
}

#[async_trait]
pub trait AgentRepository: Send + Sync {
    async fn insert_agent(&self, department: &DepartmentId, agent: &Agent) -> DomainResult<()>;
    async fn get_agent(
        &self,
        department: &DepartmentId,
        agent_id: &str,
    ) -> DomainResult<Option<Agent>>;
    async fn update_agent(&self, department: &DepartmentId, agent: &Agent) -> DomainResult<()>;
    async fn list_agents(
        &self,
        department: &DepartmentId,
        filter: &AgentFilter,
        page: Page,
    ) -> DomainResult<(Vec<Agent>, usize)>;
    /// Cascades task links, bindings and the agent's chat-memory asset.
    async fn delete_agents(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult>;
}

#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    pub team_id: Option<String>,
    pub creator_user_id: Option<String>,
    pub status: Option<TaskStatus>,
}

#[derive(Debug, Clone, Default)]
pub struct ParticipationFilter {
    pub team_id: String,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
}

#[async_trait]
pub trait TaskRepository: Send + Sync {
    async fn insert_task(
        &self,
        department: &DepartmentId,
        task: &Task,
        links: &[TaskAgent],
    ) -> DomainResult<()>;
    async fn get_task(
        &self,
        department: &DepartmentId,
        task_id: &str,
    ) -> DomainResult<Option<Task>>;
    async fn update_task(&self, department: &DepartmentId, task: &Task) -> DomainResult<()>;
    async fn list_tasks(
        &self,
        department: &DepartmentId,
        filter: &TaskFilter,
        page: Page,
    ) -> DomainResult<(Vec<Task>, usize)>;
    async fn delete_tasks(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult>;
    async fn link_agent(&self, department: &DepartmentId, link: &TaskAgent) -> DomainResult<()>;
    async fn unlink_agent(
        &self,
        department: &DepartmentId,
        task_id: &str,
        agent_id: &str,
    ) -> DomainResult<bool>;
    async fn list_task_agents(
        &self,
        department: &DepartmentId,
        task_id: &str,
        page: Page,
    ) -> DomainResult<(Vec<TaskAgent>, usize)>;
    async fn append_participation(
        &self,
        department: &DepartmentId,
        log: &ParticipationLog,
    ) -> DomainResult<()>;
    async fn list_participation(
        &self,
        department: &DepartmentId,
        filter: &ParticipationFilter,
        page: Page,
    ) -> DomainResult<(Vec<ParticipationLog>, usize)>;
}

#[derive(Debug, Clone, Default)]
pub struct AssetFilter {
    pub team_id: Option<String>,
    pub asset_type: Option<AssetType>,
    pub status: Option<AssetStatus>,
    pub owner_user_id: Option<String>,
    pub visibility: Option<Vec<Visibility>>,
}

#[async_trait]
pub trait AssetRepository: Send + Sync {
    async fn insert_asset(&self, department: &DepartmentId, asset: &Asset) -> DomainResult<()>;
    async fn get_asset(
        &self,
        department: &DepartmentId,
        asset_id: &str,
    ) -> DomainResult<Option<Asset>>;
    async fn update_asset(&self, department: &DepartmentId, asset: &Asset) -> DomainResult<()>;
    /// All matching assets, newest first (filtering by permission happens
    /// in the use case).
    async fn list_assets(
        &self,
        department: &DepartmentId,
        filter: &AssetFilter,
    ) -> DomainResult<Vec<Asset>>;
    /// Idempotent; cascades bindings and ACL rows.
    async fn delete_assets(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult>;
    async fn touch_asset(
        &self,
        department: &DepartmentId,
        asset_id: &str,
        at: &str,
    ) -> DomainResult<()>;
}

#[async_trait]
pub trait BindingRepository: Send + Sync {
    /// Replaces every binding of the agent.
    async fn replace_bindings(
        &self,
        department: &DepartmentId,
        agent_id: &str,
        bindings: &[AgentBinding],
    ) -> DomainResult<()>;
    /// Adds a binding unless one already exists for (agent, asset).
    async fn ensure_binding(
        &self,
        department: &DepartmentId,
        binding: &AgentBinding,
    ) -> DomainResult<()>;
    async fn list_bindings(
        &self,
        department: &DepartmentId,
        agent_id: &str,
    ) -> DomainResult<Vec<AgentBinding>>;
}

#[async_trait]
pub trait AclRepository: Send + Sync {
    /// Upserts on (asset, subject, permission).
    async fn grant(&self, department: &DepartmentId, entry: &AclEntry) -> DomainResult<AclEntry>;
    async fn revoke(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<AclEntry>>;
    async fn get_acl(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<AclEntry>>;
    async fn list_acl(
        &self,
        department: &DepartmentId,
        asset_id: &str,
    ) -> DomainResult<Vec<AclEntry>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigParam {
    pub module: String,
    pub param_name: String,
    pub param_value: String,
    /// `global` or `user`.
    pub scope: String,
}

#[async_trait]
pub trait ConfigRepository: Send + Sync {
    /// Effective params: user values override global ones.
    async fn get_params(
        &self,
        department: &DepartmentId,
        user_id: Option<&str>,
        module: &str,
    ) -> DomainResult<Vec<ConfigParam>>;
    async fn set_user_params(
        &self,
        department: &DepartmentId,
        user_id: &str,
        module: &str,
        params: &[(String, String)],
    ) -> DomainResult<()>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeEntity {
    pub knowledge_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub service_url: String,
    pub name: String,
    pub summary: Option<String>,
    pub team_id: String,
    pub user_id: Option<String>,
    pub repo_url: Option<String>,
    pub branch: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[async_trait]
pub trait KnowledgeRepository: Send + Sync {
    async fn upsert_knowledge(
        &self,
        department: &DepartmentId,
        entity: &KnowledgeEntity,
    ) -> DomainResult<()>;
    async fn get_knowledge(
        &self,
        department: &DepartmentId,
        id: &str,
    ) -> DomainResult<Option<KnowledgeEntity>>;
    async fn delete_knowledge(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult>;
    async fn list_knowledge(
        &self,
        department: &DepartmentId,
        team_id: &str,
        kind: Option<&str>,
        ids: Option<&[String]>,
        page: Page,
    ) -> DomainResult<(Vec<KnowledgeEntity>, usize)>;
}

/// A redaction pattern a system admin added to the department, on top of
/// the built-in rules and the deployment's. Never deleted: removing one
/// disables it, so the record of who added what stays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedactionRule {
    pub rule_id: String,
    /// The `<kind>` of `[REDACTED:<kind>]`.
    pub kind: String,
    pub pattern: String,
    #[serde(default)]
    pub description: String,
    pub enabled: bool,
    pub created_by: String,
    pub created_at: String,
    pub updated_by: String,
    pub updated_at: String,
}

#[async_trait]
pub trait RedactionRuleRepository: Send + Sync {
    /// Every rule, enabled or not, oldest first.
    async fn list_redaction_rules(
        &self,
        department: &DepartmentId,
    ) -> DomainResult<Vec<RedactionRule>>;
    async fn put_redaction_rule(
        &self,
        department: &DepartmentId,
        rule: &RedactionRule,
    ) -> DomainResult<()>;
}

/// Everything the metadata use cases need, as one injectable bundle.
pub trait MetaStore:
    UserRepository
    + UserKeyRepository
    + TeamRepository
    + MemberRepository
    + AgentRepository
    + TaskRepository
    + AssetRepository
    + BindingRepository
    + AclRepository
    + ConfigRepository
    + KnowledgeRepository
    + RedactionRuleRepository
{
}

impl<T> MetaStore for T where
    T: UserRepository
        + UserKeyRepository
        + TeamRepository
        + MemberRepository
        + AgentRepository
        + TaskRepository
        + AssetRepository
        + BindingRepository
        + AclRepository
        + ConfigRepository
        + KnowledgeRepository
        + RedactionRuleRepository
{
}
