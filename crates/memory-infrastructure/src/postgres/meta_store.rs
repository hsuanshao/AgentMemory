//! Postgres adapter for the metadata plane. Like the SQLite adapter, each
//! entity is a JSONB document plus the columns its queries filter on.
//! `INSERT OR REPLACE` becomes `ON CONFLICT … DO UPDATE`, `INSERT OR
//! IGNORE` becomes `ON CONFLICT DO NOTHING`, and SQLite's ASCII
//! case-insensitive `LIKE` becomes `ILIKE`.

use std::sync::Arc;

use async_trait::async_trait;
use memory_application::common::{BatchDeleteResult, FailedId, Page};
use memory_application::ports::meta::{
    AclRepository, AgentFilter, AgentRepository, AssetFilter, AssetRepository, BindingRepository,
    ConfigParam, ConfigRepository, KnowledgeEntity, KnowledgeRepository, MemberRepository,
    ParticipationFilter, TaskFilter, TaskRepository, TeamRepository, UserFilter, UserKeyRepository,
    UserRepository,
};
use memory_domain::error::DomainResult;
use memory_domain::meta::{
    chat_memory_asset_id, AclEntry, Agent, AgentBinding, Asset, ParticipationLog, Task, TaskAgent,
    Team, TeamMember, User, UserKey,
};
use memory_domain::scope::DepartmentId;
use serde::de::DeserializeOwned;
use sqlx::postgres::PgConnection;

use super::db::{pg_err, PgDb, Schema};
use super::sql::{args, count, docs, execute, fetch_all, fetch_optional, get, to_doc, Arg, Where};

pub struct PostgresMetaStore {
    db: Arc<PgDb>,
}

impl PostgresMetaStore {
    pub fn new(db: Arc<PgDb>) -> Self {
        Self { db }
    }
}

/// The `doc` of the first matching row.
async fn one<T: DeserializeOwned>(
    c: &mut PgConnection,
    sql: &str,
    params: &[Arg],
) -> DomainResult<Option<T>> {
    fetch_optional(c, sql, params)
        .await?
        .map(|r| super::sql::doc(&r, "doc"))
        .transpose()
}

async fn many<T: DeserializeOwned>(
    c: &mut PgConnection,
    sql: &str,
    params: &[Arg],
) -> DomainResult<Vec<T>> {
    docs(&fetch_all(c, sql, params).await?)
}

/// `SELECT doc … ORDER BY … LIMIT/OFFSET` plus the matching total.
async fn paged<T: DeserializeOwned>(
    c: &mut PgConnection,
    table: &str,
    w: &Where,
    order: &str,
    page: Page,
) -> DomainResult<(Vec<T>, usize)> {
    let total = count(
        c,
        &format!("SELECT COUNT(*) FROM {table} {}", w.sql()),
        &w.params,
    )
    .await?;
    let items = many(
        c,
        &format!(
            "SELECT doc FROM {table} {} ORDER BY {order} LIMIT ? OFFSET ?",
            w.sql()
        ),
        &w.with(args![page.limit, page.offset]),
    )
    .await?;
    Ok((items, total))
}

/// Records whether deleting `id` removed a row.
fn record(result: &mut BatchDeleteResult, id: &str, removed: u64) {
    if removed > 0 {
        result.deleted_ids.push(id.to_owned());
    } else {
        result.failed.push(FailedId {
            id: id.to_owned(),
            reason: "not_found".into(),
        });
    }
}

async fn delete_asset_rows(c: &mut PgConnection, s: &Schema, asset_id: &str) -> DomainResult<u64> {
    execute(
        c,
        &format!("DELETE FROM {s}.meta_agent_bindings WHERE asset_id = ?"),
        &args![asset_id],
    )
    .await?;
    execute(
        c,
        &format!("DELETE FROM {s}.meta_asset_acl WHERE asset_id = ?"),
        &args![asset_id],
    )
    .await?;
    execute(
        c,
        &format!("DELETE FROM {s}.meta_assets WHERE asset_id = ?"),
        &args![asset_id],
    )
    .await
}

async fn insert_member(c: &mut PgConnection, s: &Schema, m: &TeamMember) -> DomainResult<u64> {
    execute(
        c,
        &format!(
            "INSERT INTO {s}.meta_team_members (team_id, user_id, status, joined_at, doc) VALUES (?,?,?,?,?) \
             ON CONFLICT (team_id, user_id) DO UPDATE SET status = EXCLUDED.status, joined_at = EXCLUDED.joined_at, doc = EXCLUDED.doc"
        ),
        &[
            Arg::from(&m.team_id),
            Arg::from(&m.user_id),
            Arg::from(m.status.as_str()),
            Arg::from(&m.joined_at),
            to_doc(m)?,
        ],
    )
    .await
}

