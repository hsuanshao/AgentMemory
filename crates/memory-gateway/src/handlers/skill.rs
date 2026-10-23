//! `/v3/skill/*`.

use axum::extract::State;
use axum::response::Response;
use memory_application::ports::skill::{BufferKey, BufferedMessage};
use memory_application::skill::service::{
    ListQuery, ReadOptions, ResourceInput, SkillIdentity, StringPatch,
};
use memory_domain::meta::visibility::ScopeVisibility;
use serde_json::{json, Value};

use crate::context::{Ctx, JsonBody};
use crate::envelope::{ok, ApiError};
use crate::state::SharedState;

type ApiResult = Result<Response, ApiError>;

fn identity(ctx: &Ctx, body: &Value) -> Result<SkillIdentity, ApiError> {
    ctx.parse(body)
}

fn str_of<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}

fn required<'a>(ctx: &Ctx, body: &'a Value, key: &str) -> Result<&'a str, ApiError> {
    str_of(body, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request(&ctx.request_id, format!("{key} is required")))
}

fn expected_version(ctx: &Ctx, body: &Value) -> Result<u32, ApiError> {
    body.get("expected_version")
        .and_then(Value::as_u64)
        .filter(|v| *v >= 1)
        .map(|v| v as u32)
        .ok_or_else(|| {
            ApiError::bad_request(&ctx.request_id, "expected_version (>= 1) is required")
        })
}

fn version_of(body: &Value) -> Option<u32> {
    body.get("version")
        .and_then(Value::as_u64)
        .map(|v| v as u32)
}

fn flag(body: &Value, key: &str, default: bool) -> bool {
    body.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn read_options(body: &Value) -> ReadOptions {
    ReadOptions {
        version: version_of(body),
        include_content: flag(body, "include_content", true),
        include_manifest: flag(body, "include_manifest", true),
    }
}

fn pagination(body: &Value) -> (Option<usize>, Option<usize>) {
    let p = body.get("pagination").unwrap_or(&Value::Null);
    let n = |k: &str| p.get(k).and_then(Value::as_u64).map(|v| v as usize);
    (n("limit"), n("offset"))
}

pub async fn create(State(s): State<SharedState>, ctx: Ctx, JsonBody(body): JsonBody) -> ApiResult {
    let department = ctx.department()?;
    let who = identity(&ctx, &body)?;
    let resources: Vec<ResourceInput> = body
        .get("resources")
        .map(|v| ctx.parse(v))
        .transpose()?
        .unwrap_or_default();
    let summary = s
        .services
        .skills
        .create(
            &department,
            &who,
            required(&ctx, &body, "name")?,
            required(&ctx, &body, "content")?,
            resources,
            body.get("metadata").cloned(),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, summary))
}

pub async fn update(State(s): State<SharedState>, ctx: Ctx, JsonBody(body): JsonBody) -> ApiResult {
    let department = ctx.department()?;
    let summary = s
        .services
        .skills
        .update(
            &department,
            &identity(&ctx, &body)?,
            required(&ctx, &body, "skill_id")?,
            expected_version(&ctx, &body)?,
            required(&ctx, &body, "content")?,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, summary))
}

pub async fn patch(State(s): State<SharedState>, ctx: Ctx, JsonBody(body): JsonBody) -> ApiResult {
    let department = ctx.department()?;
    let summary = s
        .services
        .skills
        .patch(
            &department,
            &identity(&ctx, &body)?,
            required(&ctx, &body, "skill_id")?,
            expected_version(&ctx, &body)?,
            &StringPatch {
                old: str_of(&body, "old_string").unwrap_or_default().to_owned(),
                new: str_of(&body, "new_string").unwrap_or_default().to_owned(),
                replace_all: flag(&body, "replace_all", false),
            },
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, summary))
}

pub async fn delete(State(s): State<SharedState>, ctx: Ctx, JsonBody(body): JsonBody) -> ApiResult {
    let department = ctx.department()?;
    let data = s
        .services
        .skills
        .delete(
            &department,
            &identity(&ctx, &body)?,
            required(&ctx, &body, "skill_id")?,
            expected_version(&ctx, &body)?,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, data))
}

pub async fn get(State(s): State<SharedState>, ctx: Ctx, JsonBody(body): JsonBody) -> ApiResult {
    let department = ctx.department()?;
    let detail = s
        .services
        .skills
        .get(
            &department,
            &identity(&ctx, &body)?,
            required(&ctx, &body, "skill_id")?,
            read_options(&body),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, detail))
}

pub async fn get_by_name(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let detail = s
        .services
        .skills
        .get_by_name(
            &department,
            required(&ctx, &body, "team_id")?,
            required(&ctx, &body, "agent_id")?,
            required(&ctx, &body, "skill_name")?,
            read_options(&body),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, detail))
}

pub async fn list(State(s): State<SharedState>, ctx: Ctx, JsonBody(body): JsonBody) -> ApiResult {
    let department = ctx.department()?;
    let mut query: ListQuery = body
        .get("filters")
        .map(|v| ctx.parse(v))
        .transpose()?
        .unwrap_or_default();
    (query.limit, query.offset) = pagination(&body);
    let (items, total) = s
        .services
        .skills
        .list(&department, &identity(&ctx, &body)?, query)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"items": items, "total": total})))
}

