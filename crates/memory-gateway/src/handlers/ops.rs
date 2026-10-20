//! Health, monitoring, prompts, generation logs, knowledge metadata,
//! chat-memory clearing, department destruction and legacy v1 endpoints.

use std::collections::HashMap;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use memory_application::admin::KnowledgeInput;
use memory_application::common::Page;
use memory_application::pipeline::memory_prompts::{ApplyTarget, PromptAction};
use memory_application::ports::pipeline::{
    DeadLetterFilter, ProfileState, PromptSettingFilter, PromptTarget, SessionState,
};
use memory_domain::memory::Layer;
use memory_domain::scope::{DepartmentId, SegmentId};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::context::{Ctx, JsonBody};
use crate::envelope::{ok, ApiError};
use crate::state::SharedState;

type ApiResult = Result<Response, ApiError>;

pub async fn health(State(s): State<SharedState>) -> Response {
    let worker = s
        .services
        .pipeline
        .status(&SegmentId::default_bucket(), 0)
        .await
        .ok();
    Json(json!({
        "status": if s.health.fts_available { "ok" } else { "degraded" },
        "version": s.settings.version,
        "uptime": s.started_at.elapsed().as_secs(),
        "stores": {
            "vectorStore": true,
            "embeddingService": s.health.embedding_enabled,
            "ftsAvailable": s.health.fts_available,
            "llmConfigured": s.health.llm_configured,
            "backend": s.health.storage_backend,
            "vectorIndex": s.health.vector_index,
        },
        "services": {
            "pipelineWorker": worker.map(|w| json!({"metrics": w.worker, "queue": w.queue})),
            "stateBackend": s.health.coordination_backend,
        },
    }))
    .into_response()
}

fn scope_rows(
    sessions: Vec<SessionState>,
    profiles: Vec<ProfileState>,
    every_n: u32,
) -> Vec<Value> {
    let by_profile: HashMap<String, &ProfileState> =
        profiles.iter().map(|p| (p.profile.key(), p)).collect();
    let mut seen = std::collections::HashSet::new();
    let mut rows: Vec<Value> = sessions
        .iter()
        .map(|st| {
            let key = format!("team:{}|agent:{}", st.scope.team_id, st.scope.agent_id);
            seen.insert(key.clone());
            let p = by_profile.get(&key);
            json!({
                "scope": key,
                "team_id": st.scope.team_id, "agent_id": st.scope.agent_id, "user_id": st.scope.user_id, "session_id": st.session_id,
                "conversation_count": st.conversation_count,
                "warmup_threshold": memory_domain::pipeline::Warmup(st.warmup).threshold(every_n),
                "l1_cursor": st.l1_cursor, "l1_due_at": st.l1_due_at, "last_l1_at": st.last_l1_at,
                "l2_pending": p.map(|p| p.l2_pending), "l2_due_at": p.and_then(|p| p.l2_due_at),
                "last_l2_at": p.and_then(|p| p.last_l2_at), "last_l3_at": p.and_then(|p| p.last_l3_at),
                "memories_since_persona": p.map(|p| p.memories_since_persona),
                "last_active_at": st.last_active_at,
            })
        })
        .collect();
    rows.extend(profiles.iter().filter(|p| !seen.contains(&p.profile.key())).map(|p| {
        json!({
            "scope": p.profile.key(), "team_id": p.profile.team_id, "agent_id": p.profile.agent_id,
            "user_id": null, "session_id": null, "l2_pending": p.l2_pending, "l2_due_at": p.l2_due_at,
            "last_l2_at": p.last_l2_at, "last_l3_at": p.last_l3_at, "memories_since_persona": p.memories_since_persona,
        })
    }));
    rows
}

pub async fn pipeline_status(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let limit = body
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 500) as usize;
    let status = s
        .services
        .pipeline
        .status(&department, limit)
        .await
        .map_err(|e| ctx.err(e))?;
    let every_n = memory_domain::pipeline::PipelineSettings::default().every_n_conversations;
    Ok(ok(
        &ctx.request_id,
        json!({
            "worker": status.worker,
            "queue": status.queue,
            "scopes": scope_rows(status.sessions, status.profiles, every_n),
        }),
    ))
}

