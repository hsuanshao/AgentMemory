//! Ports to the outside world that are not storage: LLM, embedding, time
//! and identifiers.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memory_domain::error::DomainResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
}

/// The error code of a completion that tried to use a tool instead of
/// answering: a CLI refused the call, or an endpoint returned a tool call.
/// Distillation never allows tools, so the same input fails the same way
/// again (#37).
pub const LLM_TOOL_REQUEST: &str = "LLM_TOOL_REQUEST";

/// An OpenAI-compatible chat completion endpoint.
#[async_trait]
pub trait LlmClient: Send + Sync {
    fn is_configured(&self) -> bool;
    fn model(&self) -> &str;
    async fn complete(&self, messages: &[ChatMessage]) -> DomainResult<String>;
}

/// Text embedding. A provider with `dimensions() == None` disables vector
/// search; recall then degrades to BM25.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn dimensions(&self) -> Option<usize>;
    async fn embed(&self, texts: &[String]) -> DomainResult<Vec<Vec<f32>>>;
}

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub trait IdGenerator: Send + Sync {
    /// `<prefix><random>`, e.g. `msg-3f9a…`.
    fn next(&self, prefix: &str) -> String;
    /// `n` lowercase hex characters.
    fn hex(&self, n: usize) -> String;
    /// URL-safe base64 of `bytes` cryptographically random bytes.
    fn secret(&self, bytes: usize) -> String;
}
