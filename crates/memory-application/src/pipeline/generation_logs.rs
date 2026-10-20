//! Generation-log queries and the recorder used by pipeline runners.

use std::sync::Arc;

use chrono::Duration;
use memory_domain::error::{DomainError, DomainResult, ErrorKind};
use memory_domain::memory::Layer;
use memory_domain::scope::DepartmentId;
use sha2::{Digest, Sha256};
use tracing::warn;

use super::prompts::CustomStrategy;
use crate::ports::pipeline::{
    GenerationCount, GenerationLog, GenerationLogFilter, GenerationLogRepository, GenerationRef,
};
use crate::ports::system::{Clock, IdGenerator};

const DEFAULT_RANGE_DAYS: i64 = 7;

pub struct GenerationLogPage {
    pub items: Vec<GenerationLog>,
    pub next_cursor: Option<String>,
}

/// Everything a runner knows about one finished generation.
pub struct GenerationRecord<'a> {
    pub layer: Layer,
    pub team_id: &'a str,
    pub agent_id: &'a str,
    pub user_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub strategy: Option<&'a CustomStrategy>,
    pub system_prompt: &'a str,
    pub model: &'a str,
    pub input_refs: Vec<GenerationRef>,
    pub output_refs: Vec<GenerationRef>,
    pub started_at_ms: i64,
    pub error: Option<String>,
}

pub struct GenerationLogService {
    repo: Arc<dyn GenerationLogRepository>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl GenerationLogService {
    pub fn new(
        repo: Arc<dyn GenerationLogRepository>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { repo, clock, ids }
    }

    /// Best-effort: a failing log write never fails the generation itself.
    pub async fn record(&self, department: &DepartmentId, r: GenerationRecord<'_>) {
        let finished = self.clock.now().timestamp_millis();
        // `empty`: the LLM answered but nothing was written (nothing worth
        // keeping, or an answer that changed nothing).
        let status = if r.error.is_some() {
            "failed"
        } else if r.output_refs.is_empty() {
            "empty"
        } else {
            "succeeded"
        };
        let log = GenerationLog {
            log_id: format!(
                "mgl_{}_{status}_{finished}_{}",
                r.layer.as_str(),
                self.ids.hex(16)
            ),
            layer: r.layer,
            status: status.into(),
            team_id: r.team_id.into(),
            agent_id: r.agent_id.into(),
            user_id: r.user_id.map(Into::into),
            session_id: r.session_id.map(Into::into),
            memory_prompt_id: r
                .strategy
                .map(|s| s.memory_prompt_id.clone())
                .unwrap_or_else(|| format!("builtin:{}", r.layer.as_str())),
            prompt_sha256: hex_sha256(r.system_prompt),
            model: r.model.into(),
            input_refs: r.input_refs,
            output_refs: r.output_refs,
            started_at_ms: r.started_at_ms,
            finished_at_ms: finished,
            latency_ms: finished - r.started_at_ms,
            error_message: r.error,
        };
        if let Err(err) = self.repo.append(department, &log).await {
            warn!(error = %err, "failed to write generation log");
        }
    }

    pub async fn list(
        &self,
        department: &DepartmentId,
        layer: Option<Layer>,
        status: Option<String>,
        range_ms: Option<(i64, i64)>,
        limit: usize,
        cursor: Option<&str>,
    ) -> DomainResult<GenerationLogPage> {
        let now = self.clock.now().timestamp_millis();
        let (from_ms, to_ms) = range_ms.unwrap_or((
            now - Duration::days(DEFAULT_RANGE_DAYS).num_milliseconds(),
            now,
        ));
        let before = cursor.map(decode_cursor).transpose()?;
        let filter = GenerationLogFilter {
            layer,
            status,
            from_ms,
            to_ms,
            before,
        };
        let limit = limit.clamp(1, 100);
        let items = self.repo.list(department, &filter, limit).await?;
        let next_cursor = (items.len() == limit)
            .then(|| {
                items
                    .last()
                    .map(|l| format!("{}:{}", l.started_at_ms, l.log_id))
            })
            .flatten();
        Ok(GenerationLogPage { items, next_cursor })
    }

    /// Generations started in the last `hours`, per layer and status.
    pub async fn summary(
        &self,
        department: &DepartmentId,
        hours: i64,
    ) -> DomainResult<(i64, Vec<GenerationCount>)> {
        let from_ms = (self.clock.now() - Duration::hours(hours)).timestamp_millis();
        Ok((from_ms, self.repo.summarize(department, from_ms).await?))
    }

    pub async fn get(
        &self,
        department: &DepartmentId,
        log_id: &str,
    ) -> DomainResult<GenerationLog> {
        self.repo
            .get(department, log_id)
            .await?
            .ok_or_else(log_not_found)
    }

    pub async fn find_by_memory(
        &self,
        department: &DepartmentId,
        layer: Layer,
        memory_id: &str,
    ) -> DomainResult<GenerationLog> {
        self.repo
            .find_by_output(department, layer, memory_id)
            .await?
            .ok_or_else(log_not_found)
    }
}

fn log_not_found() -> DomainError {
    DomainError::new(
        ErrorKind::NotFound,
        "MEMORY_GENERATION_LOG_NOT_FOUND",
        "generation log not found",
    )
}

fn decode_cursor(raw: &str) -> DomainResult<(i64, String)> {
    let invalid = || {
        DomainError::new(
            ErrorKind::InvalidInput,
            "INVALID_GENERATION_LOG_CURSOR",
            "invalid cursor",
        )
    };
    if raw.len() > 512 {
        return Err(invalid());
    }
    let (ts, id) = raw.split_once(':').ok_or_else(invalid)?;
    Ok((ts.parse().map_err(|_| invalid())?, id.to_owned()))
}

pub fn hex_sha256(text: &str) -> String {
    hex_encode(&Sha256::digest(text.as_bytes()))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
