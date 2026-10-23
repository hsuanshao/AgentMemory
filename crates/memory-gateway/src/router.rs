//! Route table and cross-cutting HTTP concerns (auth, CORS, body limits).

use axum::extract::{DefaultBodyLimit, OriginalUri, Request, State};
use axum::http::{header, HeaderValue, Method};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::authz;
use crate::context::{Ctx, DEPARTMENT_ID, USER_KEY};
use crate::envelope::ApiError;
use crate::handlers::{department_exists, memory, meta, ops, skill};
use crate::state::SharedState;
use memory_application::meta::{invalid_user_key, Caller};
use memory_domain::error::DomainError;

/// Length-independent comparison so token checks do not leak timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_bearer(
    State(s): State<SharedState>,
    ctx: Ctx,
    req: Request,
    next: Next,
) -> Response {
    let open = req.uri().path() == "/health" || req.method() == Method::OPTIONS;
    if let (Some(expected), false) = (&s.settings.api_key, open) {
        let presented = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim);
        match presented {
            None => {
                return ApiError::unauthorized(
                    &ctx.request_id,
                    "Unauthorized: missing Bearer token",
                )
                .into_response()
            }
            Some(token) if !constant_time_eq(token.as_bytes(), expected.as_bytes()) => {
                return ApiError::unauthorized(&ctx.request_id, "Unauthorized: invalid token")
                    .into_response()
            }
            _ => {}
        }
    }
    next.run(req).await
}

/// Storage is created on first use, so a request for a department nobody set
/// up stops here instead of creating one. `init-admin` starts a department;
/// `auth/verify` answers "not valid" itself.
async fn known_department(
    s: &SharedState,
    ctx: &Ctx,
    path: &str,
    unknown: fn(&Ctx) -> ApiError,
) -> Result<(), ApiError> {
    // Names its department in the body and needs no header.
    if path == "/v3/department/destroy" {
        return Ok(());
    }
    let department = ctx.department()?;
    // Asked even where it does not refuse: the answer also clears a stale
    // view of a department another replica destroyed.
    let exists = department_exists(s, ctx, &department).await?;
    let exempt = matches!(
        path,
        "/v3/internal/meta/user/init-admin" | "/v3/meta/auth/verify"
    );
    if exists || exempt {
        Ok(())
    } else {
        Err(unknown(ctx))
    }
}

fn original_path(req: &Request) -> String {
    req.extensions()
        .get::<OriginalUri>()
        .map_or_else(|| req.uri().path().to_owned(), |u| u.path().to_owned())
}

