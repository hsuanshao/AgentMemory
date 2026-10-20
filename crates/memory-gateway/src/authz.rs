//! User-key authorization for the memory data plane and skills.
//!
//! One choke point instead of per-handler checks: the middleware
//! authenticates `x-memory-user-key`, authorizes the requested
//! team/agent/user, and rewrites the body and isolation headers to the
//! resolved scope so handlers can only ever see an authorized one.

use axum::body::{to_bytes, Body};
use axum::extract::{OriginalUri, Request, State};
use axum::http::{header, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use memory_domain::meta::memory_access::{MemoryAction, RequestedScope};
use serde_json::Value;

use crate::context::Ctx;
use crate::envelope::ApiError;
use crate::state::SharedState;

/// Last path segments of routes that never mutate data.
const READ_SUFFIXES: &[&str] = &[
    "query",
    "search",
    "count",
    "ls",
    "read",
    "recall",
    "get",
    "get-by-name",
    "list",
    "versions",
    "listing",
];

/// Anything not known to be a read is treated as a write.
fn action_for(path: &str) -> MemoryAction {
    let last = path.rsplit('/').next().unwrap_or_default();
    if READ_SUFFIXES.contains(&last) {
        MemoryAction::Read
    } else {
        MemoryAction::Write
    }
}

const FIELDS: [&str; 4] = ["team_id", "agent_id", "user_id", "task_id"];

fn header_name(field: &str) -> String {
    format!("x-memory-{}", field.replace('_', "-"))
}

fn requested(ctx: &Ctx, body: &Value) -> RequestedScope {
    let pick = |field: &str| {
        body.get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
            .or_else(|| ctx.header(&header_name(field)))
    };
    RequestedScope {
        team_id: pick("team_id"),
        agent_id: pick("agent_id"),
        user_id: pick("user_id"),
        task_id: pick("task_id"),
    }
}

pub async fn require_user(
    State(s): State<SharedState>,
    ctx: Ctx,
    req: Request,
    next: Next,
) -> Response {
    match authorize(&s, &ctx, req).await {
        Ok(req) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

async fn authorize(s: &SharedState, ctx: &Ctx, req: Request) -> Result<Request, ApiError> {
    let department = ctx.department()?;
    let user_key = ctx.user_key()?;
    let path = req
        .extensions()
        .get::<OriginalUri>()
        .map_or_else(|| req.uri().path().to_owned(), |u| u.path().to_owned());
    let limit = if path.ends_with("/session/add") {
        s.settings.max_session_body_bytes
    } else {
        s.settings.max_body_bytes
    };

    let (mut parts, body) = req.into_parts();
    let bytes = to_bytes(body, limit).await.map_err(|e| {
        ApiError::payload_too_large(&ctx.request_id, format!("Request body rejected: {e}"))
    })?;
    let mut json: Value = if bytes.iter().all(u8::is_ascii_whitespace) {
        Value::Object(Default::default())
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::bad_request(&ctx.request_id, "Invalid JSON body"))?
    };
    let Some(object) = json.as_object_mut() else {
        return Err(ApiError::bad_request(
            &ctx.request_id,
            "body must be a JSON object",
        ));
    };

    let scope = s
        .services
        .meta
        .authorize_memory(
            &department,
            &user_key,
            &requested(ctx, &Value::Object(object.clone())),
            action_for(&path),
        )
        .await
        .map_err(|e| ctx.err(e))?;

    let resolved = [
        Some(scope.team_id.as_str()),
        Some(scope.agent_id.as_str()),
        Some(scope.user_id.as_str()),
        scope.task_id.as_ref().map(|t| t.as_str()),
    ];
    for (field, value) in FIELDS.iter().zip(resolved) {
        let name = HeaderName::try_from(header_name(field)).expect("static isolation header name");
        match value {
            Some(v) => {
                object.insert((*field).to_owned(), Value::String(v.to_owned()));
                let v = HeaderValue::from_str(v).map_err(|_| {
                    ApiError::bad_request(&ctx.request_id, format!("invalid {field}"))
                })?;
                parts.headers.insert(name, v);
            }
            None => {
                object.remove(*field);
                parts.headers.remove(name);
            }
        }
    }

    let bytes = serde_json::to_vec(&json).expect("JSON value serializes");
    parts
        .headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
    Ok(Request::from_parts(parts, Body::from(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_routes_default_to_write() {
        assert_eq!(action_for("/v3/recall"), MemoryAction::Read);
        assert_eq!(action_for("/v3/skill/files/read"), MemoryAction::Read);
        assert_eq!(action_for("/v3/atomic/search"), MemoryAction::Read);
        assert_eq!(action_for("/v3/conversation/add"), MemoryAction::Write);
        assert_eq!(action_for("/v3/scenario/rm"), MemoryAction::Write);
        assert_eq!(
            action_for("/v3/skill/conversation/force-archive"),
            MemoryAction::Write
        );
    }
}