/// `{kind?, limit?}` → the job filter for this department's dead letters.
fn dead_letter_filter(
    ctx: &Ctx,
    department: DepartmentId,
    body: &Value,
) -> Result<DeadLetterFilter, ApiError> {
    let kind = match body.get("kind").and_then(Value::as_str) {
        None | Some("") => None,
        Some(k @ ("l1" | "l2" | "l3" | "skill")) => Some(k.to_owned()),
        Some(other) => {
            return Err(ApiError::bad_request(
                &ctx.request_id,
                format!("kind must be l1, l2, l3 or skill, got {other}"),
            ))
        }
    };
    Ok(DeadLetterFilter {
        department: Some(department),
        kind,
    })
}

fn limit(body: &Value, default: u64, max: u64) -> usize {
    body.get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .clamp(1, max) as usize
}

pub async fn dead_letter_list(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let filter = dead_letter_filter(&ctx, ctx.department()?, &body)?;
    let items = s
        .services
        .pipeline
        .dead_letters(&filter, limit(&body, 50, 500))
        .await
        .map_err(|e| ctx.err(e))?;
    let items: Vec<Value> = items
        .into_iter()
        .map(|d| {
            json!({
                "kind": d.job.kind(),
                "job": d.job,
                "attempt": d.attempt,
                "error": d.error,
                "dead_at_ms": d.dead_at_ms,
            })
        })
        .collect();
    Ok(ok(&ctx.request_id, json!({ "items": items })))
}

pub async fn dead_letter_redrive(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let filter = dead_letter_filter(&ctx, ctx.department()?, &body)?;
    let requeued = s
        .services
        .pipeline
        .redrive_dead_letters(&filter, limit(&body, 10, 1000))
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({ "requeued": requeued })))
}

pub async fn generation_log_summary(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let hours = body
        .get("hours")
        .and_then(Value::as_i64)
        .unwrap_or(24)
        .clamp(1, 24 * 30);
    let (from_ms, items) = s
        .services
        .generation_logs
        .summary(&department, hours)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({
            "hours": hours,
            "from_ms": from_ms,
            "llm": {
                "configured": s.services.pipeline.llm_configured(),
                "model": s.services.pipeline.llm_model(),
            },
            "items": items,
        }),
    ))
}

pub async fn stats_overview(State(s): State<SharedState>, ctx: Ctx) -> ApiResult {
    let department = ctx.department()?;
    Ok(ok(
        &ctx.request_id,
        s.services
            .stats
            .overview(&department)
            .await
            .map_err(|e| ctx.err(e))?,
    ))
}

// ── Generation logs ─────────────────────────────────────────────────────

fn layer_of(ctx: &Ctx, body: &Value) -> Result<Option<Layer>, ApiError> {
    body.get("layer")
        .and_then(Value::as_str)
        .map(Layer::parse)
        .transpose()
        .map_err(|e| ctx.err(e))
}

fn ms_range(body: &Value) -> Option<(i64, i64)> {
    match (
        body.get("start_time").and_then(Value::as_i64),
        body.get("end_time").and_then(Value::as_i64),
    ) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => None,
    }
}

pub async fn generation_log_list(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let status = body
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let limit = body.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
    let page = s
        .services
        .generation_logs
        .list(
            &department,
            layer_of(&ctx, &body)?,
            status,
            ms_range(&body),
            limit,
            body.get("cursor").and_then(Value::as_str),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"items": page.items, "next_cursor": page.next_cursor}),
    ))
}

pub async fn generation_log_get(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let logs = &s.services.generation_logs;
    let log = match (
        body.get("log_id").and_then(Value::as_str),
        body.get("memory_id").and_then(Value::as_str),
        layer_of(&ctx, &body)?,
    ) {
        (Some(id), None, _) => logs.get(&department, id).await,
        (None, Some(memory), Some(layer)) => logs.find_by_memory(&department, layer, memory).await,
        _ => {
            return Err(ApiError::bad_request(
                &ctx.request_id,
                "provide log_id, or memory_id with layer",
            ))
        }
    }
    .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, log))
}

// ── Memory prompts ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PromptCreate {
    name: String,
    layer: String,
    prompt: String,
}