async fn insert_key(c: &mut PgConnection, s: &Schema, k: &UserKey) -> DomainResult<u64> {
    execute(
        c,
        &format!("INSERT INTO {s}.meta_user_keys (key_id, user_id, key_value, created_at, doc) VALUES (?,?,?,?,?)"),
        &[
            Arg::from(&k.key_id),
            Arg::from(&k.user_id),
            Arg::from(&k.key_value),
            Arg::from(&k.created_at),
            to_doc(k)?,
        ],
    )
    .await
}

async fn insert_link(c: &mut PgConnection, s: &Schema, l: &TaskAgent) -> DomainResult<u64> {
    execute(
        c,
        &format!(
            "INSERT INTO {s}.meta_task_agents (task_id, agent_id, created_at, doc) VALUES (?,?,?,?) \
             ON CONFLICT (task_id, agent_id) DO UPDATE SET created_at = EXCLUDED.created_at, doc = EXCLUDED.doc"
        ),
        &[
            Arg::from(&l.task_id),
            Arg::from(&l.agent_id),
            Arg::from(&l.created_at),
            to_doc(l)?,
        ],
    )
    .await
}

async fn insert_binding(
    c: &mut PgConnection,
    s: &Schema,
    b: &AgentBinding,
    replace: bool,
) -> DomainResult<u64> {
    let on_conflict = if replace {
        "DO UPDATE SET priority = EXCLUDED.priority, doc = EXCLUDED.doc"
    } else {
        "DO NOTHING"
    };
    execute(
        c,
        &format!(
            "INSERT INTO {s}.meta_agent_bindings (agent_id, asset_id, priority, doc) VALUES (?,?,?,?) \
             ON CONFLICT (agent_id, asset_id) {on_conflict}"
        ),
        &[
            Arg::from(&b.agent_id),
            Arg::from(&b.asset_id),
            Arg::from(b.priority),
            to_doc(b)?,
        ],
    )
    .await
}

#[async_trait]
impl UserRepository for PostgresMetaStore {
    async fn create_user(
        &self,
        department: &DepartmentId,
        user: &User,
        default_key: &UserKey,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        execute(
            &mut tx,
            &format!("INSERT INTO {s}.meta_users (user_id, username, user_type, status, created_at, doc) VALUES (?,?,?,?,?,?)"),
            &[
                Arg::from(&user.user_id),
                Arg::from(&user.username),
                Arg::from(user.user_type.as_str()),
                Arg::from(user.status.as_str()),
                Arg::from(&user.created_at),
                to_doc(user)?,
            ],
        )
        .await?;
        insert_key(&mut tx, &s, default_key).await?;
        tx.commit().await.map_err(pg_err)
    }

    async fn get_user(
        &self,
        department: &DepartmentId,
        user_id: &str,
    ) -> DomainResult<Option<User>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_users WHERE user_id = ?"),
            &args![user_id],
        )
        .await
    }

    async fn find_user_by_name(
        &self,
        department: &DepartmentId,
        username: &str,
    ) -> DomainResult<Option<User>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_users WHERE username = ?"),
            &args![username],
        )
        .await
    }

    async fn list_users(
        &self,
        department: &DepartmentId,
        filter: &UserFilter,
        page: Page,
    ) -> DomainResult<(Vec<User>, usize)> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        if let Some(team) = &filter.team_id {
            w.raw(
                &format!("user_id IN (SELECT user_id FROM {s}.meta_team_members WHERE team_id = ? AND status = 'active')"),
                [Arg::from(team)],
            );
        }
        if let Some(ids) = &filter.user_ids {
            w.in_list("user_id", ids);
        }
        if let Some(name) = &filter.username {
            w.raw("username ILIKE ?", [Arg::from(format!("%{name}%"))]);
        }
        w.eq_opt("user_type", filter.user_type.map(|t| t.as_str().to_owned()));
        w.eq_opt("status", filter.status.map(|st| st.as_str().to_owned()));
        paged(
            &mut *self.db.conn().await?,
            &format!("{s}.meta_users"),
            &w,
            "created_at, user_id",
            page,
        )
        .await
    }

    async fn count_users(&self, department: &DepartmentId) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        count(
            &mut *self.db.conn().await?,
            &format!("SELECT COUNT(*) FROM {s}.meta_users"),
            &[],
        )
        .await
    }

    async fn delete_users(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        let mut result = BatchDeleteResult::default();
        for id in ids {
            let p = args![id];
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_user_keys WHERE user_id = ?"),
                &p,
            )
            .await?;
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_team_members WHERE user_id = ?"),
                &p,
            )
            .await?;
            execute(
                &mut tx,
                &format!(
                    "DELETE FROM {s}.meta_asset_acl WHERE subject_type = 'user' AND subject_id = ?"
                ),
                &p,
            )
            .await?;
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_config_params WHERE scope = 'user' AND user_id = ?"),
                &p,
            )
            .await?;
            let n = execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_users WHERE user_id = ?"),
                &p,
            )
            .await?;
            record(&mut result, id, n);
        }
        tx.commit().await.map_err(pg_err)?;
        Ok(result)
    }
}

