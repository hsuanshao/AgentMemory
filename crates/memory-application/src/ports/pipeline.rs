//! Ports of the distillation pipeline: durable scheduling state, the job
//! queue, custom memory prompts and generation logs.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memory_domain::error::DomainResult;
use memory_domain::memory::Layer;
use memory_domain::profile::ProfileScope;
use memory_domain::scope::{InstanceId, MemoryScope};
use serde::{Deserialize, Serialize};

use crate::common::Page;

/// Per-session L1 scheduling state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub instance: InstanceId,
    pub scope: MemoryScope,
    pub session_id: String,
    pub conversation_count: u32,
    pub warmup: u32,
    /// Storage sequence of the last L0 message distilled into L1.
    pub l1_cursor: Option<i64>,
    /// Idle-debounce deadline for the next L1 run.
    pub l1_due_at: Option<DateTime<Utc>>,
    pub last_l1_at: Option<DateTime<Utc>>,
    pub last_scene_name: Option<String>,
    pub last_active_at: DateTime<Utc>,
}

/// Per team + agent L2/L3 state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileState {
    pub instance: InstanceId,
    pub profile: ProfileScope,
    pub l2_pending: bool,
    pub l2_due_at: Option<DateTime<Utc>>,
    /// Storage sequence of the last L1 atom consolidated into L2.
    pub l2_cursor: Option<i64>,
    pub last_l2_at: Option<DateTime<Utc>>,
    pub last_l3_at: Option<DateTime<Utc>>,
    pub scenes_processed: u32,
    pub memories_since_persona: u32,
    pub persona_update_request: Option<String>,
}

impl ProfileState {
    pub fn new(instance: InstanceId, profile: ProfileScope) -> Self {
        Self {
            instance,
            profile,
            l2_pending: false,
            l2_due_at: None,
            l2_cursor: None,
            last_l2_at: None,
            last_l3_at: None,
            scenes_processed: 0,
            memories_since_persona: 0,
            persona_update_request: None,
        }
    }
}

/// Durable pipeline state so timers survive restarts.
#[async_trait]
pub trait PipelineStateRepository: Send + Sync {
    async fn get_session(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        session_id: &str,
    ) -> DomainResult<Option<SessionState>>;
    async fn put_session(&self, state: &SessionState) -> DomainResult<()>;
    async fn get_profile(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
    ) -> DomainResult<Option<ProfileState>>;
    async fn put_profile(&self, state: &ProfileState) -> DomainResult<()>;
    /// Sessions / profiles whose deadline is at or before `now`, across
    /// every known instance.
    async fn due_sessions(&self, now: DateTime<Utc>) -> DomainResult<Vec<SessionState>>;
    async fn due_profiles(&self, now: DateTime<Utc>) -> DomainResult<Vec<ProfileState>>;
    async fn list_sessions(
        &self,
        instance: &InstanceId,
        limit: usize,
    ) -> DomainResult<Vec<SessionState>>;
    async fn list_profiles(
        &self,
        instance: &InstanceId,
        limit: usize,
    ) -> DomainResult<Vec<ProfileState>>;
    async fn delete_profile(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
    ) -> DomainResult<()>;
    /// Drops all scheduling state of an instance.
    async fn delete_instance(&self, instance: &InstanceId) -> DomainResult<()>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PipelineJob {
    L1 {
        instance: InstanceId,
        scope: MemoryScope,
        session_id: String,
    },
    L2 {
        instance: InstanceId,
        profile: ProfileScope,
    },
    L3 {
        instance: InstanceId,
        profile: ProfileScope,
    },
    SkillExtract {
        instance: InstanceId,
        task_id: String,
    },
}

impl PipelineJob {
    /// Jobs with the same key never run concurrently.
    pub fn serial_key(&self) -> String {
        match self {
            Self::L1 {
                instance,
                scope,
                session_id,
            } => format!("{instance}/{}/s:{session_id}", scope.key()),
            Self::L2 { instance, profile } | Self::L3 { instance, profile } => {
                format!("{instance}/{}", profile.key())
            }
            Self::SkillExtract { instance, task_id } => format!("{instance}/skill/{task_id}"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::L1 { .. } => "l1",
            Self::L2 { .. } => "l2",
            Self::L3 { .. } => "l3",
            Self::SkillExtract { .. } => "skill",
        }
    }

    pub fn instance(&self) -> &InstanceId {
        match self {
            Self::L1 { instance, .. }
            | Self::L2 { instance, .. }
            | Self::L3 { instance, .. }
            | Self::SkillExtract { instance, .. } => instance,
        }
    }
}

/// A job that failed on every attempt and was set aside instead of
/// retried. It stays until it is redriven or pushed out by newer ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadLetter {
    pub job: PipelineJob,
    /// Retries it had used up.
    pub attempt: u32,
    /// Why the last attempt failed. Absent on letters written before it was
    /// recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When it was set aside (epoch ms); 0 when unknown.
    #[serde(default)]
    pub dead_at_ms: i64,
}

/// Which dead letters to list or redrive.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeadLetterFilter {
    pub instance: Option<InstanceId>,
    /// A job kind (`PipelineJob::kind`).
    pub kind: Option<String>,
}