pub async fn prompt_create(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let input: PromptCreate = ctx.parse(&body)?;
    let layer = Layer::parse(&input.layer).map_err(|e| ctx.err(e))?;
    let p = s
        .services
        .prompts
        .create(&department, &input.name, layer, &input.prompt)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"memory_prompt_id": p.memory_prompt_id, "version": p.version, "created_at_ms": p.created_at_ms}),
    ))
}

fn order_newest(body: &Value) -> bool {
    body.get("time_order").and_then(Value::as_str) != Some("asc")
}

fn list_page(body: &Value) -> Page {
    let n = |k: &str| body.get(k).and_then(Value::as_u64).map(|v| v as usize);
    Page::standard(n("limit"), n("offset"))
}

pub async fn prompt_get(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let prompts = &s.services.prompts;
    let str_of = |k: &str| body.get(k).and_then(Value::as_str);
    if let Some(id) = str_of("memory_prompt_id") {
        return Ok(ok(
            &ctx.request_id,
            prompts.get(&department, id).await.map_err(|e| ctx.err(e))?,
        ));
    }
    let layer = layer_of(&ctx, &body)?.ok_or_else(|| {
        ApiError::bad_request(&ctx.request_id, "memory_prompt_id or layer is required")
    })?;
    if str_of("team_id").is_some() || str_of("agent_id").is_some() {
        let resolved = prompts
            .resolve(
                &department,
                str_of("team_id").unwrap_or(""),
                str_of("agent_id").unwrap_or(""),
                layer,
            )
            .await
            .map_err(|e| ctx.err(e))?;
        let data = match resolved {
            Some(c) => {
                json!({"source": c.source, "memory_prompt_id": c.memory_prompt_id, "version": c.version, "prompt": c.prompt, "layer": layer})
            }
            None => {
                json!({"source": "builtin", "memory_prompt_id": format!("builtin:{}", layer.as_str()), "prompt": memory_application::pipeline::prompts::builtin(layer), "layer": layer})
            }
        };
        return Ok(ok(&ctx.request_id, data));
    }
    let items = prompts
        .list(
            &department,
            Some(layer),
            list_page(&body),
            order_newest(&body),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"items": items})))
}

pub async fn prompt_update(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let id = body
        .get("memory_prompt_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let p = s
        .services
        .prompts
        .update(
            &department,
            id,
            body.get("name").and_then(Value::as_str),
            body.get("prompt").and_then(Value::as_str),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"memory_prompt_id": p.memory_prompt_id, "version": p.version, "updated_at_ms": p.updated_at_ms}),
    ))
}

pub async fn prompt_delete(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let mut ids: Vec<String> = ctx.parse(body.get("memory_prompt_ids").unwrap_or(&Value::Null))?;
    ids.sort();
    ids.dedup();
    if ids.is_empty() || ids.len() > 100 {
        return Err(ApiError::bad_request(
            &ctx.request_id,
            "memory_prompt_ids must contain 1-100 ids",
        ));
    }
    let deleted = s
        .services
        .prompts
        .delete(&department, &ids)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"deleted_count": deleted, "memory_prompt_ids": ids}),
    ))
}

#[derive(Deserialize)]
struct PromptSet {
    action: String,
    layer: String,
    memory_prompt_id: Option<String>,
    team_id: Option<String>,
    #[serde(default)]
    agent_ids: Vec<String>,
}

pub async fn prompt_set(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let input: PromptSet = ctx.parse(&body)?;
    let layer = Layer::parse(&input.layer).map_err(|e| ctx.err(e))?;
    let action = match (input.action.as_str(), input.memory_prompt_id) {
        ("apply", Some(id)) => PromptAction::Apply {
            memory_prompt_id: id,
        },
        ("apply", None) => {
            return Err(ApiError::bad_request(
                &ctx.request_id,
                "memory_prompt_id is required to apply",
            ))
        }
        ("clear", _) => PromptAction::Clear,
        _ => {
            return Err(ApiError::bad_request(
                &ctx.request_id,
                "action must be apply or clear",
            ))
        }
    };
    let affected = s
        .services
        .prompts
        .set(
            &department,
            layer,
            action,
            ApplyTarget {
                team_id: input.team_id,
                agent_ids: input.agent_ids,
            },
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"affected": affected})))
}

