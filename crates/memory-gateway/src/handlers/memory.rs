//! L0–L3 data plane, session turns and recall.

use axum::extract::State;
use axum::response::Response;
use chrono::{DateTime, TimeZone, Utc};
use memory_application::common::{Page, TimeRange};
use memory_application::memory::service::NewMessage;
use memory_application::memory::session::SessionTurnHeaders;
use memory_application::ports::memory::{AtomFilter, ConversationFilter};
use memory_domain::memory::{AtomType, AtomicMemory, ConversationMessage, Role};
use memory_domain::profile::{parse_scene, PersonaOp, ProfileFile, ProfileScope};
use memory_domain::retrieval::SearchMode;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::context::{Ctx, JsonBody};
use crate::envelope::{ok, ApiError};
use crate::state::SharedState;

type ApiResult = Result<Response, ApiError>;

/// Accepts ISO-8601 strings or epoch milliseconds.
fn parse_time(ctx: &Ctx, v: Option<&Value>) -> Result<Option<DateTime<Utc>>, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(n
            .as_i64()
            .and_then(|ms| Utc.timestamp_millis_opt(ms).single())),
        Some(Value::String(s)) => DateTime::parse_from_rfc3339(s)
            .map(|t| Some(t.with_timezone(&Utc)))
            .map_err(|_| ApiError::bad_request(&ctx.request_id, format!("invalid time: {s}"))),
        Some(_) => Err(ApiError::bad_request(
            &ctx.request_id,
            "time must be an ISO string or epoch ms",
        )),
    }
}

fn time_range(ctx: &Ctx, body: &Value) -> Result<TimeRange, ApiError> {
    Ok(TimeRange {
        start: parse_time(ctx, body.get("time_start"))?,
        end: parse_time(ctx, body.get("time_end"))?,
    })
}

fn page(body: &Value) -> Page {
    let n = |k: &str| body.get(k).and_then(Value::as_u64).map(|v| v as usize);
    Page::standard(n("limit"), n("offset"))
}

fn search_limit(body: &Value) -> usize {
    body.get("limit")
        .and_then(Value::as_u64)
        .map_or(5, |v| v as usize)
        .clamp(1, 100)
}

fn query_text(ctx: &Ctx, body: &Value) -> Result<String, ApiError> {
    body.get("query")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_request(&ctx.request_id, "query is required"))
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn conversation_json(m: &ConversationMessage) -> Value {
    json!({
        "id": m.id, "version": m.version, "role": role_str(m.role), "content": m.content,
        "timestamp": m.timestamp.to_rfc3339(), "recorded_at": m.recorded_at.to_rfc3339(),
        "session_id": m.session_id, "team_id": m.scope.team_id, "agent_id": m.scope.agent_id,
        "user_id": m.scope.user_id, "task_id": m.scope.task_id,
    })
}

fn atom_json(a: &AtomicMemory) -> Value {
    json!({
        "id": a.id, "version": a.version, "type": a.atom_type.as_str(), "background": a.background,
        "content": a.content, "priority": a.priority, "session_id": a.session_id,
        "created_at": a.created_at.to_rfc3339(), "updated_at": a.updated_at.to_rfc3339(),
        "team_id": a.scope.team_id, "agent_id": a.scope.agent_id, "user_id": a.scope.user_id, "task_id": a.scope.task_id,
    })
}

fn with_score(mut v: Value, score: f64) -> Value {
    v["score"] = json!(score);
    v
}

// ── L0 ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AddMessage {
    role: Role,
    content: String,
    timestamp: Option<Value>,
    recorded_at: Option<Value>,
}