#[async_trait]
impl UserKeyRepository for PostgresMetaStore {
    async fn insert_key(&self, department: &DepartmentId, key: &UserKey) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        insert_key(&mut *self.db.conn().await?, &s, key)
            .await
            .map(|_| ())
    }

    async fn find_key_by_value(
        &self,
        department: &DepartmentId,
        value: &str,
    ) -> DomainResult<Option<UserKey>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_user_keys WHERE key_value = ?"),
            &args![value],
        )
        .await
    }

    async fn get_key(
        &self,
        department: &DepartmentId,
        key_id: &str,
    ) -> DomainResult<Option<UserKey>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_user_keys WHERE key_id = ?"),
            &args![key_id],
        )
        .await
    }

    async fn list_keys(
        &self,
        department: &DepartmentId,
        user_id: &str,
        page: Page,
    ) -> DomainResult<(Vec<UserKey>, usize)> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.eq("user_id", user_id);
        paged(
            &mut *self.db.conn().await?,
            &format!("{s}.meta_user_keys"),
            &w,
            "created_at, key_id",
            page,
        )
        .await
    }

    async fn update_key(&self, department: &DepartmentId, key: &UserKey) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("UPDATE {s}.meta_user_keys SET doc = ? WHERE key_id = ?"),
            &[to_doc(key)?, Arg::from(&key.key_id)],
        )
        .await
        .map(|_| ())
    }

    async fn delete_key(&self, department: &DepartmentId, key_id: &str) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        let removed: Option<UserKey> = one(
            &mut tx,
            &format!("SELECT doc FROM {s}.meta_user_keys WHERE key_id = ? FOR UPDATE"),
            &args![key_id],
        )
        .await?;
        execute(
            &mut tx,
            &format!("DELETE FROM {s}.meta_user_keys WHERE key_id = ?"),
            &args![key_id],
        )
        .await?;
        if let Some(key) = removed.filter(|k| k.is_default) {
            let next: Option<UserKey> = one(
                &mut tx,
                &format!("SELECT doc FROM {s}.meta_user_keys WHERE user_id = ? ORDER BY created_at, key_id LIMIT 1"),
                &args![key.user_id.as_str()],
            )
            .await?;
            if let Some(mut promoted) = next {
                promoted.is_default = true;
                execute(
                    &mut tx,
                    &format!("UPDATE {s}.meta_user_keys SET doc = ? WHERE key_id = ?"),
                    &[to_doc(&promoted)?, Arg::from(&promoted.key_id)],
                )
                .await?;
            }
        }
        tx.commit().await.map_err(pg_err)
    }

    async fn touch_key(
        &self,
        department: &DepartmentId,
        key_id: &str,
        at: &str,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("UPDATE {s}.meta_user_keys SET doc = jsonb_set(doc, '{{last_used_at}}', to_jsonb(?::text)) WHERE key_id = ?"),
            &args![at, key_id],
        )
        .await
        .map(|_| ())
    }
}

#[async_trait]
impl TeamRepository for PostgresMetaStore {
    async fn create_team(
        &self,
        department: &DepartmentId,
        team: &Team,
        owner: &TeamMember,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        execute(
            &mut tx,
            &format!(
                "INSERT INTO {s}.meta_teams (team_id, name, created_at, doc) VALUES (?,?,?,?)"
            ),
            &[
                Arg::from(&team.team_id),
                Arg::from(&team.name),
                Arg::from(&team.created_at),
                to_doc(team)?,
            ],
        )
        .await?;
        insert_member(&mut tx, &s, owner).await?;
        tx.commit().await.map_err(pg_err)
    }