pub async fn search(State(s): State<SharedState>, ctx: Ctx, JsonBody(body): JsonBody) -> ApiResult {
    let department = ctx.department()?;
    let top_k = body
        .get("top_k")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let team_scope = str_of(&body, "scope") == Some("team");
    let items = s
        .services
        .skills
        .search(
            &department,
            &identity(&ctx, &body)?,
            required(&ctx, &body, "query")?,
            top_k,
            team_scope,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"items": items})))
}

pub async fn versions(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let (limit, offset) = pagination(&body);
    let (items, total) = s
        .services
        .skills
        .versions(
            &department,
            &identity(&ctx, &body)?,
            required(&ctx, &body, "skill_id")?,
            limit,
            offset,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"items": items, "total": total})))
}

pub async fn files_write(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let files: Vec<ResourceInput> = ctx.parse(body.get("files").unwrap_or(&Value::Null))?;
    let summary = s
        .services
        .skills
        .write_files(
            &department,
            &identity(&ctx, &body)?,
            required(&ctx, &body, "skill_id")?,
            expected_version(&ctx, &body)?,
            files,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, summary))
}

pub async fn files_remove(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let paths: Vec<String> = ctx.parse(body.get("paths").unwrap_or(&Value::Null))?;
    let summary = s
        .services
        .skills
        .remove_files(
            &department,
            &identity(&ctx, &body)?,
            required(&ctx, &body, "skill_id")?,
            expected_version(&ctx, &body)?,
            paths,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, summary))
}

pub async fn files_read(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let file = s
        .services
        .skills
        .read_file(
            &department,
            &identity(&ctx, &body)?,
            required(&ctx, &body, "skill_id")?,
            required(&ctx, &body, "path")?,
            version_of(&body),
            str_of(&body, "encoding"),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, file))
}

pub async fn listing(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let budget = body
        .get("char_budget")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let listing = s
        .services
        .skills
        .listing(
            &department,
            &identity(&ctx, &body)?,
            str_of(&body, "query"),
            budget,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, listing))
}

pub async fn export(ctx: Ctx) -> ApiResult {
    Err(ApiError::not_implemented(
        &ctx.request_id,
        "skill export is not implemented",
    ))
}

fn buffer_key(ctx: &Ctx, body: &Value, session: Option<String>) -> Result<BufferKey, ApiError> {
    Ok(BufferKey {
        user_id: required(ctx, body, "user_id")?.to_owned(),
        team_id: required(ctx, body, "team_id")?.to_owned(),
        agent_id: required(ctx, body, "agent_id")?.to_owned(),
        session_id: match session {
            Some(s) => s,
            None => required(ctx, body, "session_id")?.to_owned(),
        },
    })
}

fn messages(ctx: &Ctx, body: &Value) -> Result<Vec<BufferedMessage>, ApiError> {
    ctx.parse(body.get("messages").unwrap_or(&Value::Null))
}

pub async fn conversation_add(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let key = buffer_key(&ctx, &body, None)?;
    let result = s
        .services
        .skill_extraction
        .conversation_add(&department, key, messages(&ctx, &body)?)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, result))
}

pub async fn force_archive(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let key = buffer_key(&ctx, &body, None)?;
    let reason = str_of(&body, "reason").map(str::to_owned);
    Ok(ok(
        &ctx.request_id,
        s.services
            .skill_extraction
            .force_archive(&department, key, reason)
            .await
            .map_err(|e| ctx.err(e))?,
    ))
}

pub async fn extract(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let who = identity(&ctx, &body)?;
    let archived = s
        .services
        .skill_extraction
        .extract(
            &department,
            &who,
            str_of(&body, "session_id").map(str::to_owned),
            messages(&ctx, &body)?,
            str_of(&body, "reason").map(str::to_owned),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"ok": true, "task_id": archived.task_id, "archived_at_ms": archived.archived_at_ms, "archive_key": archived.archive_key}),
    ))
}

// ── Proposals from skill extraction ─────────────────────────────────────

/// What the caller's role lets them decide (their own tasks, or any in a
/// team they oversee; all for system admins).
async fn decider(s: &SharedState, ctx: &Ctx) -> Result<(String, ScopeVisibility), ApiError> {
    let department = ctx.department()?;
    let key = ctx.user_key()?;
    let meta = &s.services.meta;
    let caller = meta
        .authenticate(&department, &key)
        .await
        .map_err(|e| ctx.err(e))?;
    let visibility = meta
        .visibility(&department, Some(&key))
        .await
        .map_err(|e| ctx.err(e))?;
    Ok((caller.user_id, visibility))
}

pub async fn proposal_list(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let team = required(&ctx, &body, "team_id")?;
    let (_, visibility) = decider(&s, &ctx).await?;
    let items = s
        .services
        .skill_extraction
        .proposals(&department, &visibility, team, str_of(&body, "agent_id"))
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({ "items": items })))
}

async fn decide(s: SharedState, ctx: Ctx, body: Value, approve: bool) -> ApiResult {
    let department = ctx.department()?;
    let task_id = required(&ctx, &body, "task_id")?;
    let index = body
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| ApiError::bad_request(&ctx.request_id, "index is required"))?;
    let (user, visibility) = decider(&s, &ctx).await?;
    let decided = s
        .services
        .skill_extraction
        .decide(
            &department,
            &visibility,
            &user,
            task_id,
            index as usize,
            approve,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, decided))
}

pub async fn proposal_approve(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    decide(s, ctx, body, true).await
}

pub async fn proposal_reject(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    decide(s, ctx, body, false).await
}
