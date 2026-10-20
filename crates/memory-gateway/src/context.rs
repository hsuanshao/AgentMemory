//! Per-request context: request id, target department, isolation headers and
//! the JSON body (parsed here so malformed input still gets an envelope).

use axum::async_trait;
use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::HeaderMap;
use memory_domain::error::DomainError;
use memory_domain::scope::{DepartmentId, MemoryScope, SegmentId};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::envelope::ApiError;

pub const DEPARTMENT_ID: &str = "x-memory-department-id";
pub const USER_KEY: &str = "x-memory-user-key";

#[derive(Debug, Clone)]
pub struct Ctx {
    pub request_id: String,
    headers: HeaderMap,
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

fn new_request_id() -> String {
    format!("req-{}", &uuid::Uuid::new_v4().simple().to_string()[..16])
}

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for Ctx {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let request_id = header(&parts.headers, "x-request-id").unwrap_or_else(new_request_id);
        Ok(Self {
            request_id,
            headers: parts.headers.clone(),
        })
    }
}

impl Ctx {
    pub fn header(&self, name: &str) -> Option<String> {
        header(&self.headers, name)
    }

    pub fn err(&self, e: DomainError) -> ApiError {
        ApiError::from_domain(&self.request_id, e)
    }

    /// The memory department (`x-memory-department-id`), required on `/v3/*`.
    pub fn department(&self) -> Result<DepartmentId, ApiError> {
        let raw = self.header(DEPARTMENT_ID).ok_or_else(|| {
            let message = if self.header("x-memory-service-id").is_some() {
                // Sent by clients older than 0.3.0.
                "missing header: x-memory-department-id (x-memory-service-id was renamed in 0.3.0; update the client)"
            } else {
                "missing header: x-memory-department-id"
            };
            ApiError::unauthorized(&self.request_id, message)
        })?;
        SegmentId::parse(DEPARTMENT_ID, &raw).map_err(|e| self.err(e))
    }

    pub fn user_key(&self) -> Result<String, ApiError> {
        self.header(USER_KEY).ok_or_else(|| {
            ApiError::unauthorized(&self.request_id, "unauthorized: missing_user_key")
        })
    }

    /// Body field first, then `x-memory-<name>` header.
    fn isolation(&self, body: &Value, field: &str) -> Option<String> {
        body.get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
            .or_else(|| self.header(&format!("x-memory-{}", field.replace('_', "-"))))
    }

    pub fn scope(&self, body: &Value) -> Result<MemoryScope, ApiError> {
        MemoryScope::from_optional(
            self.isolation(body, "team_id").as_deref(),
            self.isolation(body, "agent_id").as_deref(),
            self.isolation(body, "user_id").as_deref(),
            self.isolation(body, "task_id").as_deref(),
        )
        .map_err(|e| self.err(e))
    }

    pub fn session_id(&self, body: &Value) -> Option<String> {
        self.isolation(body, "session_id")
    }

    pub fn parse<T: DeserializeOwned>(&self, body: &Value) -> Result<T, ApiError> {
        serde_json::from_value(body.clone())
            .map_err(|e| ApiError::bad_request(&self.request_id, format!("invalid request: {e}")))
    }
}

/// The request body as JSON; an empty body is `{}`.
pub struct JsonBody(pub Value);

#[async_trait]
impl<S: Send + Sync> FromRequest<S> for JsonBody {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let request_id = header(req.headers(), "x-request-id").unwrap_or_default();
        let bytes = Bytes::from_request(req, state).await.map_err(|e| {
            ApiError::payload_too_large(&request_id, format!("Request body rejected: {e}"))
        })?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self(Value::Object(Default::default())));
        }
        serde_json::from_slice(&bytes)
            .map(Self)
            .map_err(|_| ApiError::bad_request(&request_id, "Invalid JSON body"))
    }
}