    async fn get_team(
        &self,
        department: &DepartmentId,
        team_id: &str,
    ) -> DomainResult<Option<Team>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_teams WHERE team_id = ?"),
            &args![team_id],
        )
        .await
    }

    async fn update_team(&self, department: &DepartmentId, team: &Team) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("UPDATE {s}.meta_teams SET name = ?, doc = ? WHERE team_id = ?"),
            &[
                Arg::from(&team.name),
                to_doc(team)?,
                Arg::from(&team.team_id),
            ],
        )
        .await
        .map(|_| ())
    }

    async fn list_teams_of_user(
        &self,
        department: &DepartmentId,
        user_id: Option<&str>,
        name: Option<&str>,
        page: Page,
    ) -> DomainResult<(Vec<Team>, usize)> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        if let Some(user_id) = user_id {
            w.raw(
                &format!("team_id IN (SELECT team_id FROM {s}.meta_team_members WHERE user_id = ? AND status = 'active')"),
                [Arg::from(user_id)],
            );
        }
        if let Some(n) = name {
            w.raw("name ILIKE ?", [Arg::from(format!("%{n}%"))]);
        }
        paged(
            &mut *self.db.conn().await?,
            &format!("{s}.meta_teams"),
            &w,
            "created_at, team_id",
            page,
        )
        .await
    }

    async fn count_teams(&self, department: &DepartmentId) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        count(
            &mut *self.db.conn().await?,
            &format!("SELECT COUNT(*) FROM {s}.meta_teams"),
            &[],
        )
        .await
    }

    async fn delete_teams(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        let mut result = BatchDeleteResult::default();
        for id in ids {
            let p = args![id];
            let assets = fetch_all(
                &mut tx,
                &format!("SELECT asset_id FROM {s}.meta_assets WHERE team_id = ?"),
                &p,
            )
            .await?;
            for row in &assets {
                let asset: String = get(row, "asset_id")?;
                delete_asset_rows(&mut tx, &s, &asset).await?;
            }
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_agent_bindings WHERE agent_id IN (SELECT agent_id FROM {s}.meta_agents WHERE team_id = ?)"),
                &p,
            )
            .await?;
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_task_agents WHERE task_id IN (SELECT task_id FROM {s}.meta_tasks WHERE team_id = ?)"),
                &p,
            )
            .await?;
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_tasks WHERE team_id = ?"),
                &p,
            )
            .await?;
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_agents WHERE team_id = ?"),
                &p,
            )
            .await?;
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_team_members WHERE team_id = ?"),
                &p,
            )
            .await?;
            let n = execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_teams WHERE team_id = ?"),
                &p,
            )
            .await?;
            record(&mut result, id, n);
        }
        tx.commit().await.map_err(pg_err)?;
        Ok(result)
    }
}

#[async_trait]
impl MemberRepository for PostgresMetaStore {
    async fn upsert_member(
        &self,
        department: &DepartmentId,
        member: &TeamMember,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        insert_member(&mut *self.db.conn().await?, &s, member)
            .await
            .map(|_| ())
    }

    async fn get_member(
        &self,
        department: &DepartmentId,
        team_id: &str,
        user_id: &str,
    ) -> DomainResult<Option<TeamMember>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_team_members WHERE team_id = ? AND user_id = ?"),
            &args![team_id, user_id],
        )
        .await
    }

    async fn list_members(
        &self,
        department: &DepartmentId,
        team_id: &str,
        page: Page,
    ) -> DomainResult<(Vec<TeamMember>, usize)> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.eq("team_id", team_id).eq("status", "active");
        paged(
            &mut *self.db.conn().await?,
            &format!("{s}.meta_team_members"),
            &w,
            "joined_at, user_id",
            page,
        )
        .await
    }

    async fn list_memberships(
        &self,
        department: &DepartmentId,
        user_id: &str,
    ) -> DomainResult<Vec<TeamMember>> {
        let s = self.db.schema(department).await?;
        // SQLite leaves this order unspecified; make it deterministic.
        many(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_team_members WHERE user_id = ? ORDER BY team_id"),
            &args![user_id],
        )
        .await
    }

    async fn remove_member(
        &self,
        department: &DepartmentId,
        team_id: &str,
        user_id: &str,
    ) -> DomainResult<bool> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("DELETE FROM {s}.meta_team_members WHERE team_id = ? AND user_id = ?"),
            &args![team_id, user_id],
        )
        .await
        .map(|n| n > 0)
    }
}

