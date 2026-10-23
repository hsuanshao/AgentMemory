//! Composition root: the only place that knows every concrete type.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use memory_application::admin::{
    ChatMemoryService, DepartmentLifecycle, KnowledgeService, StatsService,
};
use memory_application::memory::recall::{RecallService, RecallSettings};
use memory_application::memory::search::AtomSearcher;
use memory_application::memory::service::{ConversationObserver, MemoryService};
use memory_application::memory::session::SessionService;
use memory_application::meta::{ChatMemoryCleaner, MetaService, MetaSettings};
use memory_application::migration::LegacyScopeMigration;
use memory_application::pipeline::generation_logs::GenerationLogService;
use memory_application::pipeline::l1::L1Extractor;
use memory_application::pipeline::l2::SceneExtractor;
use memory_application::pipeline::l3::PersonaGenerator;
use memory_application::pipeline::memory_prompts::MemoryPromptService;
use memory_application::pipeline::metrics::PipelineMetrics;
use memory_application::pipeline::service::{PipelineDeps, PipelineService, SkillJobRunner};
use memory_application::ports::meta::KnowledgeRepository;
use memory_application::ports::pipeline::DeadLetterPolicy;
use memory_application::ports::system::{Clock, EmbeddingProvider, IdGenerator, LlmClient};
use memory_application::skill::extraction::{ExtractionSettings, SkillExtractionService};
use memory_application::skill::service::{SkillService, SkillSettings};
use memory_application::upgrade::LegacyUpgrade;
use memory_domain::pipeline::PipelineSettings;
use memory_domain::retrieval::SearchMode;
use memory_gateway::{AppState, GatewaySettings, HealthFacts, Services};
use memory_infrastructure::cli_llm::{harden, CliLlm, CliLlmSettings, Hardened, PromptMode};
use memory_infrastructure::fs::LocalFileStore;
use memory_infrastructure::lifecycle::{CompositeLifecycle, StateCleanup};
use memory_infrastructure::openai::{
    EmbeddingSettings, LlmSettings, NoEmbedding, OpenAiEmbedding, OpenAiLlm,
};
use memory_infrastructure::system::{RandomIds, SystemClock};
use memory_infrastructure::worker::{
    spawn_dead_letter_sweeper, spawn_scanner, spawn_worker, WorkerQueue, WorkerSettings,
};
use tokio::task::JoinHandle;

use crate::backends::{build_coordination, build_storage};
use crate::config::{expand_home, Config};

pub struct App {
    pub state: Arc<AppState>,
    pub background: Vec<JoinHandle<()>>,
}

fn llm(config: &Config) -> Arc<dyn LlmClient> {
    let timeout = Duration::from_millis(config.llm.timeout_ms);
    if config.llm.provider == "cli" {
        let cli = &config.llm.cli;
        return Arc::new(CliLlm::new(CliLlmSettings {
            command: cli.command.clone(),
            args: hardened_args(&cli.command, &cli.args, &cli.hardening),
            prompt_mode: if cli.prompt_mode == "arg" {
                PromptMode::Arg
            } else {
                PromptMode::Stdin
            },
            working_dir: cli.working_dir.as_deref().map(expand_home),
            env: cli.env.clone().into_iter().collect(),
            timeout,
            max_concurrency: cli.max_concurrency,
        }));
    }
    Arc::new(OpenAiLlm::new(LlmSettings {
        base_url: config.llm.base_url.clone(),
        api_key: config.llm.api_key.clone(),
        model: config.llm.model.clone(),
        max_tokens: config.llm.max_tokens,
        timeout,
    }))
}

