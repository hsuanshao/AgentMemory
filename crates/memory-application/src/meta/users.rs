//! Users, user keys, bootstrap, configuration and quotas.

use memory_domain::error::{DomainError, DomainResult, ErrorKind};
use memory_domain::meta::{KeyStatus, User, UserKey, UserStatus, UserType, USER_KEY_PREFIX};
use memory_domain::scope::DepartmentId;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    denied, limit_exceeded, page_of, require_non_empty, require_system_admin, Caller, MetaPage,
    MetaService, Pagination,
};
use crate::common::BatchDeleteResult;
use crate::ports::meta::{ConfigParam, UserFilter};

#[derive(Debug, Clone, Serialize)]
pub struct UserPublic {
    pub user_id: String,
    pub user_type: UserType,
    pub username: String,
    pub status: UserStatus,
    pub created_at: String,
}

impl From<User> for UserPublic {
    fn from(u: User) -> Self {
        Self {
            user_id: u.user_id,
            user_type: u.user_type,
            username: u.username,
            status: u.status,
            created_at: u.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedUser {
    pub user_id: String,
    pub user_type: UserType,
    pub username: String,
    pub created_at: String,
    pub default_user_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserKeyPublic {
    pub key_id: String,
    pub user_id: String,
    pub key_prefix: String,
    pub name: Option<String>,
    pub status: KeyStatus,
    pub is_default: bool,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_value: Option<String>,
}

impl UserKeyPublic {
    fn masked(key: UserKey) -> Self {
        Self {
            key_prefix: key.masked(),
            key_id: key.key_id,
            user_id: key.user_id,
            name: key.name,
            status: key.status,
            is_default: key.is_default,
            last_used_at: key.last_used_at,
            expires_at: key.expires_at,
            created_at: key.created_at,
            key_value: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyResult {
    pub valid: bool,
    pub user: Option<UserPublic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootstrapResult {
    pub user_id: String,
    pub user_key: String,
    pub team_id: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserInput {
    pub username: String,
    pub user_id: Option<String>,
    pub user_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListUsersInput {
    pub team_id: Option<String>,
    pub user_ids: Option<Vec<String>>,
    pub username: Option<String>,
    #[serde(flatten)]
    pub pagination: Pagination,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeyInput {
    pub user_id: Option<String>,
    pub name: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateKeyInput {
    pub key_id: String,
    pub name: Option<String>,
    pub expires_at: Option<String>,
}

/// The 401 for a user key that does not authenticate (also used for a
/// department that does not exist, so the two cannot be told apart).
pub fn invalid_user_key() -> DomainError {
    DomainError::new(
        ErrorKind::Unauthenticated,
        "unauthorized",
        "invalid_user_key",
    )
}

impl MetaService {
    fn new_key(
        &self,
        user_id: &str,
        value: String,
        is_default: bool,
        name: Option<String>,
        expires_at: Option<String>,
    ) -> UserKey {
        UserKey {
            key_id: self.entity_id("uky"),
            user_id: user_id.to_owned(),
            key_value: value,
            name,
            status: KeyStatus::Active,
            is_default,
            last_used_at: None,
            expires_at,
            created_at: self.now(),
        }
    }

    fn generate_key_value(&self) -> String {
        format!("{USER_KEY_PREFIX}{}", self.ids.secret(24))
    }

    async fn user_for_key(
        &self,
        department: &DepartmentId,
        value: &str,
    ) -> DomainResult<Option<User>> {
        let Some(key) = self.store.find_key_by_value(department, value).await? else {
            return Ok(None);
        };
        let now = self.now();
        if key.status != KeyStatus::Active || key.is_expired(&now) {
            return Ok(None);
        }
        let user = self
            .store
            .get_user(department, &key.user_id)
            .await?
            .filter(|u| u.status == UserStatus::Active);
        if user.is_some() {
            self.store.touch_key(department, &key.key_id, &now).await?;
        }
        Ok(user)
    }

    /// Resolves `x-memory-user-key` to the caller, or 401.
    pub async fn authenticate(
        &self,
        department: &DepartmentId,
        user_key: &str,
    ) -> DomainResult<Caller> {
        let user = self
            .user_for_key(department, user_key)
            .await?
            .ok_or_else(invalid_user_key)?;
        Ok(Caller::from_user(&user))
    }

    /// `auth/verify`: never 401, the answer is in `valid`.
    pub async fn verify(
        &self,
        department: &DepartmentId,
        user_key: &str,
    ) -> DomainResult<VerifyResult> {
        let user = self.user_for_key(department, user_key).await?;
        Ok(VerifyResult {
            valid: user.is_some(),
            user: user.map(UserPublic::from),
        })
    }

    async fn insert_user(
        &self,
        department: &DepartmentId,
        input: CreateUserInput,
        user_type: UserType,
    ) -> DomainResult<CreatedUser> {
        require_non_empty("username", &input.username)?;
        if self.store.count_users(department).await? >= self.settings.max_users_per_department {
            return Err(limit_exceeded(
                "user_limit_exceeded",
                "user limit reached for this department",
            ));
        }
        if self
            .store
            .find_user_by_name(department, &input.username)
            .await?
            .is_some()
        {
            return Err(DomainError::conflict(
                "duplicate_entry",
                format!("username exists: {}", input.username),
            ));
        }
        let key_value = match input.user_key {
            Some(k) if !k.starts_with(USER_KEY_PREFIX) => {
                return Err(DomainError::invalid("user_key must start with sk-mem-"))
            }
            Some(k) => {
                if self
                    .store
                    .find_key_by_value(department, &k)
                    .await?
                    .is_some()
                {
                    return Err(DomainError::conflict(
                        "duplicate_user_key",
                        "user key already exists",
                    ));
                }
                k
            }
            None => self.generate_key_value(),
        };
        let now = self.now();
        let user = User {
            user_id: input
                .user_id
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| self.entity_id("usr")),
            username: input.username,
            user_type,
            status: UserStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            metadata_json: json!({}),
        };
        let key = self.new_key(
            &user.user_id,
            key_value.clone(),
            true,
            Some("default".into()),
            None,
        );
        self.store.create_user(department, &user, &key).await?;
        Ok(CreatedUser {
            user_id: user.user_id,
            user_type: user.user_type,
            username: user.username,
            created_at: user.created_at,
            default_user_key: key_value,
        })
    }

    pub async fn create_user(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: CreateUserInput,
    ) -> DomainResult<CreatedUser> {
        require_system_admin(caller)?;
        self.insert_user(department, input, UserType::Normal).await
    }

    /// First-run bootstrap: a system admin plus a default team and agent.
    pub async fn init_admin(
        &self,
        department: &DepartmentId,
        username: &str,
        user_key: Option<String>,
    ) -> DomainResult<BootstrapResult> {
        if self.store.count_users(department).await? > 0 {
            return Err(DomainError::conflict(
                "already_initialized",
                "department already has users",
            ));
        }
        let admin = self
            .insert_user(
                department,
                CreateUserInput {
                    username: username.to_owned(),
                    user_id: None,
                    user_key,
                },
                UserType::SystemAdmin,
            )
            .await?;
        let caller = Caller {
            user_id: admin.user_id.clone(),
            is_system_admin: true,
        };
        let team = self
            .create_team(
                department,
                &caller,
                super::CreateTeamInput {
                    name: "default-team".into(),
                    owner_user_id: admin.user_id.clone(),
                    description: Some("Created at bootstrap".into()),
                    metadata_json: None,
                },
            )
            .await
            .ok();
        let agent = match &team {
            Some(t) => self
                .create_agent(
                    department,
                    &caller,
                    super::CreateAgentInput {
                        team_id: t.team_id.clone(),
                        owner_user_id: admin.user_id.clone(),
                        name: format!("default-agent-{username}"),
                        description: None,
                        prompt: Some(String::new()),
                        visibility: Some("team".into()),
                        metadata_json: Some(json!({"ui": {"role_prompt": "", "rules_prompt": ""}})),
                    },
                )
                .await
                .ok(),
            None => None,
        };
        Ok(BootstrapResult {
            user_id: admin.user_id,
            user_key: admin.default_user_key,
            team_id: team.map(|t| t.team_id),
            agent_id: agent.map(|a| a.agent_id),
        })
    }

    pub async fn get_user(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        user_id: Option<&str>,
        user_key: Option<&str>,
    ) -> DomainResult<UserPublic> {
        let user = match (user_id, user_key) {
            (Some(id), None) => self.store.get_user(department, id).await?,
            (None, Some(key)) => self.user_for_key(department, key).await?,
            _ => {
                return Err(DomainError::invalid(
                    "exactly one of user_id or user_key is required",
                ))
            }
        }
        .ok_or_else(|| DomainError::not_found("user", user_id.unwrap_or("<key>")))?;
        if user.user_id != caller.user_id && !caller.is_system_admin {
            return Err(denied("can only read yourself"));
        }
        Ok(user.into())
    }

    pub async fn delete_users(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        require_system_admin(caller)?;
        for id in ids {
            let user = self.store.get_user(department, id).await?;
            if user.is_some_and(|u| u.user_type == UserType::SystemAdmin) {
                return Err(DomainError::conflict(
                    "last_system_admin",
                    "the system admin cannot be deleted",
                ));
            }
        }
        self.store.delete_users(department, ids).await
    }

    pub async fn list_users(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: ListUsersInput,
    ) -> DomainResult<MetaPage<UserPublic>> {
        let page = input.pagination.page();
        let mut filter = UserFilter {
            team_id: input.team_id.clone(),
            user_ids: input.user_ids,
            username: input.username,
            ..Default::default()
        };
        match (&input.team_id, caller.is_system_admin) {
            (None, false) => return Err(denied("system admin required to list all users")),
            (Some(team), false) => {
                let member = self.require_member(department, team, caller).await?;
                if !member.is_admin() {
                    filter.user_ids = Some(vec![caller.user_id.clone()]);
                }
            }
            _ => {}
        }
        let (users, total) = self.store.list_users(department, &filter, page).await?;
        let visible: Vec<UserPublic> = users
            .into_iter()
            .filter(|u| {
                caller.is_system_admin
                    || u.user_type != UserType::SystemAdmin
                    || u.user_id == caller.user_id
            })
            .map(Into::into)
            .collect();
        Ok(page_of(visible, total, page))
    }

    pub async fn list_users_internal(
        &self,
        department: &DepartmentId,
        filter: UserFilter,
        pagination: Pagination,
    ) -> DomainResult<MetaPage<UserPublic>> {
        let page = pagination.page();
        let (users, total) = self.store.list_users(department, &filter, page).await?;
        Ok(page_of(
            users.into_iter().map(Into::into).collect(),
            total,
            page,
        ))
    }

    fn require_self_or_admin(caller: &Caller, user_id: &str) -> DomainResult<()> {
        if caller.user_id == user_id || caller.is_system_admin {
            Ok(())
        } else {
            Err(denied("can only manage your own keys"))
        }
    }

    pub async fn create_key(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: CreateKeyInput,
    ) -> DomainResult<UserKeyPublic> {
        let user_id = input.user_id.unwrap_or_else(|| caller.user_id.clone());
        Self::require_self_or_admin(caller, &user_id)?;
        self.store
            .get_user(department, &user_id)
            .await?
            .ok_or_else(|| DomainError::not_found("user", &user_id))?;
        let (keys, _) = self
            .store
            .list_keys(
                department,
                &user_id,
                crate::common::Page {
                    limit: 1000,
                    offset: 0,
                },
            )
            .await?;
        if keys
            .iter()
            .filter(|k| k.status == KeyStatus::Active)
            .count()
            >= self.settings.max_active_keys_per_user
        {
            return Err(limit_exceeded("key_limit_exceeded", "too many active keys"));
        }
        let value = self.generate_key_value();
        let key = self.new_key(&user_id, value.clone(), false, input.name, input.expires_at);
        self.store.insert_key(department, &key).await?;
        let mut public = UserKeyPublic::masked(key);
        public.key_value = Some(value);
        Ok(public)
    }

    pub async fn list_keys(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        user_id: Option<String>,
        pagination: Pagination,
    ) -> DomainResult<MetaPage<UserKeyPublic>> {
        let user_id = user_id.unwrap_or_else(|| caller.user_id.clone());
        Self::require_self_or_admin(caller, &user_id)?;
        let page = pagination.page();
        let (keys, total) = self.store.list_keys(department, &user_id, page).await?;
        Ok(page_of(
            keys.into_iter().map(UserKeyPublic::masked).collect(),
            total,
            page,
        ))
    }

    async fn owned_key(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        key_id: &str,
    ) -> DomainResult<UserKey> {
        let key = self
            .store
            .get_key(department, key_id)
            .await?
            .ok_or_else(|| DomainError::not_found("user_key", key_id))?;
        Self::require_self_or_admin(caller, &key.user_id)?;
        Ok(key)
    }

    pub async fn get_key(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        key_id: &str,
    ) -> DomainResult<UserKeyPublic> {
        Ok(UserKeyPublic::masked(
            self.owned_key(department, caller, key_id).await?,
        ))
    }

    pub async fn revoke_key(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        key_id: &str,
    ) -> DomainResult<UserKeyPublic> {
        let key = self.owned_key(department, caller, key_id).await?;
        let (keys, _) = self
            .store
            .list_keys(
                department,
                &key.user_id,
                crate::common::Page {
                    limit: 1000,
                    offset: 0,
                },
            )
            .await?;
        if keys
            .iter()
            .filter(|k| k.status == KeyStatus::Active)
            .count()
            <= 1
        {
            return Err(DomainError::conflict(
                "last_key_cannot_revoke",
                "a user must keep one active key",
            ));
        }
        self.store.delete_key(department, key_id).await?;
        let mut public = UserKeyPublic::masked(key);
        public.status = KeyStatus::Revoked;
        Ok(public)
    }

    pub async fn update_key(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        input: UpdateKeyInput,
    ) -> DomainResult<UserKeyPublic> {
        let mut key = self.owned_key(department, caller, &input.key_id).await?;
        if input.name.is_some() {
            key.name = input.name;
        }
        if input.expires_at.is_some() {
            key.expires_at = input.expires_at;
        }
        self.store.update_key(department, &key).await?;
        Ok(UserKeyPublic::masked(key))
    }

    pub async fn get_config(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        user_id: &str,
        module: &str,
        param: Option<&str>,
    ) -> DomainResult<Vec<ConfigParam>> {
        if caller.user_id != user_id {
            return Err(denied("can only read your own configuration"));
        }
        let params = self
            .store
            .get_params(department, Some(user_id), module)
            .await?;
        Ok(params
            .into_iter()
            .filter(|p| param.is_none_or(|n| n == p.param_name))
            .collect())
    }

    pub async fn set_config(
        &self,
        department: &DepartmentId,
        caller: &Caller,
        user_id: &str,
        module: &str,
        params: Vec<(String, String)>,
    ) -> DomainResult<Vec<ConfigParam>> {
        if caller.user_id != user_id {
            return Err(denied("can only change your own configuration"));
        }
        self.store
            .set_user_params(department, user_id, module, &params)
            .await?;
        self.store
            .get_params(department, Some(user_id), module)
            .await
    }

    pub async fn department_quota(
        &self,
        department: &DepartmentId,
    ) -> DomainResult<serde_json::Value> {
        Ok(json!({
            "max_users_per_department": self.settings.max_users_per_department,
            "max_teams_per_department": self.settings.max_teams_per_department,
            "max_active_keys_per_user": self.settings.max_active_keys_per_user,
            "users": self.store.count_users(department).await?,
            "teams": self.store.count_teams(department).await?,
        }))
    }
}
