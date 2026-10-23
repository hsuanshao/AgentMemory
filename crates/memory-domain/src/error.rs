//! Domain error taxonomy. Each kind maps to one HTTP status at the gateway;
//! the domain itself stays transport-agnostic.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Malformed or contradictory input (HTTP 400).
    InvalidInput,
    /// Missing or bad credentials (HTTP 401).
    Unauthenticated,
    /// Authenticated but not allowed, or ownership mismatch (HTTP 403).
    PermissionDenied,
    /// Missing, or not visible from the caller's context (HTTP 404).
    NotFound,
    /// Optimistic-lock failure, duplicates, limits (HTTP 409).
    Conflict,
    /// Valid shape but violates a business rule (HTTP 422).
    Unprocessable,
    /// Payload too large (HTTP 413).
    TooLarge,
    /// Quota exceeded (HTTP 429).
    QuotaExceeded,
    /// A dependency (LLM, storage) is unavailable (HTTP 503).
    Unavailable,
    /// Anything else (HTTP 500).
    Internal,
}

/// `code` is a stable machine-readable identifier such as
/// `team_not_found` or `SKILL_VERSION_STALE`.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{code}: {detail}")]
pub struct DomainError {
    pub kind: ErrorKind,
    pub code: String,
    pub detail: String,
}

impl DomainError {
    pub fn new(kind: ErrorKind, code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            code: code.into(),
            detail: detail.into(),
        }
    }

    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidInput, "invalid_argument", detail)
    }

    pub fn not_found(what: &str, id: impl std::fmt::Display) -> Self {
        Self::new(
            ErrorKind::NotFound,
            format!("{what}_not_found"),
            format!("not found: {id}"),
        )
    }

    pub fn permission_denied(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::PermissionDenied, "permission_denied", detail)
    }

    pub fn conflict(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, code, detail)
    }

    pub fn unprocessable(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unprocessable, code, detail)
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, "unavailable", detail)
    }

    /// A department nobody created (`init-admin`), or one destroyed since.
    pub fn unknown_department(department: impl std::fmt::Display) -> Self {
        Self::new(
            ErrorKind::NotFound,
            "unknown_department",
            format!("{department}: run init-admin for it first"),
        )
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, "internal", detail)
    }
}

pub type DomainResult<T> = Result<T, DomainError>;
