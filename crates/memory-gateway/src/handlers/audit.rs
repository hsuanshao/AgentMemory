//! The credential audit log: a department's own events for its system
//! admins, every event for the operator.

use axum::extract::State;
use axum::response::Response;
use memory_application::common::Page;
use memory_application::ports::audit::{AuditFilter, AuditKind};
use serde_json::{json, Value};

use crate::context::{Ctx, JsonBody};
use crate::envelope::{ok, ApiError};
use crate::state::SharedState;

type ApiResult = Result<Response, ApiError>;

fn filter(ctx: &Ctx, body: &Value, department: Option<String>) -> Result<AuditFilter, ApiError> {
    let kind = match body.get("kind").and_then(Value::as_str) {
        None | Some("") => None,
        Some(k) => Some(AuditKind::parse(k).ok_or_else(|| {
            ApiError::bad_request(&ctx.request_id, format!("unknown audit kind `{k}`"))
        })?),
    };
    Ok(AuditFilter {
        department,
        kind,
        since_ms: body.get("since_ms").and_then(Value::as_i64),
    })
}

fn page(body: &Value) -> Page {
    let n = |k: &str| body.get(k).and_then(Value::as_u64).map(|v| v as usize);
    Page::clamped(n("limit"), n("offset"), 50, 500)
}

async fn list(s: &SharedState, ctx: &Ctx, filter: AuditFilter, page: Page) -> ApiResult {
    let items = s
        .services
        .guard
        .list(&filter, page)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({ "items": items })))
}

/// `audit/list`: events that named the caller's department.
pub async fn department_list(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let filter = filter(&ctx, &body, Some(department.as_str().to_owned()))?;
    list(&s, &ctx, filter, page(&body)).await
}

/// `internal/audit/list`: every event, or one department's
/// (`department_id` in the body), including departments that do not exist.
pub async fn operator_list(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = body
        .get("department_id")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .map(str::to_owned);
    let filter = filter(&ctx, &body, department)?;
    list(&s, &ctx, filter, page(&body)).await
}