impl DeadLetterFilter {
    pub fn matches(&self, letter: &DeadLetter) -> bool {
        self.instance
            .as_ref()
            .is_none_or(|i| i == letter.job.instance())
            && self.kind.as_deref().is_none_or(|k| k == letter.job.kind())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct QueueStats {
    pub pending: usize,
    pub running: usize,
    pub dead_letter: usize,
}

#[async_trait]
pub trait JobQueue: Send + Sync {
    async fn enqueue(&self, job: PipelineJob) -> DomainResult<()>;
    async fn stats(&self) -> DomainResult<QueueStats>;
    /// Dead letters matching `filter`, newest first, at most `limit`.
    async fn dead_letters(
        &self,
        filter: &DeadLetterFilter,
        limit: usize,
    ) -> DomainResult<Vec<DeadLetter>>;
    /// Queues matching dead letters again as first attempts, oldest first,
    /// at most `limit`. Returns how many were requeued.
    async fn redrive(&self, filter: &DeadLetterFilter, limit: usize) -> DomainResult<usize>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptTarget {
    Instance,
    Team,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryPrompt {
    pub memory_prompt_id: String,
    pub name: String,
    pub layer: Layer,
    pub prompt: String,
    pub version: u32,
    pub status: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptSetting {
    pub target_type: PromptTarget,
    pub team_id: Option<String>,
    pub agent_id: Option<String>,
    pub layer: Layer,
    pub memory_prompt_id: String,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptLogEntry {
    pub log_id: String,
    pub action: String,
    pub memory_prompt_id: Option<String>,
    pub layer: Layer,
    pub target_type: PromptTarget,
    pub team_id: Option<String>,
    pub agent_id: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Default)]
pub struct PromptSettingFilter {
    pub memory_prompt_id: Option<String>,
    pub target_type: Option<PromptTarget>,
    pub team_id: Option<String>,
    pub agent_id: Option<String>,
    pub layer: Option<Layer>,
}

#[async_trait]
pub trait MemoryPromptRepository: Send + Sync {
    async fn insert(&self, instance: &InstanceId, prompt: &MemoryPrompt) -> DomainResult<()>;
    async fn get(&self, instance: &InstanceId, id: &str) -> DomainResult<Option<MemoryPrompt>>;
    async fn update(&self, instance: &InstanceId, prompt: &MemoryPrompt) -> DomainResult<()>;
    async fn delete(&self, instance: &InstanceId, ids: &[String]) -> DomainResult<usize>;
    async fn list(
        &self,
        instance: &InstanceId,
        layer: Option<Layer>,
        page: Page,
        newest_first: bool,
    ) -> DomainResult<Vec<MemoryPrompt>>;
    async fn count(&self, instance: &InstanceId) -> DomainResult<usize>;
    async fn put_setting(&self, instance: &InstanceId, setting: &PromptSetting)
        -> DomainResult<()>;
    async fn clear_setting(
        &self,
        instance: &InstanceId,
        target: PromptTarget,
        team_id: Option<&str>,
        agent_id: Option<&str>,
        layer: Layer,
    ) -> DomainResult<usize>;
    async fn list_settings(
        &self,
        instance: &InstanceId,
        filter: &PromptSettingFilter,
        page: Page,
    ) -> DomainResult<Vec<PromptSetting>>;
    async fn append_log(&self, instance: &InstanceId, entry: &PromptLogEntry) -> DomainResult<()>;
    async fn list_logs(
        &self,
        instance: &InstanceId,
        filter: &PromptSettingFilter,
        action: Option<&str>,
        from_ms: i64,
        to_ms: i64,
        page: Page,
    ) -> DomainResult<Vec<PromptLogEntry>>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationRef {
    pub layer: String,
    pub record_id: String,
}

/// Provenance of one L1/L2/L3 generation run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationLog {
    pub log_id: String,
    pub layer: Layer,
    pub status: String,
    pub team_id: String,
    pub agent_id: String,
    pub user_id: Option<String>,
    pub session_id: Option<String>,
    pub memory_prompt_id: String,
    pub prompt_sha256: String,
    pub model: String,
    pub input_refs: Vec<GenerationRef>,
    pub output_refs: Vec<GenerationRef>,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub latency_ms: i64,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GenerationLogFilter {
    pub layer: Option<Layer>,
    pub status: Option<String>,
    pub from_ms: i64,
    pub to_ms: i64,
    /// Opaque cursor: return logs started strictly before this.
    pub before: Option<(i64, String)>,
}

#[async_trait]
pub trait GenerationLogRepository: Send + Sync {
    async fn append(&self, instance: &InstanceId, log: &GenerationLog) -> DomainResult<()>;
    async fn get(&self, instance: &InstanceId, log_id: &str)
        -> DomainResult<Option<GenerationLog>>;
    async fn find_by_output(
        &self,
        instance: &InstanceId,
        layer: Layer,
        memory_id: &str,
    ) -> DomainResult<Option<GenerationLog>>;
    async fn list(
        &self,
        instance: &InstanceId,
        filter: &GenerationLogFilter,
        limit: usize,
    ) -> DomainResult<Vec<GenerationLog>>;
    async fn count(&self, instance: &InstanceId) -> DomainResult<usize>;
    /// How many generations started since `from_ms`, per layer and status.
    async fn summarize(
        &self,
        instance: &InstanceId,
        from_ms: i64,
    ) -> DomainResult<Vec<GenerationCount>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GenerationCount {
    pub layer: Layer,
    pub status: String,
    pub count: usize,
}
