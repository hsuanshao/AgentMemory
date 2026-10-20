//! Skill storage ports.

use async_trait::async_trait;
use memory_domain::error::DomainResult;
use memory_domain::scope::DepartmentId;
use memory_domain::skill::{SkillStatus, SkillVersion};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default)]
pub struct SkillFilter {
    pub team_id: Option<String>,
    pub owner_agent_id: Option<String>,
    pub owner_user_id: Option<String>,
    pub name_prefix: Option<String>,
    pub statuses: Vec<SkillStatus>,
}

/// Versioned SKILL.md rows. Every write produces a new immutable version
/// and moves the head flag; the head row is indexed for search.
#[async_trait]
pub trait SkillRepository: Send + Sync {
    async fn skill_id_exists(
        &self,
        department: &DepartmentId,
        skill_id: &str,
    ) -> DomainResult<bool>;
    async fn head(
        &self,
        department: &DepartmentId,
        skill_id: &str,
    ) -> DomainResult<Option<SkillVersion>>;
    async fn version(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
    ) -> DomainResult<Option<SkillVersion>>;
    async fn find_head_by_name(
        &self,
        department: &DepartmentId,
        team_id: &str,
        agent_id: &str,
        name: &str,
    ) -> DomainResult<Option<SkillVersion>>;
    /// Inserts `next` as the new head (demoting the previous head).
    async fn append_version(
        &self,
        department: &DepartmentId,
        next: &SkillVersion,
    ) -> DomainResult<()>;
    async fn list_heads(
        &self,
        department: &DepartmentId,
        filter: &SkillFilter,
        limit: usize,
        offset: usize,
    ) -> DomainResult<(Vec<SkillVersion>, usize)>;
    async fn search_heads(
        &self,
        department: &DepartmentId,
        filter: &SkillFilter,
        query: &str,
        limit: usize,
    ) -> DomainResult<Vec<(SkillVersion, f64, String)>>;
    async fn list_versions(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        limit: usize,
        offset: usize,
    ) -> DomainResult<(Vec<SkillVersion>, usize)>;
    /// Deletes every version; returns how many rows were removed.
    async fn delete_skill(&self, department: &DepartmentId, skill_id: &str) -> DomainResult<usize>;
    async fn delete_versions(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        versions: &[u32],
    ) -> DomainResult<usize>;
    async fn count_heads(&self, department: &DepartmentId) -> DomainResult<usize>;
}

/// Resource files of a skill version (`skills/<id>/v<n>/files/<path>`).
#[async_trait]
pub trait SkillResourceStore: Send + Sync {
    async fn copy_version(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        from: u32,
        to: u32,
    ) -> DomainResult<()>;
    async fn write(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
        path: &str,
        bytes: &[u8],
    ) -> DomainResult<()>;
    async fn remove(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
        path: &str,
    ) -> DomainResult<()>;
    async fn read(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
        path: &str,
    ) -> DomainResult<Option<Vec<u8>>>;
    async fn delete_version(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
    ) -> DomainResult<()>;
    async fn delete_skill(&self, department: &DepartmentId, skill_id: &str) -> DomainResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferedMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferCounters {
    pub tool_call_count: u32,
    pub byte_count: usize,
}

/// Identifies one agent's conversation buffer for skill extraction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BufferKey {
    pub user_id: String,
    pub team_id: String,
    pub agent_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillTask {
    pub task_id: String,
    pub key: BufferKey,
    pub archive_key: String,
    pub archived_at_ms: i64,
    pub reason: String,
    /// Optional hint from the main agent.
    pub hint: Option<String>,
    pub status: String,
    pub attempts: u32,
    pub error: Option<String>,
    pub candidates: serde_json::Value,
}

#[async_trait]
pub trait SkillBufferStore: Send + Sync {
    async fn load(
        &self,
        department: &DepartmentId,
        key: &BufferKey,
    ) -> DomainResult<(Vec<BufferedMessage>, BufferCounters)>;
    async fn save(
        &self,
        department: &DepartmentId,
        key: &BufferKey,
        messages: &[BufferedMessage],
        counters: &BufferCounters,
    ) -> DomainResult<()>;
    async fn clear(&self, department: &DepartmentId, key: &BufferKey) -> DomainResult<()>;
    /// Persists an archived slice and returns its key.
    async fn archive(
        &self,
        department: &DepartmentId,
        key: &BufferKey,
        at_ms: i64,
        messages: &[BufferedMessage],
    ) -> DomainResult<String>;
    async fn read_archive(
        &self,
        department: &DepartmentId,
        archive_key: &str,
    ) -> DomainResult<Option<Vec<BufferedMessage>>>;
    async fn put_task(&self, department: &DepartmentId, task: &SkillTask) -> DomainResult<()>;
    async fn get_task(
        &self,
        department: &DepartmentId,
        task_id: &str,
    ) -> DomainResult<Option<SkillTask>>;
}