/// User-key routes: an unknown department looks like a wrong key.
async fn require_department_for_user(
    State(s): State<SharedState>,
    ctx: Ctx,
    req: Request,
    next: Next,
) -> Response {
    match known_department(&s, &ctx, &original_path(&req), |ctx| {
        ctx.err(invalid_user_key())
    })
    .await
    {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

/// Bearer routes (checked after the Bearer key): a plain 404.
async fn require_department(
    State(s): State<SharedState>,
    ctx: Ctx,
    req: Request,
    next: Next,
) -> Response {
    let unknown = |ctx: &Ctx| {
        ApiError::not_found(
            &ctx.request_id,
            "unknown_department: run init-admin for it first",
        )
    };
    match known_department(&s, &ctx, &original_path(&req), unknown).await {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

/// A user of the request's department (`x-memory-user-key`), the caller of
/// every department route; `meta/auth/verify` checks a key itself. The
/// server API key does not stand in for one.
async fn require_member(
    State(s): State<SharedState>,
    ctx: Ctx,
    req: Request,
    next: Next,
) -> Response {
    if original_path(&req) == "/v3/meta/auth/verify" {
        return next.run(req).await;
    }
    match caller(&s, &ctx).await {
        Ok(_) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

/// A system admin of the request's department.
async fn require_system_admin(
    State(s): State<SharedState>,
    ctx: Ctx,
    req: Request,
    next: Next,
) -> Response {
    match caller(&s, &ctx).await {
        Ok(c) if c.is_system_admin => next.run(req).await,
        Ok(_) => ctx
            .err(DomainError::permission_denied(
                "system admins of the department only",
            ))
            .into_response(),
        Err(e) => e.into_response(),
    }
}

async fn caller(s: &SharedState, ctx: &Ctx) -> Result<Caller, ApiError> {
    let department = ctx.department()?;
    let key = ctx.user_key()?;
    s.services
        .meta
        .authenticate(&department, &key)
        .await
        .map_err(|e| ctx.err(e))
}

fn cors(origins: &[String]) -> Option<CorsLayer> {
    if origins.is_empty() {
        return None;
    }
    let allow = if origins.iter().any(|o| o == "*") {
        AllowOrigin::any()
    } else {
        AllowOrigin::list(origins.iter().filter_map(|o| HeaderValue::from_str(o).ok()))
    };
    Some(
        CorsLayer::new()
            .allow_origin(allow)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers([
                header::CONTENT_TYPE,
                header::AUTHORIZATION,
                header::HeaderName::from_static(USER_KEY),
                header::HeaderName::from_static(DEPARTMENT_ID),
            ]),
    )
}

async fn not_found(ctx: Ctx) -> Response {
    ApiError::not_found(&ctx.request_id, "Not found").into_response()
}

fn data_plane() -> Router<SharedState> {
    Router::new()
        .route("/conversation/add", post(memory::conversation_add))
        .route("/conversation/query", post(memory::conversation_query))
        .route("/conversation/search", post(memory::conversation_search))
        .route("/conversation/delete", post(memory::conversation_delete))
        .route("/conversation/count", post(memory::conversation_count))
        .route("/atomic/update", post(memory::atomic_update))
        .route("/atomic/query", post(memory::atomic_query))
        .route("/atomic/search", post(memory::atomic_search))
        .route("/atomic/delete", post(memory::atomic_delete))
        .route("/atomic/count", post(memory::atomic_count))
        .route("/scenario/ls", post(memory::scenario_ls))
        .route("/scenario/read", post(memory::scenario_read))
        .route("/scenario/write", post(memory::scenario_write))
        .route("/scenario/rm", post(memory::scenario_rm))
        .route("/scenario/count", post(memory::scenario_count))
        .route("/core/read", post(memory::core_read))
        .route("/core/write", post(memory::core_write))
        .route("/core/count", post(memory::core_count))
        .route("/recall", post(memory::recall))
}

fn skill_routes() -> Router<SharedState> {
    Router::new()
        .route("/create", post(skill::create))
        .route("/update", post(skill::update))
        .route("/patch", post(skill::patch))
        .route("/delete", post(skill::delete))
        .route("/get", post(skill::get))
        .route("/get-by-name", post(skill::get_by_name))
        .route("/list", post(skill::list))
        .route("/search", post(skill::search))
        .route("/versions", post(skill::versions))
        .route("/files/write", post(skill::files_write))
        .route("/files/remove", post(skill::files_remove))
        .route("/files/read", post(skill::files_read))
        .route("/export", post(skill::export))
        .route("/listing", post(skill::listing))
        .route("/extract", post(skill::extract))
        .route("/conversation/add", post(skill::conversation_add))
        .route("/conversation/force-archive", post(skill::force_archive))
        .route("/proposal/list", post(skill::proposal_list))
        .route("/proposal/approve", post(skill::proposal_approve))
        .route("/proposal/reject", post(skill::proposal_reject))
}

/// Department administration and operational state, for the department's
/// own users: every call carries a user key of the department, and the
/// handlers apply that user's role (`meta/*` per action, scopes and logs
/// by `ScopeVisibility`, redrive for system admins).
fn department_routes() -> Router<SharedState> {
    Router::new()
        .route("/pipeline/status", post(ops::pipeline_status))
        .route("/pipeline/dead-letter/list", post(ops::dead_letter_list))
        .route(
            "/pipeline/dead-letter/redrive",
            post(ops::dead_letter_redrive),
        )
        .route("/stats/overview", post(ops::stats_overview))
        .route(
            "/memory-generation-log/list",
            post(ops::generation_log_list),
        )
        .route("/memory-generation-log/get", post(ops::generation_log_get))
        .route(
            "/memory-generation-log/summary",
            post(ops::generation_log_summary),
        )
        .route("/meta/config/user/:action", post(meta::config_user))
        .route("/meta/:resource/:action", post(meta::dispatch))
}

/// Department-wide settings and data: system admins of the department only.
fn system_admin_routes() -> Router<SharedState> {
    Router::new()
        .route("/memory-prompt/create", post(ops::prompt_create))
        .route("/memory-prompt/get", post(ops::prompt_get))
        .route("/memory-prompt/update", post(ops::prompt_update))
        .route("/memory-prompt/delete", post(ops::prompt_delete))
        .route("/memory-prompt/set", post(ops::prompt_set))
        .route("/memory-prompt/setting/list", post(ops::prompt_settings))
        .route("/memory-prompt/log", post(ops::prompt_log))
        .route("/knowledge/create", post(ops::knowledge_create))
        .route("/knowledge/get", post(ops::knowledge_get))
        .route("/knowledge/update", post(ops::knowledge_update))
        .route("/knowledge/delete", post(ops::knowledge_delete))
        .route("/knowledge/list", post(ops::knowledge_list))
        .route("/chat-memory/clear", post(ops::chat_memory_clear))
}

/// The server operator's: bootstrap, upgrades, destruction and the
/// operator commands (`memory-server generations`, `dead-letters`), which
/// see the whole department. Only the server API key opens these.
fn operator_routes() -> Router<SharedState> {
    Router::new()
        .route("/department/destroy", post(ops::department_destroy))
        .route(
            "/internal/meta/user/init-admin",
            post(meta::internal_init_admin),
        )
        .route(
            "/internal/meta/user/list-by-department",
            post(meta::internal_list_users),
        )
        .route(
            "/internal/migrate/default-scope",
            post(meta::internal_migrate_default_scope),
        )
        .route("/internal/upgrade", post(meta::internal_upgrade))
        .route(
            "/internal/pipeline/dead-letter/list",
            post(ops::dead_letter_list),
        )
        .route(
            "/internal/pipeline/dead-letter/redrive",
            post(ops::dead_letter_redrive),
        )
        .route(
            "/internal/memory-generation-log/list",
            post(ops::generation_log_list),
        )
        .route(
            "/internal/memory-generation-log/summary",
            post(ops::generation_log_summary),
        )
}

/// Three trust zones, none of which lets one department reach another:
/// - the memory data plane, skills and session turns, authorized per user
///   and scope (`x-memory-user-key`, see `authz`);
/// - department administration and operational state, for users of that
///   department (`require_member`, `require_system_admin`);
/// - the operator's bootstrap, upgrade and destroy routes, which alone take
///   the server API key (Bearer).
pub fn build_router(state: SharedState) -> Router {
    // Layers run outermost (last added) first.
    let user_auth = || middleware::from_fn_with_state(state.clone(), authz::require_user);
    let user_department =
        || middleware::from_fn_with_state(state.clone(), require_department_for_user);
    let member = || middleware::from_fn_with_state(state.clone(), require_member);
    let session = Router::new()
        .route("/session/add", post(memory::session_add))
        .layer(DefaultBodyLimit::max(state.settings.max_session_body_bytes))
        .layer(user_auth())
        .layer(user_department());
    let user_scoped = Router::new()
        .merge(data_plane())
        .nest("/skill", skill_routes())
        .layer(DefaultBodyLimit::max(state.settings.max_body_bytes))
        .layer(user_auth())
        .layer(user_department());
    let department = department_routes()
        .layer(DefaultBodyLimit::max(state.settings.max_body_bytes))
        .layer(member())
        .layer(user_department());
    let system_admin = system_admin_routes()
        .layer(DefaultBodyLimit::max(state.settings.max_body_bytes))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_system_admin,
        ))
        .layer(member())
        .layer(user_department());
    let operator = operator_routes()
        .layer(DefaultBodyLimit::max(state.settings.max_body_bytes))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_department,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ));
    let v3 = Router::new()
        .merge(user_scoped)
        .merge(session)
        .merge(department)
        .merge(system_admin)
        .merge(operator);

    let mut app = Router::new()
        .route("/health", get(ops::health))
        .nest("/v3", v3)
        .fallback(not_found)
        .layer(TraceLayer::new_for_http());
    if let Some(cors) = cors(&state.settings.cors_origins) {
        app = app.layer(cors);
    }
    app.with_state(state)
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn token_comparison() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret!"));
    }
}
