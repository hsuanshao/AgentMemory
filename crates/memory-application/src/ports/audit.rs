//! Credential failures: the audit log they are written to, and the counter
//! the gateway throttles a source by. Both are system-wide, not per
//! department: a guess against an unknown department is recorded too.

use std::time::Duration;

use async_trait::async_trait;
use memory_domain::error::DomainResult;
use serde::{Deserialize, Serialize};

use crate::common::Page;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    /// A user key or the server API key was presented and refused.
    CredentialRejected,
    /// A source reached the failure limit and is refused for a while.
    SourceLocked,
    /// A user key was issued outside the Panel (`internal/upgrade`).
    KeyIssued,
}

impl AuditKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CredentialRejected => "credential_rejected",
            Self::SourceLocked => "source_locked",
            Self::KeyIssued => "key_issued",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "credential_rejected" => Some(Self::CredentialRejected),
            "source_locked" => Some(Self::SourceLocked),
            "key_issued" => Some(Self::KeyIssued),
            _ => None,
        }
    }
}

/// One audit record. Never holds a credential: `credential` names its kind
/// and, for a user key, the first characters of its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub event_id: String,
    pub at_ms: i64,
    pub kind: AuditKind,
    /// The department the request named, as sent (it may not exist).
    pub department: Option<String>,
    /// The client address the gateway throttles by.
    pub source: String,
    /// `X-Forwarded-For` as received, when a trusted proxy sent it.
    pub forwarded_for: Option<String>,
    pub user_agent: Option<String>,
    pub path: String,
    pub credential: Option<String>,
    pub request_id: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    /// `None` lists every department's events (the operator's view).
    pub department: Option<String>,
    pub kind: Option<AuditKind>,
    pub since_ms: Option<i64>,
}

#[async_trait]
pub trait AuditLog: Send + Sync {
    async fn record(&self, event: &AuditEvent) -> DomainResult<()>;
    /// Newest first.
    async fn list(&self, filter: &AuditFilter, page: Page) -> DomainResult<Vec<AuditEvent>>;
    /// Removes events older than `before_ms`; returns how many.
    async fn prune(&self, before_ms: i64) -> DomainResult<u64>;
}

/// Where a source stands right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FailureState {
    /// Failures in the current window.
    pub failures: u32,
    /// Time left on a lock, if the source is locked.
    pub locked_for: Option<Duration>,
}

/// Failure counts per source, shared by every replica when coordination
/// is shared.
#[async_trait]
pub trait FailureCounter: Send + Sync {
    async fn state(&self, source: &str) -> DomainResult<FailureState>;
    /// Counts one failure; the window starts with the first one and the
    /// count resets when it ends. Returns the count.
    async fn add(&self, source: &str, window: Duration) -> DomainResult<u32>;
    async fn lock(&self, source: &str, duration: Duration) -> DomainResult<()>;
}