fn setting_filter(ctx: &Ctx, body: &Value) -> Result<PromptSettingFilter, ApiError> {
    let s = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_owned);
    let target_type = match s("target_type").as_deref() {
        None => None,
        Some("department") => Some(PromptTarget::Department),
        Some("team") => Some(PromptTarget::Team),
        Some("agent") => Some(PromptTarget::Agent),
        Some(other) => {
            return Err(ApiError::bad_request(
                &ctx.request_id,
                format!("invalid target_type: {other}"),
            ))
        }
    };
    Ok(PromptSettingFilter {
        memory_prompt_id: s("memory_prompt_id"),
        target_type,
        team_id: s("team_id"),
        agent_id: s("agent_id"),
        layer: layer_of(ctx, body)?,
    })
}

pub async fn prompt_settings(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let items = s
        .services
        .prompts
        .list_settings(&department, &setting_filter(&ctx, &body)?, list_page(&body))
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"items": items})))
}

pub async fn prompt_log(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let items = s
        .services
        .prompts
        .logs(
            &department,
            &setting_filter(&ctx, &body)?,
            body.get("action").and_then(Value::as_str),
            ms_range(&body),
            list_page(&body),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, json!({"items": items})))
}

// ── Knowledge, chat memory, department ────────────────────────────────────

pub async fn knowledge_create(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let input: KnowledgeInput = ctx.parse(&body)?;
    Ok(ok(
        &ctx.request_id,
        s.services
            .knowledge
            .create(&department, input)
            .await
            .map_err(|e| ctx.err(e))?,
    ))
}

pub async fn knowledge_get(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let id = body
        .get("knowledge_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let team = body.get("team_id").and_then(Value::as_str);
    Ok(ok(
        &ctx.request_id,
        s.services
            .knowledge
            .get(&department, id, team)
            .await
            .map_err(|e| ctx.err(e))?,
    ))
}

pub async fn knowledge_update(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let input: KnowledgeInput = ctx.parse(&body)?;
    Ok(ok(
        &ctx.request_id,
        s.services
            .knowledge
            .update(&department, input)
            .await
            .map_err(|e| ctx.err(e))?,
    ))
}

pub async fn knowledge_delete(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let ids: Vec<String> = ctx.parse(body.get("knowledge_ids").unwrap_or(&Value::Null))?;
    let team = body.get("team_id").and_then(Value::as_str);
    Ok(ok(
        &ctx.request_id,
        s.services
            .knowledge
            .delete(&department, &ids, team)
            .await
            .map_err(|e| ctx.err(e))?,
    ))
}

pub async fn knowledge_list(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let team = body
        .get("team_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request(&ctx.request_id, "team_id is required"))?;
    let ids: Option<Vec<String>> = body
        .get("knowledge_ids")
        .map(|v| ctx.parse(v))
        .transpose()?;
    let p = body.get("pagination").cloned().unwrap_or(Value::Null);
    let page = Page::clamped(
        p.get("limit").and_then(Value::as_u64).map(|v| v as usize),
        p.get("offset").and_then(Value::as_u64).map(|v| v as usize),
        20,
        1000,
    );
    let (items, total) = s
        .services
        .knowledge
        .list(
            &department,
            team,
            body.get("type").and_then(Value::as_str),
            ids.as_deref(),
            page,
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"items": items, "total": total, "limit": page.limit, "offset": page.offset}),
    ))
}

pub async fn chat_memory_clear(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let ids: Vec<String> = ctx.parse(body.get("memory_ids").unwrap_or(&Value::Null))?;
    let items = s
        .services
        .chat_memory
        .clear(&department, &ids)
        .await
        .map_err(|e| ctx.err(e))?;
    let all_cleared = items.iter().all(|i| i.cleared);
    Ok(ok(
        &ctx.request_id,
        json!({"items": items, "all_cleared": all_cleared}),
    ))
}

pub async fn department_destroy(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let raw = body
        .get("department_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request(&ctx.request_id, "department_id is required"))?;
    let department: DepartmentId =
        SegmentId::parse("department_id", raw).map_err(|e| ctx.err(e))?;
    let cleaned = s
        .services
        .lifecycle
        .destroy(&department)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(
        &ctx.request_id,
        json!({"department_id": raw, "cleaned": cleaned}),
    ))
}