/// The CLI's arguments with its own tools turned off first (#37), or as
/// configured when hardening is `off` or the CLI has no known way to do it.
fn hardened_args(command: &str, args: &[String], hardening: &str) -> Vec<String> {
    if hardening == "off" {
        tracing::warn!(
            command,
            "llm.cli.hardening is off: the distillation CLI keeps its own tools; a refused tool call drops the input"
        );
        return args.to_vec();
    }
    match harden(command, args) {
        Hardened::Full(hardened) => {
            tracing::info!(command, args = ?hardened, "distillation CLI runs with its tools turned off");
            hardened
        }
        Hardened::Partial(hardened, missing) => {
            tracing::warn!(command, args = ?hardened, missing, "distillation CLI runs with its tools only partly turned off");
            hardened
        }
        Hardened::Unknown => {
            tracing::warn!(
                command,
                "no known way to turn off this CLI's tools: a refused tool call drops the input (see docs/ops/DISTILLATION.md)"
            );
            args.to_vec()
        }
    }
}

fn embedder(config: &Config) -> Arc<dyn EmbeddingProvider> {
    let e = &config.memory.embedding;
    if !e.is_enabled() {
        return Arc::new(NoEmbedding);
    }
    Arc::new(OpenAiEmbedding::new(EmbeddingSettings {
        base_url: e.base_url.clone(),
        api_key: e.api_key.clone(),
        model: e.model.clone(),
        dimensions: e.dimensions,
        timeout: Duration::from_millis(e.timeout_ms),
    }))
}

/// The embedding dimension storage is built for (`None`: embeddings off).
pub fn vector_dims(config: &Config) -> Option<usize> {
    embedder(config).dimensions()
}

fn pipeline_settings(config: &Config) -> PipelineSettings {
    let p = &config.memory.pipeline;
    PipelineSettings {
        every_n_conversations: p.every_n_conversations.max(1),
        enable_warmup: p.enable_warmup,
        l1_idle_timeout_secs: p.l1_idle_timeout_seconds,
        l2_delay_after_l1_secs: p.l2_delay_after_l1_seconds,
        l2_min_interval_secs: p.l2_min_interval_seconds,
        l2_max_interval_secs: p.l2_max_interval_seconds,
        persona_trigger_every_n: config.memory.persona.trigger_every_n.max(1),
        max_scenes: config.memory.persona.max_scenes.max(1),
        max_memories_per_session: config.memory.extraction.max_memories_per_session,
    }
}

fn recall_settings(config: &Config) -> RecallSettings {
    let r = &config.memory.recall;
    RecallSettings {
        max_results: r.max_results.max(1),
        score_threshold: r.score_threshold,
        strategy: SearchMode::parse(&r.strategy).unwrap_or_default(),
        timeout: Duration::from_millis(r.timeout_ms),
        max_chars_per_memory: r.max_chars_per_memory,
        max_total_chars: r.max_total_recall_chars,
    }
}