#[async_trait]
impl AgentRepository for PostgresMetaStore {
    async fn insert_agent(&self, department: &DepartmentId, a: &Agent) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("INSERT INTO {s}.meta_agents (agent_id, team_id, owner_user_id, status, name, created_at, doc) VALUES (?,?,?,?,?,?,?)"),
            &[
                Arg::from(&a.agent_id),
                Arg::from(&a.team_id),
                Arg::from(&a.owner_user_id),
                Arg::from(a.status.as_str()),
                Arg::from(&a.name),
                Arg::from(&a.created_at),
                to_doc(a)?,
            ],
        )
        .await
        .map(|_| ())
    }

    async fn get_agent(
        &self,
        department: &DepartmentId,
        agent_id: &str,
    ) -> DomainResult<Option<Agent>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_agents WHERE agent_id = ?"),
            &args![agent_id],
        )
        .await
    }

    async fn update_agent(&self, department: &DepartmentId, a: &Agent) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("UPDATE {s}.meta_agents SET status = ?, name = ?, doc = ? WHERE agent_id = ?"),
            &[
                Arg::from(a.status.as_str()),
                Arg::from(&a.name),
                to_doc(a)?,
                Arg::from(&a.agent_id),
            ],
        )
        .await
        .map(|_| ())
    }

    async fn list_agents(
        &self,
        department: &DepartmentId,
        filter: &AgentFilter,
        page: Page,
    ) -> DomainResult<(Vec<Agent>, usize)> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.eq_opt("team_id", filter.team_id.clone())
            .eq_opt("owner_user_id", filter.owner_user_id.clone())
            .eq_opt("status", filter.status.map(|st| st.as_str().to_owned()));
        if let Some(n) = &filter.name {
            w.raw("name ILIKE ?", [Arg::from(format!("%{n}%"))]);
        }
        paged(
            &mut *self.db.conn().await?,
            &format!("{s}.meta_agents"),
            &w,
            "created_at, agent_id",
            page,
        )
        .await
    }

    async fn delete_agents(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        let mut result = BatchDeleteResult::default();
        for id in ids {
            let p = args![id];
            let team = fetch_optional(
                &mut tx,
                &format!("SELECT team_id FROM {s}.meta_agents WHERE agent_id = ?"),
                &p,
            )
            .await?;
            if let Some(row) = team {
                let team: String = get(&row, "team_id")?;
                delete_asset_rows(&mut tx, &s, &chat_memory_asset_id(&team, id)).await?;
            }
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_task_agents WHERE agent_id = ?"),
                &p,
            )
            .await?;
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_agent_bindings WHERE agent_id = ?"),
                &p,
            )
            .await?;
            let n = execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_agents WHERE agent_id = ?"),
                &p,
            )
            .await?;
            record(&mut result, id, n);
        }
        tx.commit().await.map_err(pg_err)?;
        Ok(result)
    }
}

#[async_trait]
impl TaskRepository for PostgresMetaStore {
    async fn insert_task(
        &self,
        department: &DepartmentId,
        t: &Task,
        links: &[TaskAgent],
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        execute(
            &mut tx,
            &format!("INSERT INTO {s}.meta_tasks (task_id, team_id, creator_user_id, status, created_at, doc) VALUES (?,?,?,?,?,?)"),
            &[
                Arg::from(&t.task_id),
                Arg::from(&t.team_id),
                Arg::from(&t.creator_user_id),
                Arg::from(t.status.as_str()),
                Arg::from(&t.created_at),
                to_doc(t)?,
            ],
        )
        .await?;
        for l in links {
            insert_link(&mut tx, &s, l).await?;
        }
        tx.commit().await.map_err(pg_err)
    }

