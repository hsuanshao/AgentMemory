//! Metadata plane: users, keys, teams, agents, tasks, assets and access.
//!
//! Authorization rules: system admins
//! manage users; team owners/admins manage teams; owners manage their own
//! agents, tasks and assets.

mod agents;
mod assets;
mod data_access;
mod reads;
mod teams;
mod users;
mod visibility;

use std::sync::{Arc, OnceLock};

use chrono::SecondsFormat;
use memory_domain::error::{DomainError, DomainResult, ErrorKind};
use memory_domain::meta::{TeamMember, User, UserType};
use memory_domain::scope::DepartmentId;
use serde::{Deserialize, Serialize};

pub use agents::*;
pub use assets::*;
pub use teams::*;
pub use users::*;
pub use visibility::DisplayNames;

use crate::ports::meta::MetaStore;
use crate::ports::system::{Clock, IdGenerator};

/// The authenticated caller of a metadata request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub user_id: String,
    pub is_system_admin: bool,
}

impl Caller {
    pub fn from_user(user: &User) -> Self {
        Self {
            user_id: user.user_id.clone(),
            is_system_admin: user.user_type == UserType::SystemAdmin,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MetaSettings {
    pub max_users_per_department: usize,
    pub max_teams_per_department: usize,
    pub max_active_keys_per_user: usize,
}

impl Default for MetaSettings {
    fn default() -> Self {
        Self {
            max_users_per_department: 500,
            max_teams_per_department: 100,
            max_active_keys_per_user: 20,
        }
    }
}

/// Meta list envelope `{ items, total, limit, offset }`.
#[derive(Debug, Clone, Serialize)]
pub struct MetaPage<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Pagination {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

impl Pagination {
    pub fn page(self) -> crate::common::Page {
        crate::common::Page::standard(self.limit, self.offset)
    }
}

pub(crate) fn page_of<T>(items: Vec<T>, total: usize, p: crate::common::Page) -> MetaPage<T> {
    MetaPage {
        items,
        total,
        limit: p.limit,
        offset: p.offset,
    }
}

pub struct MetaService {
    pub(crate) store: Arc<dyn MetaStore>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) ids: Arc<dyn IdGenerator>,
    pub(crate) settings: MetaSettings,
    cleaner: OnceLock<Arc<dyn ChatMemoryCleaner>>,
}

impl MetaService {
    pub fn new(
        store: Arc<dyn MetaStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        settings: MetaSettings,
    ) -> Self {
        Self {
            store,
            clock,
            ids,
            settings,
            cleaner: OnceLock::new(),
        }
    }

    /// Late-bound to break the meta ↔ chat-memory construction cycle.
    pub fn register_chat_memory_cleaner(&self, cleaner: Arc<dyn ChatMemoryCleaner>) {
        let _ = self.cleaner.set(cleaner);
    }

    pub(crate) fn chat_memory_cleaner(&self) -> Option<&Arc<dyn ChatMemoryCleaner>> {
        self.cleaner.get()
    }

    pub(crate) fn now(&self) -> String {
        self.clock
            .now()
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    /// `{prefix}-{12 random chars}`.
    pub(crate) fn entity_id(&self, prefix: &str) -> String {
        format!("{prefix}-{}", self.ids.hex(12))
    }

    pub(crate) async fn active_membership(
        &self,
        department: &DepartmentId,
        team_id: &str,
        user_id: &str,
    ) -> DomainResult<Option<TeamMember>> {
        Ok(self
            .store
            .get_member(department, team_id, user_id)
            .await?
            .filter(TeamMember::is_active))
    }

    pub(crate) async fn require_member(
        &self,
        department: &DepartmentId,
        team_id: &str,
        caller: &Caller,
    ) -> DomainResult<TeamMember> {
        self.active_membership(department, team_id, &caller.user_id)
            .await?
            .ok_or_else(|| denied("not an active member of the team"))
    }

    pub(crate) async fn require_team_admin(
        &self,
        department: &DepartmentId,
        team_id: &str,
        caller: &Caller,
    ) -> DomainResult<TeamMember> {
        let member = self.require_member(department, team_id, caller).await?;
        if member.is_admin() {
            Ok(member)
        } else {
            Err(denied("team admin required"))
        }
    }
}

pub(crate) fn denied(detail: &str) -> DomainError {
    DomainError::permission_denied(detail)
}

pub(crate) fn require_system_admin(caller: &Caller) -> DomainResult<()> {
    if caller.is_system_admin {
        Ok(())
    } else {
        Err(denied("system admin required"))
    }
}

pub(crate) fn limit_exceeded(code: &str, detail: &str) -> DomainError {
    DomainError::new(ErrorKind::Conflict, code, detail)
}

pub(crate) fn require_non_empty(field: &str, value: &str) -> DomainResult<()> {
    if value.trim().is_empty() {
        Err(DomainError::invalid(format!("{field} is required")))
    } else {
        Ok(())
    }
}