pub async fn build(config: &Config) -> Result<App> {
    let data_dir = expand_home(&config.data.base_dir);
    std::fs::create_dir_all(&data_dir)?;
    memory_infrastructure::fs::migrate_data_dir(&data_dir)?;

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(RandomIds);
    // Every write path redacts with the same rules (#57).
    let redactor = Arc::new(config.redaction.redactor()?);
    let llm = llm(config);
    let embedder = embedder(config);

    let storage = build_storage(config, &data_dir, embedder.dimensions()).await?;
    let coordination = build_coordination(config, &data_dir).await?;
    let conversations = Arc::clone(&storage.conversations);
    let atoms = Arc::clone(&storage.atoms);
    let profiles = Arc::clone(&storage.profiles);
    let files = Arc::new(LocalFileStore::new(data_dir.clone()));
    let queue = WorkerQueue::new(Arc::clone(&coordination.queue));

    let searcher = Arc::new(AtomSearcher::new(Arc::clone(&atoms), Arc::clone(&embedder)));

    // MemoryService notifies the pipeline, which itself needs MemoryService;
    // a forwarding observer breaks the cycle.
    let observer = Arc::new(LateObserver::default());
    let memory = Arc::new(MemoryService::new(
        Arc::clone(&redactor),
        Arc::clone(&conversations),
        Arc::clone(&atoms),
        Arc::clone(&profiles),
        Arc::clone(&embedder),
        Arc::clone(&searcher),
        observer.clone(),
        Arc::clone(&clock),
        Arc::clone(&ids),
    ));
    let prompts = Arc::new(MemoryPromptService::new(
        Arc::clone(&storage.prompts),
        Arc::clone(&clock),
        Arc::clone(&ids),
    ));
    let generation_logs = Arc::new(GenerationLogService::new(
        Arc::clone(&storage.generation_logs),
        Arc::clone(&clock),
        Arc::clone(&ids),
    ));
    let settings = pipeline_settings(config);
    let l1 = L1Extractor::new(
        Arc::clone(&redactor),
        Arc::clone(&llm),
        Arc::clone(&atoms),
        Arc::clone(&embedder),
        Arc::clone(&searcher),
        Arc::clone(&clock),
        Arc::clone(&ids),
        config.memory.extraction.enable_dedup,
        settings.max_memories_per_session,
    );
    let pipeline = Arc::new(PipelineService::new(
        PipelineDeps {
            state: Arc::clone(&coordination.state),
            queue: queue.clone(),
            conversations: Arc::clone(&conversations),
            atoms: Arc::clone(&atoms),
            memory: Arc::clone(&memory),
            l1: Arc::new(l1),
            l2: Arc::new(SceneExtractor::new(
                Arc::clone(&llm),
                Arc::clone(&memory),
                Arc::clone(&clock),
                settings.max_scenes,
            )),
            l3: Arc::new(PersonaGenerator::new(
                Arc::clone(&llm),
                Arc::clone(&memory),
                Arc::clone(&clock),
            )),
            prompts: Arc::clone(&prompts),
            logs: Arc::clone(&generation_logs),
            llm: Arc::clone(&llm),
            clock: Arc::clone(&clock),
            metrics: Arc::new(PipelineMetrics::default()),
            locks: Arc::clone(&coordination.state_locks),
        },
        settings,
    ));
    if config.memory.extraction.enabled {
        observer.set(pipeline.clone());
    }

    let meta = Arc::new(MetaService::new(
        Arc::clone(&storage.meta),
        Arc::clone(&clock),
        Arc::clone(&ids),
        MetaSettings {
            max_users_per_department: config.metadata.max_users_per_department,
            max_teams_per_department: config.metadata.max_teams_per_department,
            max_active_keys_per_user: config.metadata.max_active_keys_per_user,
        },
    ));
    let chat_memory = Arc::new(ChatMemoryService::new(
        Arc::clone(&conversations),
        Arc::clone(&atoms),
        Arc::clone(&profiles),
        pipeline.clone(),
        meta.clone(),
    ));
    meta.register_chat_memory_cleaner(chat_memory.clone() as Arc<dyn ChatMemoryCleaner>);

    let skills = Arc::new(SkillService::new(
        Arc::clone(&redactor),
        Arc::clone(&storage.skills),
        files.clone(),
        meta.clone(),
        Arc::clone(&clock),
        Arc::clone(&ids),
        SkillSettings {
            version_ttl_days: config.skill.version_ttl_days,
            max_versions: config.skill.max_versions,
            listing_top_k: config.skill.routing.search_top_k,
        },
    ));
    let skill_extraction = Arc::new(SkillExtractionService::new(
        Arc::clone(&redactor),
        Arc::clone(&storage.skill_buffers),
        skills.clone(),
        pipeline.clone(),
        Arc::clone(&llm),
        Arc::clone(&clock),
        Arc::clone(&ids),
        ExtractionSettings {
            enabled: config.skill.enabled && config.skill.extraction.enabled,
            archive_bytes: config.skill.extraction.archive_bytes,
            tool_call_threshold: config.skill.extraction.tool_call_threshold,
            ..Default::default()
        },
    ));
    pipeline.register_skill_runner(skill_extraction.clone() as Arc<dyn SkillJobRunner>);

    let lifecycle: Arc<dyn DepartmentLifecycle> = Arc::new(CompositeLifecycle::new(vec![
        ("storage", Arc::clone(&storage.cleanup)),
        (
            "state",
            Arc::new(StateCleanup::new(Arc::clone(&coordination.state))),
        ),
    ]));
    let migration = Arc::new(LegacyScopeMigration::new(
        Arc::clone(&storage.meta),
        Arc::clone(&storage.profiles),
        Arc::clone(&storage.scope_migration),
        Arc::clone(&coordination.state),
    ));
    let upgrade = Arc::new(LegacyUpgrade::new(
        Arc::clone(&meta),
        Arc::clone(&migration),
    ));
    let services = Services {
        recall: Arc::new(RecallService::new(
            Arc::clone(&searcher),
            Arc::clone(&memory),
            recall_settings(config),
        )),
        session: Arc::new(SessionService::new(
            Arc::clone(&redactor),
            files,
            Arc::clone(&ids),
        )),
        stats: Arc::new(StatsService::new(
            Arc::clone(&conversations),
            Arc::clone(&atoms),
            Arc::clone(&profiles),
            Arc::clone(&storage.skills),
            Arc::clone(&storage.generation_logs),
        )),
        knowledge: Arc::new(KnowledgeService::new(
            Arc::clone(&storage.meta) as Arc<dyn KnowledgeRepository>,
            Arc::clone(&clock),
        )),
        memory,
        pipeline: pipeline.clone(),
        prompts,
        generation_logs,
        meta,
        chat_memory,
        skills,
        skill_extraction,
        lifecycle,
        migration,
        upgrade,
    };

    let w = &config.worker;
    let worker = WorkerSettings {
        concurrency: w.concurrency,
        max_retries: w.max_retries,
        scan_interval: Duration::from_millis(w.scan_interval_ms),
        dead_letters: DeadLetterPolicy {
            ttl_ms: (w.dead_letter_ttl_hours * 3_600_000) as i64,
            max_redrives: w.dead_letter_max_redrives,
            redrive_after_ms: (w.dead_letter_redrive_after_minutes * 60_000) as i64,
        },
        sweep_interval: Duration::from_secs(w.dead_letter_sweep_seconds.max(1)),
        redrive_batch: w.dead_letter_redrive_batch,
        ..Default::default()
    };
    let background = vec![
        spawn_worker(
            Arc::clone(&queue),
            Arc::clone(&coordination.job_locks),
            pipeline.clone(),
            worker,
        ),
        spawn_dead_letter_sweeper(queue, pipeline.clone(), worker),
        spawn_scanner(pipeline, worker.scan_interval),
    ];

    let state = Arc::new(AppState {
        health: HealthFacts {
            fts_available: storage.fts_available,
            embedding_enabled: embedder.dimensions().is_some(),
            llm_configured: llm.is_configured(),
            storage_backend: storage.name,
            coordination_backend: coordination.name,
            vector_index: storage.vector_index.clone(),
        },
        services,
        settings: GatewaySettings {
            api_key: config.server.api_key.clone(),
            cors_origins: config.server.cors_origins.clone(),
            max_body_bytes: config.server.max_body_bytes,
            max_session_body_bytes: config.server.max_session_body_bytes,
            version: env!("CARGO_PKG_VERSION").into(),
        },
        started_at: Instant::now(),
    });
    Ok(App { state, background })
}

/// Forwards conversation events to an observer registered after
/// construction; drops them while none is set (extraction disabled).
#[derive(Default)]
struct LateObserver {
    inner: std::sync::OnceLock<Arc<dyn ConversationObserver>>,
}

impl LateObserver {
    fn set(&self, observer: Arc<dyn ConversationObserver>) {
        let _ = self.inner.set(observer);
    }
}

#[async_trait::async_trait]
impl ConversationObserver for LateObserver {
    async fn on_conversation(
        &self,
        department: &memory_domain::scope::DepartmentId,
        scope: &memory_domain::scope::MemoryScope,
        session_id: &str,
        rounds: u32,
    ) {
        if let Some(o) = self.inner.get() {
            o.on_conversation(department, scope, session_id, rounds)
                .await;
        }
    }
}
