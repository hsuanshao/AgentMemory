//! The v3 response envelope `{ code, message, request_id, data }` and the
//! mapping from domain errors to HTTP status codes.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use memory_domain::error::{DomainError, ErrorKind};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct Envelope<T: Serialize> {
    pub code: i64,
    pub message: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

pub fn ok<T: Serialize>(request_id: &str, data: T) -> Response {
    let body = Envelope {
        code: 0,
        message: "ok".into(),
        request_id: request_id.into(),
        data: Some(data),
    };
    (StatusCode::OK, Json(body)).into_response()
}

/// A failed call. `code` is the envelope code (HTTP status for most modules,
/// five-digit codes for the skill module).
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
    pub request_id: String,
}

impl ApiError {
    pub fn from_domain(request_id: &str, err: DomainError) -> Self {
        let status = status_for(err.kind);
        let code = skill_code(&err.code).unwrap_or(status.as_u16() as i64);
        Self {
            status,
            code,
            message: public_message(request_id, &err),
            data: None,
            request_id: request_id.into(),
        }
    }

    pub fn bad_request(request_id: &str, message: impl Into<String>) -> Self {
        Self::plain(StatusCode::BAD_REQUEST, request_id, message)
    }

    fn plain(status: StatusCode, request_id: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            code: status.as_u16() as i64,
            message: message.into(),
            data: None,
            request_id: request_id.into(),
        }
    }

    pub fn unauthorized(request_id: &str, message: impl Into<String>) -> Self {
        Self::plain(StatusCode::UNAUTHORIZED, request_id, message)
    }

    pub fn payload_too_large(request_id: &str, message: impl Into<String>) -> Self {
        Self::plain(StatusCode::PAYLOAD_TOO_LARGE, request_id, message)
    }

    pub fn not_found(request_id: &str, message: impl Into<String>) -> Self {
        Self::plain(StatusCode::NOT_FOUND, request_id, message)
    }

    pub fn not_implemented(request_id: &str, message: impl Into<String>) -> Self {
        Self::plain(StatusCode::NOT_IMPLEMENTED, request_id, message)
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Envelope {
            code: self.code,
            message: self.message,
            request_id: self.request_id,
            data: self.data,
        };
        (self.status, Json(body)).into_response()
    }
}

/// What the caller is told. A failure of the server itself (storage, a
/// dependency, a bug) is logged with its detail, which may name files,
/// hosts or queries, and the caller gets its code (e.g. `LLM_UNAVAILABLE`)
/// and the request id to report.
fn public_message(request_id: &str, err: &DomainError) -> String {
    match err.kind {
        ErrorKind::Internal => {
            tracing::error!(request_id, error = %err, "internal error");
            format!(
                "{}: the server failed; its log has the details (request {request_id})",
                err.code
            )
        }
        ErrorKind::Unavailable => {
            tracing::warn!(request_id, error = %err, "service unavailable");
            format!(
                "{}: a service memory-core depends on is unavailable; its log has the details (request {request_id})",
                err.code
            )
        }
        _ => err.to_string(),
    }
}

pub fn status_for(kind: ErrorKind) -> StatusCode {
    match kind {
        ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
        ErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
        ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        ErrorKind::NotFound => StatusCode::NOT_FOUND,
        ErrorKind::Conflict => StatusCode::CONFLICT,
        ErrorKind::Unprocessable => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorKind::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorKind::QuotaExceeded => StatusCode::TOO_MANY_REQUESTS,
        ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Five-digit envelope codes of the skill module (API doc §4.2).
fn skill_code(code: &str) -> Option<i64> {
    Some(match code {
        "INVALID_FRONTMATTER" | "INVALID_PATH" => 40001,
        "SKILL_NOT_OWNER" => 40301,
        "SKILL_TEAM_MISMATCH" => 40302,
        "SKILL_NOT_FOUND" => 40401,
        "SKILL_VERSION_STALE" => 40901,
        "SKILL_VERSION_EXPIRED" => 41002,
        "RESOURCE_TOO_LARGE" | "SKILL_EXPORT_TOO_LARGE" => 41301,
        "SKILL_NAME_DUPLICATE" => 42201,
        "SKILL_PATCH_NOT_UNIQUE" => 42202,
        "SKILL_FRONTMATTER_INVALID" => 42203,
        "SKILL_QUOTA_EXCEEDED" => 4291,
        "LLM_UNAVAILABLE" => 50302,
        "SKILL_ID_COLLISION" => 50304,
        _ => return None,
    })
}

#[cfg(test)]
mod public_message_tests {
    use super::*;

    #[test]
    fn server_failures_do_not_reach_the_caller() {
        let err = DomainError::unavailable("create /data/departments/x: Permission denied");
        let api = ApiError::from_domain("req-1", err);
        assert_eq!(api.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(!api.message.contains("/data"), "{}", api.message);
        assert!(api.message.contains("req-1"));
        let conflict = ApiError::from_domain("r", DomainError::conflict("taken", "name taken"));
        assert_eq!(conflict.message, "taken: name taken");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_errors_use_five_digit_codes_but_keep_http_status() {
        let err = DomainError::new(ErrorKind::Conflict, "SKILL_VERSION_STALE", "stale");
        let api = ApiError::from_domain("r", err);
        assert_eq!(api.status, StatusCode::CONFLICT);
        assert_eq!(api.code, 40901);
    }

    #[test]
    fn meta_errors_use_http_status_as_code() {
        let api = ApiError::from_domain("r", DomainError::not_found("team", "t_1"));
        assert_eq!(api.code, 404);
        assert_eq!(api.message, "team_not_found: not found: t_1");
    }
}