    async fn get_task(
        &self,
        department: &DepartmentId,
        task_id: &str,
    ) -> DomainResult<Option<Task>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_tasks WHERE task_id = ?"),
            &args![task_id],
        )
        .await
    }

    async fn update_task(&self, department: &DepartmentId, t: &Task) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("UPDATE {s}.meta_tasks SET status = ?, doc = ? WHERE task_id = ?"),
            &[
                Arg::from(t.status.as_str()),
                to_doc(t)?,
                Arg::from(&t.task_id),
            ],
        )
        .await
        .map(|_| ())
    }

    async fn list_tasks(
        &self,
        department: &DepartmentId,
        filter: &TaskFilter,
        page: Page,
    ) -> DomainResult<(Vec<Task>, usize)> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.eq_opt("team_id", filter.team_id.clone())
            .eq_opt("creator_user_id", filter.creator_user_id.clone())
            .eq_opt("status", filter.status.map(|st| st.as_str().to_owned()));
        paged(
            &mut *self.db.conn().await?,
            &format!("{s}.meta_tasks"),
            &w,
            "created_at DESC, task_id",
            page,
        )
        .await
    }

    async fn delete_tasks(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        let mut result = BatchDeleteResult::default();
        for id in ids {
            let p = args![id];
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_task_agents WHERE task_id = ?"),
                &p,
            )
            .await?;
            let n = execute(
                &mut tx,
                &format!("DELETE FROM {s}.meta_tasks WHERE task_id = ?"),
                &p,
            )
            .await?;
            record(&mut result, id, n);
        }
        tx.commit().await.map_err(pg_err)?;
        Ok(result)
    }

    async fn link_agent(&self, department: &DepartmentId, link: &TaskAgent) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        insert_link(&mut *self.db.conn().await?, &s, link)
            .await
            .map(|_| ())
    }

    async fn unlink_agent(
        &self,
        department: &DepartmentId,
        task_id: &str,
        agent_id: &str,
    ) -> DomainResult<bool> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("DELETE FROM {s}.meta_task_agents WHERE task_id = ? AND agent_id = ?"),
            &args![task_id, agent_id],
        )
        .await
        .map(|n| n > 0)
    }

    async fn list_task_agents(
        &self,
        department: &DepartmentId,
        task_id: &str,
        page: Page,
    ) -> DomainResult<(Vec<TaskAgent>, usize)> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.eq("task_id", task_id);
        paged(
            &mut *self.db.conn().await?,
            &format!("{s}.meta_task_agents"),
            &w,
            "created_at, agent_id",
            page,
        )
        .await
    }

    async fn append_participation(
        &self,
        department: &DepartmentId,
        l: &ParticipationLog,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("INSERT INTO {s}.meta_participation_logs (id, team_id, task_id, agent_id, user_id, created_at, doc) VALUES (?,?,?,?,?,?,?)"),
            &[
                Arg::from(&l.id),
                Arg::from(&l.team_id),
                Arg::from(&l.task_id),
                Arg::from(&l.agent_id),
                Arg::from(&l.user_id),
                Arg::from(&l.created_at),
                to_doc(l)?,
            ],
        )
        .await
        .map(|_| ())
    }

    async fn list_participation(
        &self,
        department: &DepartmentId,
        filter: &ParticipationFilter,
        page: Page,
    ) -> DomainResult<(Vec<ParticipationLog>, usize)> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.eq("team_id", filter.team_id.as_str())
            .eq_opt("task_id", filter.task_id.clone())
            .eq_opt("agent_id", filter.agent_id.clone())
            .eq_opt("user_id", filter.user_id.clone());
        if let Some(a) = &filter.created_after {
            w.raw("created_at >= ?", [Arg::from(a)]);
        }
        if let Some(b) = &filter.created_before {
            w.raw("created_at <= ?", [Arg::from(b)]);
        }
        paged(
            &mut *self.db.conn().await?,
            &format!("{s}.meta_participation_logs"),
            &w,
            "created_at DESC, id",
            page,
        )
        .await
    }
}

