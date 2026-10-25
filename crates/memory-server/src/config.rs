//! Configuration: a `memory-core.yaml` file
//! with `${VAR}` / `${VAR:-default}` expansion, overridden by `AGENT_MEMORY_*` env.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use memory_infrastructure::cli_llm::{codex_without_exec, granting_arg, refused_pass_env};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub server: ServerConfig,
    pub data: DataConfig,
    pub llm: LlmConfig,
    pub memory: MemoryConfig,
    pub skill: SkillConfig,
    pub metadata: MetadataConfig,
    pub worker: WorkerConfig,
    pub storage: StorageConfig,
    pub coordination: CoordinationConfig,
    pub redaction: RedactionConfig,
    /// Numeric `AGENT_MEMORY_*` variables that did not parse; reported at
    /// startup instead of silently falling back to the config file.
    #[serde(skip)]
    pub env_errors: Vec<String>,
}

/// Secrets redacted before anything is stored or distilled (#57). The
/// built-in rules always apply; these settings add to them.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RedactionConfig {
    /// Also redact strings shaped like a password (8+ ASCII characters
    /// mixing letters, digits and a password symbol). On by default.
    pub password_shape: bool,
    /// Extra patterns, e.g. national ids or phone numbers. A `keep` group
    /// keeps that part of the match.
    pub patterns: Vec<RedactionPattern>,
    /// `AGENT_MEMORY_REDACTION_PATTERNS` that is not a JSON list of
    /// patterns; reported by validation.
    #[serde(skip)]
    pub env_error: Option<String>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            password_shape: true,
            patterns: Vec::new(),
            env_error: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RedactionPattern {
    pub kind: String,
    pub pattern: String,
}

impl RedactionConfig {
    /// These settings as the deployment layer of the rules, checked.
    pub fn deployment_rules(&self) -> Result<memory_application::redaction::DeploymentRules> {
        self.redactor()?;
        Ok(memory_application::redaction::DeploymentRules {
            password_shape: self.password_shape,
            patterns: self
                .patterns
                .iter()
                .map(|p| (p.kind.clone(), p.pattern.clone()))
                .collect(),
        })
    }

    /// The redactor these settings describe; fails on a pattern that does
    /// not compile.
    pub fn redactor(&self) -> Result<memory_redact::Redactor> {
        if let Some(e) = &self.env_error {
            bail!("AGENT_MEMORY_REDACTION_PATTERNS must be a JSON list of {{\"kind\", \"pattern\"}}: {e}");
        }
        let extra: Vec<(String, String)> = self
            .patterns
            .iter()
            .map(|p| (p.kind.clone(), p.pattern.clone()))
            .collect();
        memory_redact::Redactor::new(&extra, self.password_shape)
            .map_err(|e| anyhow::anyhow!("redaction.patterns: {e}"))
    }
}

/// Where memory, metadata and skills are persisted.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StorageConfig {
    /// `sqlite` (one file per department) or `postgres` (pgvector).
    pub backend: String,
    pub postgres: PostgresConfig,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: "sqlite".into(),
            postgres: PostgresConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PostgresConfig {
    pub url: String,
    pub max_connections: u32,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            max_connections: 10,
        }
    }
}

/// Where pipeline timers, the job queue and locks live.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CoordinationConfig {
    /// `local` (single node) or `redis` (several nodes share work).
    pub backend: String,
    pub redis: RedisConfig,
}