pub async fn conversation_add(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let scope = ctx.scope(&body)?;
    let session = ctx.session_id(&body).unwrap_or_else(|| "default".into());
    let raw: Vec<AddMessage> = ctx.parse(body.get("messages").unwrap_or(&Value::Null))?;
    let mut messages = Vec::with_capacity(raw.len());
    for m in raw {
        messages.push(NewMessage {
            role: m.role,
            content: m.content,
            timestamp: parse_time(&ctx, m.timestamp.as_ref())?,
            recorded_at: parse_time(&ctx, m.recorded_at.as_ref())?,
        });
    }
    let stored = s
        .services
        .memory
        .add_conversation(&department, &scope, &session, messages)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({
            "accepted_ids": stored.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            "accepted_versions": stored.iter().map(|_| "v1").collect::<Vec<_>>(),
            "total_count": stored.len(),
        }),
    ))
}

fn conversation_filter(ctx: &Ctx, body: &Value) -> Result<ConversationFilter, ApiError> {
    Ok(ConversationFilter {
        session_id: ctx.session_id(body),
        time: time_range(ctx, body)?,
    })
}

pub async fn conversation_query(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, scope) = (ctx.department()?, ctx.scope(&body)?);
    let filter = conversation_filter(&ctx, &body)?;
    let (items, total) = s
        .services
        .memory
        .query_conversation(&department, &scope, &filter, page(&body))
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"messages": items.iter().map(conversation_json).collect::<Vec<_>>(), "total": total}),
    ))
}

pub async fn conversation_search(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, scope) = (ctx.department()?, ctx.scope(&body)?);
    let filter = conversation_filter(&ctx, &body)?;
    let hits = s
        .services
        .memory
        .search_conversation(
            &department,
            &scope,
            &query_text(&ctx, &body)?,
            &filter,
            search_limit(&body),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    let messages: Vec<Value> = hits
        .iter()
        .map(|h| with_score(conversation_json(&h.item), h.score))
        .collect();
    Ok(ok(&ctx.request_id, json!({"messages": messages})))
}

fn string_list(ctx: &Ctx, body: &Value, key: &str, max: usize) -> Result<Vec<String>, ApiError> {
    let mut list: Vec<String> = match body.get(key) {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => ctx.parse(v)?,
    };
    list.retain(|s| !s.is_empty());
    list.sort();
    list.dedup();
    if list.len() > max {
        return Err(ApiError::bad_request(
            &ctx.request_id,
            format!("{key} accepts at most {max} items"),
        ));
    }
    Ok(list)
}

pub async fn conversation_delete(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, scope) = (ctx.department()?, ctx.scope(&body)?);
    let ids = string_list(&ctx, &body, "message_ids", 5000)?;
    let mut sessions = string_list(&ctx, &body, "session_ids", 100)?;
    if let Some(legacy) = body.get("session_id").and_then(Value::as_str) {
        sessions.push(legacy.to_owned());
    }
    let deleted = s
        .services
        .memory
        .delete_conversation(&department, &scope, &ids, &sessions)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"deleted_count": deleted})))
}

pub async fn conversation_count(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, scope) = (ctx.department()?, ctx.scope(&body)?);
    let total = s
        .services
        .memory
        .count_conversation(&department, &scope, &conversation_filter(&ctx, &body)?)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"total": total})))
}

// ── L1 ──────────────────────────────────────────────────────────────────

fn atom_filter(ctx: &Ctx, body: &Value) -> Result<AtomFilter, ApiError> {
    let atom_type = body
        .get("type")
        .and_then(Value::as_str)
        .map(AtomType::parse)
        .transpose()
        .map_err(|e| ctx.err(e))?;
    Ok(AtomFilter {
        atom_type,
        session_id: None,
        time: time_range(ctx, body)?,
    })
}

#[derive(Deserialize)]
struct AtomUpdate {
    id: String,
    content: String,
    background: Option<String>,
}

pub async fn atomic_update(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, scope) = (ctx.department()?, ctx.scope(&body)?);
    let input: AtomUpdate = ctx.parse(&body)?;
    let atom = s
        .services
        .memory
        .update_atom(
            &department,
            &scope,
            &input.id,
            input.content,
            input.background,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"id": atom.id, "version": format!("v{}", atom.version), "updated_at": atom.updated_at.to_rfc3339()}),
    ))
}