#[async_trait]
impl AssetRepository for PostgresMetaStore {
    async fn insert_asset(&self, department: &DepartmentId, a: &Asset) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("INSERT INTO {s}.meta_assets (asset_id, team_id, asset_type, status, owner_user_id, visibility, created_at, doc) VALUES (?,?,?,?,?,?,?,?)"),
            &[
                Arg::from(&a.asset_id),
                Arg::from(&a.team_id),
                Arg::from(a.asset_type.as_str()),
                Arg::from(a.status.as_str()),
                Arg::from(&a.owner_user_id),
                Arg::from(a.visibility.as_str()),
                Arg::from(&a.created_at),
                to_doc(a)?,
            ],
        )
        .await
        .map(|_| ())
    }

    async fn get_asset(
        &self,
        department: &DepartmentId,
        asset_id: &str,
    ) -> DomainResult<Option<Asset>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_assets WHERE asset_id = ?"),
            &args![asset_id],
        )
        .await
    }

    async fn update_asset(&self, department: &DepartmentId, a: &Asset) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!(
                "UPDATE {s}.meta_assets SET status = ?, visibility = ?, doc = ? WHERE asset_id = ?"
            ),
            &[
                Arg::from(a.status.as_str()),
                Arg::from(a.visibility.as_str()),
                to_doc(a)?,
                Arg::from(&a.asset_id),
            ],
        )
        .await
        .map(|_| ())
    }

    async fn list_assets(
        &self,
        department: &DepartmentId,
        filter: &AssetFilter,
    ) -> DomainResult<Vec<Asset>> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.eq_opt("team_id", filter.team_id.clone())
            .eq_opt(
                "asset_type",
                filter.asset_type.map(|t| t.as_str().to_owned()),
            )
            .eq_opt("status", filter.status.map(|st| st.as_str().to_owned()))
            .eq_opt("owner_user_id", filter.owner_user_id.clone());
        if let Some(vs) = &filter.visibility {
            let values: Vec<String> = vs.iter().map(|v| v.as_str().to_owned()).collect();
            w.in_list("visibility", &values);
        }
        many(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT doc FROM {s}.meta_assets {} ORDER BY created_at DESC, asset_id",
                w.sql()
            ),
            &w.params,
        )
        .await
    }

    async fn delete_assets(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        for id in ids {
            delete_asset_rows(&mut tx, &s, id).await?;
        }
        tx.commit().await.map_err(pg_err)?;
        Ok(BatchDeleteResult {
            deleted_ids: ids.to_vec(),
            failed: Vec::new(),
        })
    }

    async fn touch_asset(
        &self,
        department: &DepartmentId,
        asset_id: &str,
        at: &str,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        // A missing counter stays JSON null, as `json_extract(…) + 1` does.
        execute(
            &mut *self.db.conn().await?,
            &format!(
                "UPDATE {s}.meta_assets SET doc = jsonb_set(jsonb_set(doc, '{{last_used_at}}', to_jsonb(?::text)), \
                 '{{usage_count}}', COALESCE(to_jsonb((doc->>'usage_count')::bigint + 1), 'null'::jsonb)) WHERE asset_id = ?"
            ),
            &args![at, asset_id],
        )
        .await
        .map(|_| ())
    }
}

#[async_trait]
impl BindingRepository for PostgresMetaStore {
    async fn replace_bindings(
        &self,
        department: &DepartmentId,
        agent_id: &str,
        bindings: &[AgentBinding],
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        execute(
            &mut tx,
            &format!("DELETE FROM {s}.meta_agent_bindings WHERE agent_id = ?"),
            &args![agent_id],
        )
        .await?;
        for b in bindings {
            insert_binding(&mut tx, &s, b, true).await?;
        }
        tx.commit().await.map_err(pg_err)
    }

    async fn ensure_binding(
        &self,
        department: &DepartmentId,
        binding: &AgentBinding,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        insert_binding(&mut *self.db.conn().await?, &s, binding, false)
            .await
            .map(|_| ())
    }

    async fn list_bindings(
        &self,
        department: &DepartmentId,
        agent_id: &str,
    ) -> DomainResult<Vec<AgentBinding>> {
        let s = self.db.schema(department).await?;
        many(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_agent_bindings WHERE agent_id = ? ORDER BY priority DESC, asset_id"),
            &args![agent_id],
        )
        .await
    }
}

#[async_trait]
impl AclRepository for PostgresMetaStore {
    async fn grant(&self, department: &DepartmentId, e: &AclEntry) -> DomainResult<AclEntry> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        let existing: Option<AclEntry> = one(
            &mut tx,
            &format!("SELECT doc FROM {s}.meta_asset_acl WHERE asset_id = ? AND subject_type = ? AND subject_id = ? AND permission = ? FOR UPDATE"),
            &args![
                e.asset_id.as_str(),
                e.subject_type.as_str(),
                e.subject_id.as_str(),
                e.permission.as_str()
            ],
        )
        .await?;
        let stored = match existing {
            Some(mut old) => {
                old.effect = e.effect;
                old.granted_by = e.granted_by.clone();
                old.updated_at = e.updated_at.clone();
                old
            }
            None => e.clone(),
        };
        execute(
            &mut tx,
            &format!(
                "INSERT INTO {s}.meta_asset_acl (id, asset_id, subject_type, subject_id, permission, doc) VALUES (?,?,?,?,?,?) \
                 ON CONFLICT (id) DO UPDATE SET asset_id = EXCLUDED.asset_id, subject_type = EXCLUDED.subject_type, \
                 subject_id = EXCLUDED.subject_id, permission = EXCLUDED.permission, doc = EXCLUDED.doc"
            ),
            &[
                Arg::from(&stored.id),
                Arg::from(&stored.asset_id),
                Arg::from(stored.subject_type.as_str()),
                Arg::from(&stored.subject_id),
                Arg::from(stored.permission.as_str()),
                to_doc(&stored)?,
            ],
        )
        .await?;
        tx.commit().await.map_err(pg_err)?;
        Ok(stored)
    }

    async fn revoke(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<AclEntry>> {
        let s = self.db.schema(department).await?;
        let row = fetch_optional(
            &mut *self.db.conn().await?,
            &format!("DELETE FROM {s}.meta_asset_acl WHERE id = ? RETURNING doc"),
            &args![id],
        )
        .await?;
        row.map(|r| super::sql::doc(&r, "doc")).transpose()
    }

    async fn get_acl(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<AclEntry>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_asset_acl WHERE id = ?"),
            &args![id],
        )
        .await
    }

    async fn list_acl(
        &self,
        department: &DepartmentId,
        asset_id: &str,
    ) -> DomainResult<Vec<AclEntry>> {
        let s = self.db.schema(department).await?;
        many(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.meta_asset_acl WHERE asset_id = ? ORDER BY id"),
            &args![asset_id],
        )
        .await
    }
}