impl Default for CoordinationConfig {
    fn default() -> Self {
        Self {
            backend: "local".into(),
            redis: RedisConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RedisConfig {
    pub url: String,
    pub key_prefix: String,
    /// How long a job/state lock lives without renewal.
    pub lock_ttl_ms: u64,
    /// How long a popped job may go without an ack or lease extension
    /// before another node redelivers it (the node is presumed dead).
    pub queue_lease_ms: u64,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            key_prefix: "agent_memory".into(),
            lock_ttl_ms: 600_000,
            queue_lease_ms: 900_000,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub api_key: Option<String>,
    pub cors_origins: Vec<String>,
    pub max_body_bytes: usize,
    pub max_session_body_bytes: usize,
    /// Proxies whose `X-Forwarded-For` names the client: addresses, CIDR
    /// ranges or host names (resolved, e.g. the Compose service
    /// `memory-panel`). Anyone else is the client themselves.
    pub trusted_proxies: Vec<String>,
    /// Days the credential audit log keeps an event.
    pub audit_retention_days: u32,
    /// How refused credentials are answered, per client address.
    pub throttle: ThrottleConfig,
    /// Lets `internal/upgrade` issue a user key without moving legacy data:
    /// recovery of a lost administrator key. Off by default.
    pub allow_key_recovery: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8420,
            api_key: None,
            cors_origins: Vec::new(),
            max_body_bytes: 1024 * 1024,
            max_session_body_bytes: 32 * 1024 * 1024,
            trusted_proxies: Vec::new(),
            audit_retention_days: 90,
            throttle: ThrottleConfig::default(),
            allow_key_recovery: false,
        }
    }
}

/// From the `delayAfter`-th failure within `windowMinutes`, each refusal
/// waits longer (250 ms, doubling up to 5 s); the `lockAfter`-th refuses
/// the address outright for `lockMinutes`. 0 turns delays or locks off,
/// e.g. where every client shows the same address.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThrottleConfig {
    pub window_minutes: u64,
    pub delay_after: u32,
    pub lock_after: u32,
    pub lock_minutes: u64,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        Self {
            window_minutes: 15,
            delay_after: 5,
            lock_after: 20,
            lock_minutes: 15,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DataConfig {
    pub base_dir: String,
    pub max_open_departments: usize,
}

impl Default for DataConfig {
    fn default() -> Self {
        Self {
            base_dir: "~/.agent-memory".into(),
            max_open_departments: 100,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LlmConfig {
    /// `openai` (any OpenAI-compatible HTTP API) or `cli` (a local tool).
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub timeout_ms: u64,
    pub cli: CliLlmConfig,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            cli: CliLlmConfig::default(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            model: "gpt-4o".into(),
            max_tokens: 8192,
            timeout_ms: 180_000,
        }
    }
}

/// A local command-line tool that answers prompts, run in a project.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CliLlmConfig {
    pub command: String,
    pub args: Vec<String>,
    /// `stdin` (default) or `arg` (substitutes `{prompt}` in `args`).
    pub prompt_mode: String,
    /// Project base path used as the tool's working directory.
    pub working_dir: Option<String>,
    /// Set for the tool. It inherits only the basics (PATH, HOME, locale,
    /// certificates, proxies) and a known CLI's own settings.
    pub env: std::collections::BTreeMap<String, String>,
    /// More variables of memory-core's environment the tool may inherit:
    /// names, or `PREFIX_*`. Never `AGENT_MEMORY_*`.
    pub pass_env: Vec<String>,
    pub max_concurrency: usize,
    /// `auto` (default) turns off a known CLI's own tools before it sees
    /// conversation text; `off` passes `args` unchanged (#37).
    pub hardening: String,
}

impl Default for CliLlmConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            prompt_mode: "stdin".into(),
            working_dir: None,
            env: Default::default(),
            pass_env: Vec::new(),
            max_concurrency: 2,
            hardening: "auto".into(),
        }
    }
}