pub async fn atomic_query(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, scope) = (ctx.department()?, ctx.scope(&body)?);
    let (items, total) = s
        .services
        .memory
        .query_atoms(&department, &scope, &atom_filter(&ctx, &body)?, page(&body))
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"items": items.iter().map(atom_json).collect::<Vec<_>>(), "total": total}),
    ))
}

pub async fn atomic_search(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, scope) = (ctx.department()?, ctx.scope(&body)?);
    let hits = s
        .services
        .memory
        .search_atoms(
            &department,
            &scope,
            &query_text(&ctx, &body)?,
            &atom_filter(&ctx, &body)?,
            search_limit(&body),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"items": hits.iter().map(|h| with_score(atom_json(&h.item), h.score)).collect::<Vec<_>>()}),
    ))
}

pub async fn atomic_delete(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, scope) = (ctx.department()?, ctx.scope(&body)?);
    let ids = string_list(&ctx, &body, "ids", 5000)?;
    let deleted = s
        .services
        .memory
        .delete_atoms(&department, &scope, &ids)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"deleted_count": deleted})))
}

pub async fn atomic_count(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, scope) = (ctx.department()?, ctx.scope(&body)?);
    let total = s
        .services
        .memory
        .count_atoms(&department, &scope, &atom_filter(&ctx, &body)?)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"total": total})))
}

// ── L2 / L3 ─────────────────────────────────────────────────────────────

fn profile_of(ctx: &Ctx, body: &Value) -> Result<ProfileScope, ApiError> {
    Ok(ProfileScope::from(&ctx.scope(body)?))
}

/// The user recorded as the author of a version written through the API.
fn author_of(ctx: &Ctx, body: &Value) -> Result<String, ApiError> {
    Ok(ctx.scope(body)?.user_id.to_string())
}

fn str_field<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}

fn version_field(body: &Value) -> Option<u32> {
    body.get("version")
        .and_then(Value::as_u64)
        .map(|v| v as u32)
}

fn file_json(path: &str, file: Option<&ProfileFile>, profile: &ProfileScope) -> Value {
    json!({
        "path": path,
        "version": file.map(|f| f.version),
        "content": file.map(|f| f.content.clone()),
        "created_at": file.map(|f| f.created_at.to_rfc3339()),
        "updated_at": file.map(|f| f.updated_at.to_rfc3339()),
        "team_id": profile.team_id, "agent_id": profile.agent_id,
    })
}

pub async fn scenario_ls(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, profile) = (ctx.department()?, profile_of(&ctx, &body)?);
    let prefix = str_field(&body, "path_prefix").unwrap_or("");
    let scenes = s
        .services
        .memory
        .list_scenes(&department, &profile, prefix)
        .await
        .map_err(|e| ctx.err(e))?;
    let entries: Vec<Value> = scenes
        .iter()
        .map(|e| {
            json!({
                "path": e.relative_path,
                "summary": e.meta.as_ref().map(|m| m.summary.clone()),
                "heat": e.meta.as_ref().map(|m| m.heat),
                "version": e.file.version,
                "team_id": profile.team_id, "agent_id": profile.agent_id,
                "created_at": e.file.created_at.to_rfc3339(), "updated_at": e.file.updated_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(ok(
        &ctx.request_id,
        json!({"total": entries.len(), "entries": entries}),
    ))
}

pub async fn scenario_read(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, profile) = (ctx.department()?, profile_of(&ctx, &body)?);
    let path = str_field(&body, "path").unwrap_or("");
    let file = s
        .services
        .memory
        .read_scene(&department, &profile, path, version_field(&body))
        .await
        .map_err(|e| ctx.err(e))?;
    let mut data = file_json(path, file.as_ref(), &profile);
    data["summary"] = json!(file
        .as_ref()
        .and_then(|f| parse_scene(&f.content).0)
        .map(|m| m.summary));
    Ok(ok(&ctx.request_id, data))
}

pub async fn scenario_write(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, profile) = (ctx.department()?, profile_of(&ctx, &body)?);
    let path = str_field(&body, "path").unwrap_or("");
    let content = str_field(&body, "content")
        .ok_or_else(|| ApiError::bad_request(&ctx.request_id, "content is required"))?;
    let author = author_of(&ctx, &body)?;
    let file = s
        .services
        .memory
        .write_scene(
            &department,
            &profile,
            path,
            content,
            str_field(&body, "summary"),
            Some(&author),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"path": path, "version": file.version, "updated_at": file.updated_at.to_rfc3339(), "team_id": profile.team_id, "agent_id": profile.agent_id}),
    ))
}