#[async_trait]
impl ConfigRepository for PostgresMetaStore {
    async fn get_params(
        &self,
        department: &DepartmentId,
        user_id: Option<&str>,
        module: &str,
    ) -> DomainResult<Vec<ConfigParam>> {
        let s = self.db.schema(department).await?;
        // User values shadow globals: the `user` scope sorts after `global`.
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT scope, module, param_name, param_value FROM {s}.meta_config_params \
                 WHERE module = ? AND (scope = 'global' OR (scope = 'user' AND user_id = ?)) ORDER BY param_name, scope"
            ),
            &args![module, user_id.unwrap_or("")],
        )
        .await?;
        let mut effective: Vec<ConfigParam> = Vec::new();
        for r in &rows {
            let p = ConfigParam {
                scope: get(r, "scope")?,
                module: get(r, "module")?,
                param_name: get(r, "param_name")?,
                param_value: get(r, "param_value")?,
            };
            match effective.last_mut() {
                Some(last) if last.param_name == p.param_name => *last = p,
                _ => effective.push(p),
            }
        }
        Ok(effective)
    }

    async fn set_user_params(
        &self,
        department: &DepartmentId,
        user_id: &str,
        module: &str,
        params_in: &[(String, String)],
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        for (name, value) in params_in {
            execute(
                &mut tx,
                &format!(
                    "INSERT INTO {s}.meta_config_params VALUES ('user', ?, ?, ?, ?) \
                     ON CONFLICT (scope, user_id, module, param_name) DO UPDATE SET param_value = EXCLUDED.param_value"
                ),
                &args![user_id, module, name, value],
            )
            .await?;
        }
        tx.commit().await.map_err(pg_err)
    }
}

#[async_trait]
impl KnowledgeRepository for PostgresMetaStore {
    async fn upsert_knowledge(
        &self,
        department: &DepartmentId,
        e: &KnowledgeEntity,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!(
                "INSERT INTO {s}.knowledge (id, team_id, kind, created_at, doc) VALUES (?,?,?,?,?) \
                 ON CONFLICT (id) DO UPDATE SET team_id = EXCLUDED.team_id, kind = EXCLUDED.kind, \
                 created_at = EXCLUDED.created_at, doc = EXCLUDED.doc"
            ),
            &[
                Arg::from(&e.knowledge_id),
                Arg::from(&e.team_id),
                Arg::from(&e.kind),
                Arg::from(&e.created_at),
                to_doc(e)?,
            ],
        )
        .await
        .map(|_| ())
    }

    async fn get_knowledge(
        &self,
        department: &DepartmentId,
        id: &str,
    ) -> DomainResult<Option<KnowledgeEntity>> {
        let s = self.db.schema(department).await?;
        one(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.knowledge WHERE id = ?"),
            &args![id],
        )
        .await
    }

    async fn delete_knowledge(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        let mut result = BatchDeleteResult::default();
        for id in ids {
            let n = execute(
                &mut tx,
                &format!("DELETE FROM {s}.knowledge WHERE id = ?"),
                &args![id],
            )
            .await?;
            record(&mut result, id, n);
        }
        tx.commit().await.map_err(pg_err)?;
        Ok(result)
    }

    async fn list_knowledge(
        &self,
        department: &DepartmentId,
        team_id: &str,
        kind: Option<&str>,
        ids: Option<&[String]>,
        page: Page,
    ) -> DomainResult<(Vec<KnowledgeEntity>, usize)> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.eq("team_id", team_id).eq_opt("kind", kind);
        if let Some(ids) = ids {
            w.in_list("id", ids);
        }
        paged(
            &mut *self.db.conn().await?,
            &format!("{s}.knowledge"),
            &w,
            "created_at DESC, id",
            page,
        )
        .await
    }
}
