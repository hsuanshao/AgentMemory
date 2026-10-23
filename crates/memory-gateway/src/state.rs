//! Use cases the HTTP layer dispatches to, assembled by the composition root.

use std::sync::Arc;
use std::time::Instant;

use memory_application::admin::{
    ChatMemoryService, DepartmentLifecycle, KnowledgeService, StatsService,
};
use memory_application::memory::recall::RecallService;
use memory_application::memory::service::MemoryService;
use memory_application::memory::session::SessionService;
use memory_application::meta::MetaService;
use memory_application::migration::LegacyScopeMigration;
use memory_application::pipeline::generation_logs::GenerationLogService;
use memory_application::pipeline::memory_prompts::MemoryPromptService;
use memory_application::pipeline::service::PipelineService;
use memory_application::redaction::RedactionService;
use memory_application::skill::extraction::SkillExtractionService;
use memory_application::skill::service::SkillService;
use memory_application::upgrade::LegacyUpgrade;

#[derive(Debug, Clone)]
pub struct GatewaySettings {
    /// Bearer token required on ops, metadata and internal routes (None =
    /// open). The data plane is authorized by user key instead.
    pub api_key: Option<String>,
    pub cors_origins: Vec<String>,
    pub max_body_bytes: usize,
    pub max_session_body_bytes: usize,
    pub version: String,
}

/// Facts `/health` reports about dependencies.
#[derive(Debug, Clone)]
pub struct HealthFacts {
    pub fts_available: bool,
    pub embedding_enabled: bool,
    pub llm_configured: bool,
    pub storage_backend: &'static str,
    pub coordination_backend: &'static str,
    pub vector_index: String,
}

pub struct Services {
    pub memory: Arc<MemoryService>,
    pub recall: Arc<RecallService>,
    pub session: Arc<SessionService>,
    pub pipeline: Arc<PipelineService>,
    pub prompts: Arc<MemoryPromptService>,
    pub generation_logs: Arc<GenerationLogService>,
    pub meta: Arc<MetaService>,
    /// Redaction rules: the per-department redactor and its admin API.
    pub redaction: Arc<RedactionService>,
    pub chat_memory: Arc<ChatMemoryService>,
    pub knowledge: Arc<KnowledgeService>,
    pub stats: Arc<StatsService>,
    pub skills: Arc<SkillService>,
    pub skill_extraction: Arc<SkillExtractionService>,
    pub lifecycle: Arc<dyn DepartmentLifecycle>,
    pub migration: Arc<LegacyScopeMigration>,
    pub upgrade: Arc<LegacyUpgrade>,
}

pub struct AppState {
    pub services: Services,
    pub settings: GatewaySettings,
    pub health: HealthFacts,
    pub started_at: Instant,
}

pub type SharedState = Arc<AppState>;
