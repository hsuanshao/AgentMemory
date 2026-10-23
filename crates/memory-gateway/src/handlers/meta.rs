//! `/v3/meta/*` (user-key authenticated) and `/v3/internal/meta/*`.

use axum::extract::{Path, State};
use axum::response::Response;
use memory_application::meta::{
    AddMemberInput, AppendParticipationInput, BindingInput, Caller, CreateAgentInput,
    CreateAssetInput, CreateKeyInput, CreateTaskInput, CreateTeamInput, CreateUserInput,
    GrantInput, ListAccessibleInput, ListAgentsInput, ListAssetsInput, ListParticipationInput,
    ListTasksInput, ListUsersInput, Pagination, UpdateAgentInput, UpdateAssetInput, UpdateKeyInput,
    UpdateTaskInput, UpdateTeamInput, VerifyResult,
};
use memory_application::ports::meta::UserFilter;
use memory_application::redaction::{
    CreateRedactionRuleInput, TestRedactionInput, UpdateRedactionRuleInput,
};
use memory_domain::error::DomainError;
use memory_domain::meta::{UserStatus, UserType};
use memory_domain::scope::DepartmentId;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::context::{Ctx, JsonBody};
use crate::envelope::{ok, ApiError};
use crate::handlers::department_exists;
use crate::state::SharedState;

type ApiResult = Result<Response, ApiError>;

async fn caller(s: &SharedState, ctx: &Ctx, department: &DepartmentId) -> Result<Caller, ApiError> {
    s.services
        .meta
        .authenticate(department, &ctx.user_key()?)
        .await
        .map_err(|e| ctx.err(e))
}

fn str_of<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}

fn required<'a>(ctx: &Ctx, body: &'a Value, key: &str) -> Result<&'a str, ApiError> {
    str_of(body, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request(&ctx.request_id, format!("{key} is required")))
}

fn id_list(ctx: &Ctx, body: &Value, key: &str) -> Result<Vec<String>, ApiError> {
    let ids: Vec<String> = ctx.parse(body.get(key).unwrap_or(&Value::Null))?;
    if ids.is_empty() || ids.len() > 100 {
        return Err(ApiError::bad_request(
            &ctx.request_id,
            format!("{key} must contain 1-100 ids"),
        ));
    }
    Ok(ids)
}

fn pagination(ctx: &Ctx, body: &Value) -> Result<Pagination, ApiError> {
    ctx.parse(body)
}

/// Accepts either `user_id` or `user_key` (resolved to its user).
/// The user a query is about: `user_id`, or the owner of `user_key`, or the
/// caller. Asking about someone else requires system admin, otherwise the
/// owner short-circuit of the access policy would leak their assets.
async fn subject_user(
    s: &SharedState,
    ctx: &Ctx,
    department: &DepartmentId,
    body: &Value,
    who: &Caller,
) -> Result<String, ApiError> {
    let subject = match (str_of(body, "user_id"), str_of(body, "user_key")) {
        (Some(id), _) => id.to_owned(),
        (None, Some(key)) => {
            s.services
                .meta
                .authenticate(department, key)
                .await
                .map_err(|e| ctx.err(e))?
                .user_id
        }
        (None, None) => who.user_id.clone(),
    };
    if subject != who.user_id && !who.is_system_admin {
        return Err(ctx.err(DomainError::permission_denied(
            "can only query on your own behalf",
        )));
    }
    Ok(subject)
}

fn input<T: DeserializeOwned>(ctx: &Ctx, body: &Value) -> Result<T, ApiError> {
    ctx.parse(body)
}