/// Parses a list from the environment: a JSON array, or whitespace-separated.
pub fn parse_arg_list(raw: &str) -> Vec<String> {
    if raw.trim_start().starts_with('[') {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(raw) {
            return list;
        }
    }
    raw.split_whitespace().map(str::to_owned).collect()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MemoryConfig {
    pub extraction: ExtractionConfig,
    pub persona: PersonaConfig,
    pub pipeline: PipelineConfig,
    pub recall: RecallConfig,
    pub embedding: EmbeddingConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExtractionConfig {
    pub enabled: bool,
    pub enable_dedup: bool,
    pub max_memories_per_session: usize,
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            enable_dedup: true,
            max_memories_per_session: 20,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PersonaConfig {
    pub trigger_every_n: u32,
    pub max_scenes: usize,
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            trigger_every_n: 50,
            max_scenes: 15,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PipelineConfig {
    pub every_n_conversations: u32,
    pub enable_warmup: bool,
    pub l1_idle_timeout_seconds: u64,
    pub l2_delay_after_l1_seconds: u64,
    pub l2_min_interval_seconds: u64,
    pub l2_max_interval_seconds: u64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            every_n_conversations: 5,
            enable_warmup: true,
            l1_idle_timeout_seconds: 600,
            l2_delay_after_l1_seconds: 90,
            l2_min_interval_seconds: 900,
            l2_max_interval_seconds: 3600,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RecallConfig {
    pub max_results: usize,
    pub score_threshold: f64,
    pub strategy: String,
    pub timeout_ms: u64,
    pub max_chars_per_memory: usize,
    pub max_total_recall_chars: usize,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            max_results: 5,
            score_threshold: 0.3,
            strategy: "hybrid".into(),
            timeout_ms: 5000,
            max_chars_per_memory: 0,
            max_total_recall_chars: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EmbeddingConfig {
    /// `none` (BM25 only) or any OpenAI-compatible provider name.
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub dimensions: usize,
    /// Per request to the embedding service.
    pub timeout_ms: u64,
    /// Requests to the embedding service in flight at once.
    pub max_concurrency: usize,
    /// How often atoms stored without an embedding get one (0 = never;
    /// `memory-server reembed` still works).
    pub backfill_interval_secs: u64,
    /// Atoms embedded per department per backfill run, at most.
    pub backfill_batch: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            dimensions: 0,
            timeout_ms: 10_000,
            max_concurrency: 4,
            backfill_interval_secs: 600,
            backfill_batch: 200,
        }
    }
}

impl EmbeddingConfig {
    pub fn is_enabled(&self) -> bool {
        self.provider != "none"
            && !self.base_url.is_empty()
            && !self.api_key.is_empty()
            && !self.model.is_empty()
            && self.dimensions > 0
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SkillConfig {
    pub enabled: bool,
    pub routing: SkillRoutingConfig,
    pub extraction: SkillExtractionConfig,
    pub version_ttl_days: i64,
    pub max_versions: usize,
}

impl Default for SkillConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            routing: SkillRoutingConfig::default(),
            extraction: SkillExtractionConfig::default(),
            version_ttl_days: 0,
            max_versions: 100,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SkillRoutingConfig {
    pub search_top_k: usize,
}

impl Default for SkillRoutingConfig {
    fn default() -> Self {
        Self { search_top_k: 20 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SkillExtractionConfig {
    pub enabled: bool,
    pub archive_bytes: usize,
    pub tool_call_threshold: u32,
    /// Extracted changes wait for approval (`skill/proposal/*`, the Panel's
    /// Skills page); `false` applies them at once.
    pub require_approval: bool,
    /// Undecided proposals expire after this many days (0: never).
    pub proposal_ttl_days: u64,
}

impl Default for SkillExtractionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            archive_bytes: 40 * 1024,
            tool_call_threshold: 10,
            require_approval: true,
            proposal_ttl_days: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MetadataConfig {
    pub max_users_per_department: usize,
    pub max_teams_per_department: usize,
    pub max_active_keys_per_user: usize,
}

impl Default for MetadataConfig {
    fn default() -> Self {
        Self {
            max_users_per_department: 500,
            max_teams_per_department: 100,
            max_active_keys_per_user: 20,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkerConfig {
    pub concurrency: usize,
    pub max_retries: u32,
    pub scan_interval_ms: u64,
    /// Dead letters older than this (since they first died) are removed.
    pub dead_letter_ttl_hours: u64,
    /// A dead letter redriven this often is removed instead.
    pub dead_letter_max_redrives: u32,
    /// Wait before a dead letter is redriven on its own; doubled each time.
    pub dead_letter_redrive_after_minutes: u64,
    pub dead_letter_sweep_seconds: u64,
    /// Dead letters requeued per sweep, at most.
    pub dead_letter_redrive_batch: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            concurrency: 8,
            max_retries: 3,
            scan_interval_ms: 2000,
            dead_letter_ttl_hours: 24,
            dead_letter_max_redrives: 3,
            dead_letter_redrive_after_minutes: 60,
            dead_letter_sweep_seconds: 300,
            dead_letter_redrive_batch: 10,
        }
    }
}

/// Expands `${VAR}` and `${VAR:-default}` using `lookup`.
pub fn expand_vars(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let expr = &rest[start + 2..start + end];
        let (name, default) = expr
            .split_once(":-")
            .map_or((expr, None), |(n, d)| (n, Some(d)));
        out.push_str(
            &lookup(name)
                .filter(|v| !v.is_empty())
                .or(default.map(str::to_owned))
                .unwrap_or_default(),
        );
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// A numeric variable; one that does not parse is recorded in `errors`.
fn env_number<T: std::str::FromStr>(name: &str, errors: &mut Vec<String>) -> Option<T> {
    let raw = env(name)?;
    match raw.trim().parse() {
        Ok(v) => Some(v),
        Err(_) => {
            errors.push(format!("{name}={raw:?} is not a number"));
            None
        }
    }
}

fn find_config_file() -> Option<PathBuf> {
    if let Some(path) = env("AGENT_MEMORY_CONFIG") {
        return Some(PathBuf::from(path));
    }
    ["memory-core.yaml", "memory-core.yml"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => env("HOME")
            .map(|h| Path::new(&h).join(rest))
            .unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    }
}

/// Settings renamed in 0.3.0. Unknown keys are otherwise ignored, so an old
/// file would silently fall back to the defaults.
const RENAMED_KEYS: [(&str, &str); 3] = [
    ("maxOpenInstances", "maxOpenDepartments"),
    ("maxUsersPerInstance", "maxUsersPerDepartment"),
    ("maxTeamsPerInstance", "maxTeamsPerDepartment"),
];

fn reject_renamed_keys(raw: &str) -> Result<()> {
    fn walk(v: &serde_yaml::Value, found: &mut Vec<String>) {
        match v {
            serde_yaml::Value::Mapping(map) => {
                for (k, child) in map {
                    if let Some((old, new)) = k
                        .as_str()
                        .and_then(|k| RENAMED_KEYS.iter().find(|(old, _)| *old == k))
                    {
                        found.push(format!("`{old}` is now `{new}`"));
                    }
                    walk(child, found);
                }
            }
            serde_yaml::Value::Sequence(items) => items.iter().for_each(|i| walk(i, found)),
            _ => {}
        }
    }
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(raw) else {
        return Ok(()); // reported by the real parse
    };
    let mut found = Vec::new();
    walk(&value, &mut found);
    if found.is_empty() {
        Ok(())
    } else {
        bail!("renamed settings: {}", found.join(", "))
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let mut config = match find_config_file() {
            Some(path) => {
                let raw = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                let raw = expand_vars(&raw, env);
                reject_renamed_keys(&raw).with_context(|| format!("parsing {}", path.display()))?;
                serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?
            }
            None => Config::default(),
        };
        config.apply_env();
        config.validate()?;
        Ok(config)
    }

    fn apply_env(&mut self) {
        let set = |target: &mut String, name: &str| {
            if let Some(v) = env(name) {
                *target = v;
            }
        };
        set(&mut self.server.host, "AGENT_MEMORY_HOST");
        if let Some(port) = env_number("AGENT_MEMORY_PORT", &mut self.env_errors) {
            self.server.port = port;
        }
        if let Some(key) = env("AGENT_MEMORY_API_KEY") {
            self.server.api_key = Some(key);
        }
        if let Some(origins) = env("AGENT_MEMORY_CORS_ORIGINS") {
            self.server.cors_origins = origins
                .split(',')
                .map(|o| o.trim().to_owned())
                .filter(|o| !o.is_empty())
                .collect();
        }
        if let Some(proxies) = env("AGENT_MEMORY_TRUSTED_PROXIES") {
            self.server.trusted_proxies = proxies
                .split(',')
                .map(|p| p.trim().to_owned())
                .filter(|p| !p.is_empty())
                .collect();
        }
        if let Some(days) = env_number("AGENT_MEMORY_AUDIT_RETENTION_DAYS", &mut self.env_errors) {
            self.server.audit_retention_days = days;
        }
        if let Some(n) = env_number("AGENT_MEMORY_THROTTLE_DELAY_AFTER", &mut self.env_errors) {
            self.server.throttle.delay_after = n;
        }
        if let Some(n) = env_number("AGENT_MEMORY_THROTTLE_LOCK_AFTER", &mut self.env_errors) {
            self.server.throttle.lock_after = n;
        }
        if let Some(n) = env_number("AGENT_MEMORY_THROTTLE_LOCK_MINUTES", &mut self.env_errors) {
            self.server.throttle.lock_minutes = n;
        }
        if let Some(allow) = env("AGENT_MEMORY_ALLOW_KEY_RECOVERY") {
            self.server.allow_key_recovery = matches!(allow.as_str(), "1" | "true" | "on" | "yes");
        }
        set(&mut self.data.base_dir, "AGENT_MEMORY_DATA_DIR");
        set(&mut self.llm.api_key, "AGENT_MEMORY_LLM_API_KEY");
        set(&mut self.llm.base_url, "AGENT_MEMORY_LLM_BASE_URL");
        set(&mut self.llm.model, "AGENT_MEMORY_LLM_MODEL");
        set(&mut self.llm.provider, "AGENT_MEMORY_LLM_PROVIDER");
        set(&mut self.storage.backend, "AGENT_MEMORY_STORAGE_BACKEND");
        set(&mut self.storage.postgres.url, "AGENT_MEMORY_POSTGRES_URL");
        set(
            &mut self.coordination.backend,
            "AGENT_MEMORY_COORDINATION_BACKEND",
        );
        set(&mut self.coordination.redis.url, "AGENT_MEMORY_REDIS_URL");
        set(&mut self.llm.cli.command, "AGENT_MEMORY_LLM_CLI_COMMAND");
        set(
            &mut self.llm.cli.prompt_mode,
            "AGENT_MEMORY_LLM_CLI_PROMPT_MODE",
        );
        if let Some(args) = env("AGENT_MEMORY_LLM_CLI_ARGS") {
            self.llm.cli.args = parse_arg_list(&args);
        }
        set(
            &mut self.llm.cli.hardening,
            "AGENT_MEMORY_LLM_CLI_HARDENING",
        );
        if let Some(dir) = env("AGENT_MEMORY_LLM_CLI_WORKDIR") {
            self.llm.cli.working_dir = Some(dir);
        }
        if let Some(names) = env("AGENT_MEMORY_LLM_CLI_PASS_ENV") {
            self.llm.cli.pass_env = names
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
                .collect();
        }
        let e = &mut self.memory.embedding;
        set(&mut e.provider, "AGENT_MEMORY_EMBEDDING_PROVIDER");
        set(&mut e.base_url, "AGENT_MEMORY_EMBEDDING_BASE_URL");
        set(&mut e.api_key, "AGENT_MEMORY_EMBEDDING_API_KEY");
        set(&mut e.model, "AGENT_MEMORY_EMBEDDING_MODEL");
        if let Some(d) = env_number("AGENT_MEMORY_EMBEDDING_DIMENSIONS", &mut self.env_errors) {
            e.dimensions = d;
        }
        if let Some(ms) = env_number("AGENT_MEMORY_EMBEDDING_TIMEOUT_MS", &mut self.env_errors) {
            e.timeout_ms = ms;
        }
        if let Some(n) = env_number(
            "AGENT_MEMORY_EMBEDDING_MAX_CONCURRENCY",
            &mut self.env_errors,
        ) {
            e.max_concurrency = n;
        }
        if let Some(secs) = env_number(
            "AGENT_MEMORY_EMBEDDING_BACKFILL_INTERVAL_SECS",
            &mut self.env_errors,
        ) {
            e.backfill_interval_secs = secs;
        }
        if let Some(ms) = env_number(
            "AGENT_MEMORY_DISTILLATION_LLM_TIMEOUT_MS",
            &mut self.env_errors,
        ) {
            self.llm.timeout_ms = ms;
        }
        if let Some(n) = env_number("AGENT_MEMORY_WORKER_CONCURRENCY", &mut self.env_errors) {
            self.worker.concurrency = n;
        }
        if let Some(n) = env_number("AGENT_MEMORY_LLM_CLI_MAX_CONCURRENCY", &mut self.env_errors) {
            self.llm.cli.max_concurrency = n;
        }
        if let Some(on) = env("AGENT_MEMORY_REDACTION_PASSWORD_SHAPE") {
            self.redaction.password_shape = matches!(on.as_str(), "1" | "true" | "on" | "yes");
        }
        if let Some(json) = env("AGENT_MEMORY_REDACTION_PATTERNS") {
            match serde_json::from_str::<Vec<RedactionPattern>>(&json) {
                Ok(patterns) => self.redaction.patterns = patterns,
                Err(e) => self.redaction.env_error = Some(e.to_string()),
            }
        }
        if let Some(enabled) = env("AGENT_MEMORY_SKILL_ENABLED") {
            self.skill.enabled = matches!(enabled.as_str(), "1" | "true" | "on" | "yes");
        }
        if let Some(days) = env_number("AGENT_MEMORY_SKILL_PROPOSAL_TTL_DAYS", &mut self.env_errors)
        {
            self.skill.extraction.proposal_ttl_days = days;
        }
        if let Some(required) = env("AGENT_MEMORY_SKILL_REQUIRE_APPROVAL") {
            self.skill.extraction.require_approval =
                !matches!(required.as_str(), "0" | "false" | "off" | "no");
        }
        if self.server.api_key.as_deref().is_some_and(str::is_empty) {
            self.server.api_key = None;
        }
    }

    /// Exposing the API beyond loopback without a key would leak every
    /// tenant's memory.
    fn validate(&self) -> Result<()> {
        if !self.env_errors.is_empty() {
            bail!("invalid environment: {}", self.env_errors.join("; "));
        }
        let loopback = matches!(self.server.host.as_str(), "127.0.0.1" | "localhost" | "::1");
        if !loopback && self.server.api_key.is_none() {
            bail!(
                "AGENT_MEMORY_API_KEY is required when binding to {}",
                self.server.host
            );
        }
        self.validate_llm()?;
        self.redaction.redactor()?;
        self.validate_backends()
    }

    fn validate_backends(&self) -> Result<()> {
        match self.storage.backend.as_str() {
            "sqlite" => {}
            "postgres" if self.storage.postgres.url.is_empty() => {
                bail!("storage.postgres.url (AGENT_MEMORY_POSTGRES_URL) is required for backend `postgres`")
            }
            "postgres" => {}
            other => bail!("storage.backend must be `sqlite` or `postgres`, got `{other}`"),
        }
        match self.coordination.backend.as_str() {
            "local" => {}
            "redis" if self.coordination.redis.url.is_empty() => {
                bail!("coordination.redis.url (AGENT_MEMORY_REDIS_URL) is required for backend `redis`")
            }
            "redis" => {}
            other => bail!("coordination.backend must be `local` or `redis`, got `{other}`"),
        }
        Ok(())
    }

    fn validate_llm(&self) -> Result<()> {
        match self.llm.provider.as_str() {
            "openai" => Ok(()),
            "cli" => {
                let cli = &self.llm.cli;
                if cli.command.trim().is_empty() {
                    bail!("llm.cli.command (AGENT_MEMORY_LLM_CLI_COMMAND) is required for provider `cli`");
                }
                if !matches!(cli.prompt_mode.as_str(), "stdin" | "arg") {
                    bail!(
                        "llm.cli.promptMode must be `stdin` or `arg`, got `{}`",
                        cli.prompt_mode
                    );
                }
                if !matches!(cli.hardening.as_str(), "auto" | "off") {
                    bail!(
                        "llm.cli.hardening must be `auto` or `off`, got `{}`",
                        cli.hardening
                    );
                }
                if let Some(arg) = granting_arg(&cli.command, &cli.args) {
                    bail!(
                        "llm.cli.args must not give the distillation tool more capability: remove `{arg}`. \
                         Distillation reads untrusted conversation text, so it never gets tools or \
                         skipped permission checks (#37)"
                    );
                }
                if cli.hardening == "auto" && codex_without_exec(&cli.command, &cli.args) {
                    bail!(
                        "llm.cli.args for codex must run `exec`: only `codex exec --ignore-user-config` keeps \
                         config.toml (MCP servers, profiles) away from distillation (#37)"
                    );
                }
                if let Some((entry, why)) = cli
                    .pass_env
                    .iter()
                    .find_map(|e| refused_pass_env(e).map(|why| (e, why)))
                {
                    bail!(
                        "llm.cli.passEnv (AGENT_MEMORY_LLM_CLI_PASS_ENV) must not list `{entry}`: {why}. \
                         Distillation reads untrusted conversation text, so the tool never sees \
                         memory-core's own settings"
                    );
                }
                if let Some(name) = cli.env.keys().find(|k| k.starts_with("AGENT_MEMORY_")) {
                    bail!(
                        "llm.cli.env must not set `{name}`: memory-core's own AGENT_MEMORY_* \
                         variables stay with memory-core, as with llm.cli.passEnv"
                    );
                }
                if let Some(dir) = &cli.working_dir {
                    let path = expand_home(dir);
                    if !path.is_dir() {
                        bail!("llm.cli.workingDir does not exist: {}", path.display());
                    }
                }
                Ok(())
            }
            other => bail!("llm.provider must be `openai` or `cli`, got `{other}`"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_renamed_in_0_3_are_rejected_by_name() {
        let err = reject_renamed_keys("data:\n  maxOpenInstances: 10\n").unwrap_err();
        assert!(err
            .to_string()
            .contains("`maxOpenInstances` is now `maxOpenDepartments`"));
        reject_renamed_keys("data:\n  maxOpenDepartments: 10\n").unwrap();
    }

    #[test]
    fn expands_variables_with_defaults() {
        let lookup = |k: &str| (k == "SET").then(|| "v".to_owned());
        assert_eq!(
            expand_vars("a=${SET} b=${UNSET:-d} c=${UNSET}", lookup),
            "a=v b=d c="
        );
    }

    #[test]
    fn reference_yaml_keys_parse() {
        let yaml = "server:\n  port: 9000\nllm:\n  model: m\nmemory:\n  pipeline:\n    everyNConversations: 3\n  embedding:\n    provider: none\n";
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.server.port, 9000);
        assert_eq!(c.memory.pipeline.every_n_conversations, 3);
        assert!(!c.memory.embedding.is_enabled());
    }

    #[test]
    fn example_config_parses() {
        let raw = include_str!("../../../config/memory-core.example.yaml");
        let c: Config = serde_yaml::from_str(&expand_vars(raw, |_| None)).unwrap();
        assert_eq!(c.llm.provider, "openai");
        assert_eq!(c.llm.cli.prompt_mode, "stdin");
        assert!(c.validate().is_ok());
    }

    #[test]
    fn the_distillation_llm_timeout_comes_from_its_variable() {
        // No other test reads this variable.
        std::env::set_var("AGENT_MEMORY_DISTILLATION_LLM_TIMEOUT_MS", "720000");
        let mut c = Config::default();
        c.apply_env();
        std::env::remove_var("AGENT_MEMORY_DISTILLATION_LLM_TIMEOUT_MS");
        assert_eq!(c.llm.timeout_ms, 720_000);
        assert!(c.env_errors.is_empty(), "{:?}", c.env_errors);
    }

    #[test]
    fn a_number_variable_that_does_not_parse_is_reported() {
        // Names no other test uses.
        std::env::set_var("AGENT_MEMORY_TEST_NUMBER_BAD", "abc");
        std::env::set_var("AGENT_MEMORY_TEST_NUMBER_OK", " 7 ");
        let mut errors = Vec::new();
        assert_eq!(
            env_number::<usize>("AGENT_MEMORY_TEST_NUMBER_BAD", &mut errors),
            None
        );
        assert_eq!(
            env_number::<usize>("AGENT_MEMORY_TEST_NUMBER_OK", &mut errors),
            Some(7)
        );
        assert_eq!(
            env_number::<usize>("AGENT_MEMORY_TEST_NUMBER_UNSET", &mut errors),
            None
        );
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains("AGENT_MEMORY_TEST_NUMBER_BAD"),
            "{errors:?}"
        );
        let c = Config {
            env_errors: errors,
            ..Config::default()
        };
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("invalid environment"), "{err}");
    }

    #[test]
    fn embedding_limits_are_read_from_the_config_file() {
        let d = EmbeddingConfig::default();
        assert_eq!(
            (d.timeout_ms, d.max_concurrency, d.backfill_interval_secs),
            (10_000, 4, 600)
        );
        let yaml = "memory:\n  embedding:\n    timeoutMs: 30000\n    maxConcurrency: 2\n    backfillIntervalSecs: 0\n    backfillBatch: 50\n";
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        let e = &c.memory.embedding;
        assert_eq!(
            (
                e.timeout_ms,
                e.max_concurrency,
                e.backfill_interval_secs,
                e.backfill_batch
            ),
            (30_000, 2, 0, 50)
        );
    }

    #[test]
    fn redaction_patterns_are_checked_at_startup() {
        let yaml = "redaction:\n  passwordShape: false\n  patterns:\n    - kind: tw-national-id\n      pattern: '\\b[A-Z][12]\\d{8}\\b'\n";
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(!c.redaction.password_shape);
        let r = c.redaction.redactor().unwrap();
        assert_eq!(r.redact("A123456789"), "[REDACTED:tw-national-id]");
        assert!(c.validate().is_ok());

        let mut bad = Config::default();
        bad.redaction.patterns.push(RedactionPattern {
            kind: "broken".into(),
            pattern: "(".into(),
        });
        let err = bad.validate().unwrap_err().to_string();
        assert!(
            err.contains("redaction.patterns") && err.contains("broken"),
            "{err}"
        );

        let mut kind = Config::default();
        kind.redaction.patterns.push(RedactionPattern {
            kind: "National ID".into(),
            pattern: r"\b[A-Z]\d{9}\b".into(),
        });
        let err = kind.validate().unwrap_err().to_string();
        assert!(err.contains("National ID"), "{err}");

        let mut env = Config::default();
        env.redaction.env_error = Some("expected value".into());
        let err = env.validate().unwrap_err().to_string();
        assert!(err.contains("AGENT_MEMORY_REDACTION_PATTERNS"), "{err}");
    }

    #[test]
    fn backends_require_their_urls() {
        let mut c = Config::default();
        c.storage.backend = "postgres".into();
        assert!(c.validate().is_err());
        c.storage.postgres.url = "postgres://localhost/memory".into();
        c.coordination.backend = "redis".into();
        assert!(c.validate().is_err());
        c.coordination.redis.url = "redis://localhost".into();
        assert!(c.validate().is_ok());
        c.storage.backend = "oracle".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn cli_provider_is_validated() {
        let mut c = Config::default();
        c.llm.provider = "cli".into();
        assert!(c.validate().is_err(), "command is required");
        c.llm.cli.command = "claude".into();
        assert!(c.validate().is_ok());
        c.llm.cli.working_dir = Some("/definitely/missing/dir".into());
        assert!(c.validate().is_err());
        c.llm.provider = "other".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn cli_args_that_grant_capability_are_rejected_by_name() {
        let mut c = Config::default();
        c.llm.provider = "cli".into();
        c.llm.cli.command = "/usr/local/bin/codex".into();
        c.llm.cli.args = parse_arg_list("exec -s read-only");
        assert!(c.validate().is_ok());
        c.llm.cli.args = parse_arg_list("--model o4");
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("must run `exec`"), "{err}");
        for args in [
            "exec --dangerously-bypass-approvals-and-sandbox",
            "exec -s workspace-write",
            "exec --sandbox=danger-full-access",
            "--enable apps exec",
        ] {
            c.llm.cli.args = parse_arg_list(args);
            let err = c.validate().unwrap_err().to_string();
            assert!(err.contains("#37"), "{args}: {err}");
        }
        c.llm.cli.command = "claude".into();
        for args in [
            "-p --dangerously-skip-permissions",
            "-p --permission-mode bypassPermissions",
            "-p --allowedTools Bash",
            "-p --tools Bash,Read",
            "-p --mcp-config servers.json",
        ] {
            c.llm.cli.args = parse_arg_list(args);
            let err = c.validate().unwrap_err().to_string();
            assert!(err.contains("remove `"), "{args}: {err}");
        }
        c.llm.cli.args = parse_arg_list("-p --permission-mode plan");
        assert!(c.validate().is_ok());
        c.llm.cli.hardening = "off".into();
        assert!(c.validate().is_ok(), "off still allows plain args");
        c.llm.cli.args = parse_arg_list("-p --dangerously-skip-permissions");
        assert!(c.validate().is_err(), "off never allows granting args");
        c.llm.cli.hardening = "sometimes".into();
        c.llm.cli.args.clear();
        assert!(c.validate().is_err());
    }

    #[test]
    fn pass_env_never_reaches_memory_cores_own_variables() {
        let mut c = Config::default();
        c.llm.provider = "cli".into();
        c.llm.cli.command = "my-llm".into();
        c.llm.cli.pass_env = vec!["MY_LLM_TOKEN".into(), "MY_LLM_*".into()];
        assert!(c.validate().is_ok());
        for entry in ["AGENT_MEMORY_API_KEY", "AGENT_*", "*"] {
            c.llm.cli.pass_env = vec![entry.into()];
            let err = c.validate().unwrap_err().to_string();
            assert!(err.contains("llm.cli.passEnv"), "{entry}: {err}");
        }
        c.llm.cli.pass_env.clear();
        c.llm.cli.env.insert("MODEL".into(), "small".into());
        assert!(c.validate().is_ok());
        c.llm
            .cli
            .env
            .insert("AGENT_MEMORY_API_KEY".into(), "x".into());
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("llm.cli.env"), "{err}");
    }

    #[test]
    fn arg_lists_parse_from_json_or_words() {
        assert_eq!(
            parse_arg_list(r#"["-p", "--model", "a b"]"#),
            ["-p", "--model", "a b"]
        );
        assert_eq!(
            parse_arg_list("-p  --output-format text"),
            ["-p", "--output-format", "text"]
        );
    }

    #[test]
    fn public_bind_requires_api_key() {
        let mut c = Config::default();
        c.server.host = "0.0.0.0".into();
        assert!(c.validate().is_err());
        c.server.api_key = Some("k".into());
        assert!(c.validate().is_ok());
    }
}