pub async fn scenario_rm(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, profile) = (ctx.department()?, profile_of(&ctx, &body)?);
    let author = author_of(&ctx, &body)?;
    let removed = s
        .services
        .memory
        .remove_scene(
            &department,
            &profile,
            str_field(&body, "path").unwrap_or(""),
            Some(&author),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"removed": removed})))
}

pub async fn scenario_count(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, profile) = (ctx.department()?, profile_of(&ctx, &body)?);
    let total = s
        .services
        .memory
        .count_scenes(
            &department,
            &profile,
            str_field(&body, "path_prefix").unwrap_or(""),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"total": total})))
}

pub async fn core_read(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, profile) = (ctx.department()?, profile_of(&ctx, &body)?);
    let file = s
        .services
        .memory
        .read_persona(&department, &profile, version_field(&body))
        .await
        .map_err(|e| ctx.err(e))?;
    let mut data = file_json("persona.md", file.as_ref(), &profile);
    data.as_object_mut().map(|o| o.remove("path"));
    Ok(ok(&ctx.request_id, data))
}

pub async fn core_write(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, profile) = (ctx.department()?, profile_of(&ctx, &body)?);
    let content = str_field(&body, "content")
        .ok_or_else(|| ApiError::bad_request(&ctx.request_id, "content is required"))?;
    let file = s
        .services
        .memory
        .write_persona(
            &department,
            &profile,
            content,
            PersonaOp::Manual,
            Some(&author_of(&ctx, &body)?),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"version": file.version, "updated_at": file.updated_at.to_rfc3339(), "team_id": profile.team_id, "agent_id": profile.agent_id}),
    ))
}

pub async fn core_count(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let (department, profile) = (ctx.department()?, profile_of(&ctx, &body)?);
    let total = s
        .services
        .memory
        .count_persona(&department, &profile)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"total": total})))
}

// ── Session turns & recall ──────────────────────────────────────────────

pub async fn session_add(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let required = |name: &str| {
        ctx.header(name).ok_or_else(|| {
            ApiError::bad_request(&ctx.request_id, format!("missing header: {name}"))
        })
    };
    let (user, session) = (
        required("x-memory-user-id")?,
        required("x-memory-session-id")?,
    );
    let team = ctx.header("x-memory-team-id");
    let stored = s
        .services
        .session
        .add_turn(
            &department,
            SessionTurnHeaders {
                user_id: &user,
                session_id: &session,
                team_id: team.as_deref(),
            },
            body,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"object_key": stored.object_key, "bytes": stored.bytes}),
    ))
}

pub async fn recall(State(s): State<SharedState>, ctx: Ctx, JsonBody(body): JsonBody) -> ApiResult {
    let (department, scope) = (ctx.department()?, ctx.scope(&body)?);
    let strategy = str_field(&body, "strategy").and_then(SearchMode::parse);
    let max = body
        .get("max_results")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let result = s
        .services
        .recall
        .recall(
            &department,
            &scope,
            &query_text(&ctx, &body)?,
            max,
            strategy,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, result))
}