/// Dispatches `/v3/meta/{resource}/{action}`.
pub async fn dispatch(
    State(s): State<SharedState>,
    ctx: Ctx,
    Path((resource, action)): Path<(String, String)>,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    if (resource.as_str(), action.as_str()) == ("auth", "verify") {
        let key = required(&ctx, &body, "user_key")?;
        // Same answer as a wrong key: callers cannot probe department names.
        if !department_exists(&s, &ctx, &department).await? {
            return Ok(ok(
                &ctx.request_id,
                VerifyResult {
                    valid: false,
                    user: None,
                },
            ));
        }
        return Ok(ok(
            &ctx.request_id,
            s.services
                .meta
                .verify(&department, key)
                .await
                .map_err(|e| ctx.err(e))?,
        ));
    }
    let who = caller(&s, &ctx, &department).await?;
    let meta = &s.services.meta;
    let i = &department;
    let e = |err| ctx.err(err);
    let data: Value = match (resource.as_str(), action.as_str()) {
        ("user", "create") | ("user", "create-with-key") => json!(meta
            .create_user(i, &who, input::<CreateUserInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("user", "get") => json!(meta
            .get_user(i, &who, str_of(&body, "user_id"), str_of(&body, "user_key"))
            .await
            .map_err(e)?),
        ("user", "delete") => json!(meta
            .delete_users(i, &who, &id_list(&ctx, &body, "user_ids")?)
            .await
            .map_err(e)?),
        ("user", "list") => json!(meta
            .list_users(i, &who, input::<ListUsersInput>(&ctx, &body)?)
            .await
            .map_err(e)?),

        ("user-key", "create") => json!(meta
            .create_key(i, &who, input::<CreateKeyInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("user-key", "list") => json!(meta
            .list_keys(
                i,
                &who,
                str_of(&body, "user_id").map(str::to_owned),
                pagination(&ctx, &body)?
            )
            .await
            .map_err(e)?),
        ("user-key", "get") => json!(meta
            .get_key(i, &who, required(&ctx, &body, "key_id")?)
            .await
            .map_err(e)?),
        ("user-key", "revoke") => json!(meta
            .revoke_key(i, &who, required(&ctx, &body, "key_id")?)
            .await
            .map_err(e)?),
        ("user-key", "update") => json!(meta
            .update_key(i, &who, input::<UpdateKeyInput>(&ctx, &body)?)
            .await
            .map_err(e)?),

        ("team", "create") => json!(meta
            .create_team(i, &who, input::<CreateTeamInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("team", "get") => json!(meta
            .read_team(i, &who, required(&ctx, &body, "team_id")?)
            .await
            .map_err(e)?),
        ("team", "update") => json!(meta
            .update_team(i, &who, input::<UpdateTeamInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("team", "delete") => json!(meta
            .delete_teams(i, &who, &id_list(&ctx, &body, "team_ids")?)
            .await
            .map_err(e)?),
        ("team", "list") => {
            let all = body.get("all").and_then(Value::as_bool) == Some(true);
            if all && !who.is_system_admin {
                return Err(ctx.err(DomainError::permission_denied(
                    "system admin required to list all teams",
                )));
            }
            let user = match all {
                true => None,
                false => Some(subject_user(&s, &ctx, i, &body, &who).await?),
            };
            json!(meta
                .list_teams(
                    i,
                    user.as_deref(),
                    str_of(&body, "name"),
                    pagination(&ctx, &body)?
                )
                .await
                .map_err(e)?)
        }

        ("team-member", "add") => json!(meta
            .add_member(i, &who, input::<AddMemberInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("team-member", "remove") => json!(meta
            .remove_member(
                i,
                &who,
                required(&ctx, &body, "team_id")?,
                required(&ctx, &body, "user_id")?
            )
            .await
            .map_err(e)?),
        ("team-member", "list") => json!(meta
            .list_members(
                i,
                &who,
                required(&ctx, &body, "team_id")?,
                pagination(&ctx, &body)?
            )
            .await
            .map_err(e)?),
        ("team-member", "get") => json!(meta
            .get_member(
                i,
                &who,
                required(&ctx, &body, "team_id")?,
                required(&ctx, &body, "user_id")?
            )
            .await
            .map_err(e)?),

        ("agent", "create") => json!(meta
            .create_agent(i, &who, input::<CreateAgentInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("agent", "get") => json!(meta
            .read_agent(i, &who, required(&ctx, &body, "agent_id")?)
            .await
            .map_err(e)?),
        ("agent", "update") => json!(meta
            .update_agent(i, &who, input::<UpdateAgentInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("agent", "delete") => json!(meta
            .delete_agents(i, &who, &id_list(&ctx, &body, "agent_ids")?)
            .await
            .map_err(e)?),
        ("agent", "list") => {
            let mut filter: ListAgentsInput = input(&ctx, &body)?;
            if filter.owner_user_id.is_none() {
                if let Some(key) = str_of(&body, "owner_user_key") {
                    filter.owner_user_id =
                        Some(meta.authenticate(i, key).await.map_err(e)?.user_id);
                }
            }
            json!(meta.read_agents(i, &who, filter).await.map_err(e)?)
        }
        ("agent", "archive") => json!(meta
            .archive_agent(i, &who, required(&ctx, &body, "agent_id")?)
            .await
            .map_err(e)?),

        ("task", "create") => json!(meta
            .create_task(i, &who, input::<CreateTaskInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("task", "get") => json!(meta
            .read_task(i, &who, required(&ctx, &body, "task_id")?)
            .await
            .map_err(e)?),
        ("task", "update") => json!(meta
            .update_task(i, &who, input::<UpdateTaskInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("task", "delete") => json!(meta
            .delete_tasks(i, &who, &id_list(&ctx, &body, "task_ids")?)
            .await
            .map_err(e)?),
        ("task", "list") => json!(meta
            .read_tasks(i, &who, input::<ListTasksInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("task", "archive") => json!(meta
            .archive_task(i, &who, required(&ctx, &body, "task_id")?)
            .await
            .map_err(e)?),

        ("task-agent", "link") => json!(meta
            .link_task_agent(
                i,
                &who,
                required(&ctx, &body, "task_id")?,
                required(&ctx, &body, "agent_id")?,
                str_of(&body, "role_in_task").map(str::to_owned)
            )
            .await
            .map_err(e)?),
        ("task-agent", "unlink") => json!(meta
            .unlink_task_agent(
                i,
                &who,
                required(&ctx, &body, "task_id")?,
                required(&ctx, &body, "agent_id")?
            )
            .await
            .map_err(e)?),
        ("task-agent", "list") => json!(meta
            .read_task_agents(
                i,
                &who,
                required(&ctx, &body, "task_id")?,
                pagination(&ctx, &body)?
            )
            .await
            .map_err(e)?),

        ("participation-log", "append") => json!(meta
            .append_participation(i, &who, input::<AppendParticipationInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("participation-log", "list") => json!(meta
            .list_participation(i, &who, input::<ListParticipationInput>(&ctx, &body)?)
            .await
            .map_err(e)?),

        ("asset", "create") => json!(meta
            .create_asset(i, &who, input::<CreateAssetInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("asset", "get") => json!(meta
            .get_asset(i, &who, required(&ctx, &body, "asset_id")?)
            .await
            .map_err(e)?),
        ("asset", "update") => json!(meta
            .update_asset(i, &who, input::<UpdateAssetInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("asset", "delete") => json!(meta
            .delete_assets(i, &who, &id_list(&ctx, &body, "asset_ids")?)
            .await
            .map_err(e)?),
        ("asset", "list") => json!(meta
            .list_assets(i, &who, input::<ListAssetsInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("asset", "list-accessible") => {
            let user = subject_user(&s, &ctx, i, &body, &who).await?;
            json!(meta
                .list_accessible(i, &user, input::<ListAccessibleInput>(&ctx, &body)?)
                .await
                .map_err(e)?)
        }
        ("asset", "touch-usage") => json!(meta
            .touch_asset(i, &who, required(&ctx, &body, "asset_id")?)
            .await
            .map_err(e)?),

        ("agent-fixed-asset", "set") => {
            let bindings: Vec<BindingInput> =
                ctx.parse(body.get("bindings").unwrap_or(&Value::Null))?;
            json!(meta
                .set_bindings(i, &who, required(&ctx, &body, "agent_id")?, bindings)
                .await
                .map_err(e)?)
        }
        ("agent-fixed-asset", "list") => json!(meta
            .read_bindings(
                i,
                &who,
                required(&ctx, &body, "agent_id")?,
                pagination(&ctx, &body)?
            )
            .await
            .map_err(e)?),
        ("agent-fixed-asset", "list-with-detail") => {
            let flag = |k: &str| body.get(k).and_then(Value::as_bool).unwrap_or(false);
            json!(meta
                .list_bindings_with_detail(
                    i,
                    &who,
                    required(&ctx, &body, "agent_id")?,
                    flag("apply_visibility_filter"),
                    flag("touch_usage"),
                    pagination(&ctx, &body)?
                )
                .await
                .map_err(e)?)
        }
        ("agent-fixed-asset", "summary-by-agents") => {
            let mut ids = id_list(&ctx, &body, "agent_ids")?;
            ids.sort();
            ids.dedup();
            json!(meta
                .read_binding_summary(i, &who, &ids, str_of(&body, "asset_id"))
                .await
                .map_err(e)?)
        }

        ("acl", "grant") => {
            let mut grant: GrantInput = input(&ctx, &body)?;
            if grant.granted_by.is_none() {
                if let Some(key) = str_of(&body, "granted_by_key") {
                    grant.granted_by = Some(meta.authenticate(i, key).await.map_err(e)?.user_id);
                }
            }
            json!(meta.grant(i, &who, grant).await.map_err(e)?)
        }
        ("acl", "revoke") => json!(meta
            .revoke(i, &who, required(&ctx, &body, "id")?)
            .await
            .map_err(e)?),
        ("acl", "list") => json!(meta
            .list_acl(
                i,
                &who,
                required(&ctx, &body, "asset_id")?,
                pagination(&ctx, &body)?
            )
            .await
            .map_err(e)?),
        ("acl", "check") => {
            let user = subject_user(&s, &ctx, i, &body, &who).await?;
            json!(meta
                .check(
                    i,
                    required(&ctx, &body, "asset_id")?,
                    required(&ctx, &body, "action")?,
                    &user,
                    str_of(&body, "agent_id")
                )
                .await
                .map_err(e)?)
        }

        ("department-quota", "get") => meta.department_quota(i).await.map_err(e)?,

        // System admins only; checked in the service.
        ("redaction-rule", "list") => json!(s.services.redaction.list(i, &who).await.map_err(e)?),
        ("redaction-rule", "create") => json!(s
            .services
            .redaction
            .create(i, &who, input::<CreateRedactionRuleInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("redaction-rule", "update") => json!(s
            .services
            .redaction
            .update(i, &who, input::<UpdateRedactionRuleInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("redaction-rule", action @ ("disable" | "enable")) => json!(s
            .services
            .redaction
            .set_enabled(
                i,
                &who,
                required(&ctx, &body, "rule_id")?,
                action == "enable"
            )
            .await
            .map_err(e)?),
        ("redaction-rule", "test") => json!(s
            .services
            .redaction
            .test(i, &who, input::<TestRedactionInput>(&ctx, &body)?)
            .await
            .map_err(e)?),
        ("config", "user-get") => {
            json!({"items": meta.get_config(i, &who, required(&ctx, &body, "user_id")?, required(&ctx, &body, "module")?, str_of(&body, "param_name")).await.map_err(e)?})
        }
        ("config", "user-set") => {
            let params: std::collections::BTreeMap<String, String> =
                ctx.parse(body.get("params").unwrap_or(&Value::Null))?;
            let items = meta
                .set_config(
                    i,
                    &who,
                    required(&ctx, &body, "user_id")?,
                    required(&ctx, &body, "module")?,
                    params.into_iter().collect(),
                )
                .await
                .map_err(e)?;
            json!({"items": items})
        }
        _ => {
            return Err(ApiError::not_found(
                &ctx.request_id,
                format!("unknown meta route: {resource}/{action}"),
            ))
        }
    };
    Ok(ok(&ctx.request_id, data))
}

/// `/v3/meta/config/user/{get,set}` has one more path segment.
pub async fn config_user(
    state: State<SharedState>,
    ctx: Ctx,
    Path(action): Path<String>,
    body: JsonBody,
) -> ApiResult {
    dispatch(
        state,
        ctx,
        Path(("config".into(), format!("user-{action}"))),
        body,
    )
    .await
}

pub async fn internal_init_admin(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let username = required(&ctx, &body, "username")?;
    // The one place a department is created.
    s.services
        .departments
        .create(&department)
        .await
        .map_err(|e| ctx.err(e))?;
    let result = s
        .services
        .meta
        .init_admin(
            &department,
            username,
            str_of(&body, "user_key").map(str::to_owned),
        )
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, result))
}

pub async fn internal_list_users(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let filter = UserFilter {
        user_ids: body.get("user_ids").map(|v| ctx.parse(v)).transpose()?,
        user_type: str_of(&body, "user_type")
            .map(UserType::parse)
            .transpose()
            .map_err(|e| ctx.err(e))?,
        status: str_of(&body, "status")
            .map(UserStatus::parse)
            .transpose()
            .map_err(|e| ctx.err(e))?,
        ..Default::default()
    };
    let page = s
        .services
        .meta
        .list_users_internal(&department, filter, pagination(&ctx, &body)?)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, page))
}

/// Upgrade helper: moves the legacy `default` buckets to a real
/// team/agent/user. `dry_run: true` only reports what would move.
pub async fn internal_migrate_default_scope(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let report = s
        .services
        .migration
        .migrate_default(&department, ctx.parse(&body)?)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, report))
}

/// One-step upgrade from a pre-0.1.1 install (see `memory_application::upgrade`):
/// ensures team and agent, moves the legacy data, optionally issues a key.
pub async fn internal_upgrade(
    State(s): State<SharedState>,
    ctx: Ctx,
    JsonBody(body): JsonBody,
) -> ApiResult {
    let department = ctx.department()?;
    let report = s
        .services
        .upgrade
        .run(&department, ctx.parse(&body)?)
        .await
        .map_err(|e| ctx.err(e))?;
    Ok(ok(&ctx.request_id, report))
}
