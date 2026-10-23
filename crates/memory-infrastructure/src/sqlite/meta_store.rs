//! SQLite adapter for the metadata plane. Each entity is stored as a JSON
//! document plus the columns its queries filter on.

use std::sync::Arc;

use async_trait::async_trait;
use memory_application::common::{BatchDeleteResult, FailedId, Page};
use memory_application::ports::meta::{
    AclRepository, AgentFilter, AgentRepository, AssetFilter, AssetRepository, BindingRepository,
    ConfigParam, ConfigRepository, KnowledgeEntity, KnowledgeRepository, MemberRepository,
    ParticipationFilter, RedactionRule, RedactionRuleRepository, TaskFilter, TaskRepository,
    TeamRepository, UserFilter, UserKeyRepository, UserRepository,
};
use memory_domain::error::DomainResult;
use memory_domain::meta::{
    chat_memory_asset_id, AclEntry, Agent, AgentBinding, Asset, ParticipationLog, Task, TaskAgent,
    Team, TeamMember, User, UserKey,
};
use memory_domain::scope::DepartmentId;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;

use super::db::DepartmentPool;
use super::sql::{from_doc, to_doc, Where};

pub struct SqliteMetaStore {
    pool: Arc<DepartmentPool>,
}

impl SqliteMetaStore {
    pub fn new(pool: Arc<DepartmentPool>) -> Self {
        Self { pool }
    }

    async fn with<T, F>(&self, department: &DepartmentId, f: F) -> DomainResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        self.pool.get(department)?.call(f).await
    }
}

fn one<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Option<T>> {
    let doc: Option<String> = conn.query_row(sql, params, |r| r.get(0)).optional()?;
    doc.map(|d| from_doc(&d)).transpose()
}

fn many<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    params: &[Value],
) -> rusqlite::Result<Vec<T>> {
    let mut stmt = conn.prepare(sql)?;
    let docs = stmt
        .query_map(params_from_iter(params.iter()), |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    docs.iter().map(|d| from_doc(d)).collect()
}

/// `SELECT doc … ORDER BY … LIMIT/OFFSET` plus the matching total.
fn paged<T: DeserializeOwned>(
    conn: &Connection,
    table: &str,
    w: &Where,
    order: &str,
    page: Page,
) -> rusqlite::Result<(Vec<T>, usize)> {
    let total: usize = conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} {}", w.sql()),
        params_from_iter(w.params.iter()),
        |r| r.get(0),
    )?;
    let sql = format!(
        "SELECT doc FROM {table} {} ORDER BY {order} LIMIT ? OFFSET ?",
        w.sql()
    );
    let items = many(
        conn,
        &sql,
        &w.with([
            Value::from(page.limit as i64),
            Value::from(page.offset as i64),
        ]),
    )?;
    Ok((items, total))
}

/// Deletes ids one by one inside `tx`, reporting the missing ones.
fn batch_delete(
    tx: &Transaction<'_>,
    ids: &[String],
    mut delete_one: impl FnMut(&Transaction<'_>, &str) -> rusqlite::Result<usize>,
) -> rusqlite::Result<BatchDeleteResult> {
    let mut result = BatchDeleteResult::default();
    for id in ids {
        if delete_one(tx, id)? > 0 {
            result.deleted_ids.push(id.clone());
        } else {
            result.failed.push(FailedId {
                id: id.clone(),
                reason: "not_found".into(),
            });
        }
    }
    Ok(result)
}

fn delete_asset_rows(tx: &Transaction<'_>, asset_id: &str) -> rusqlite::Result<usize> {
    tx.execute(
        "DELETE FROM meta_agent_bindings WHERE asset_id = ?",
        [asset_id],
    )?;
    tx.execute("DELETE FROM meta_asset_acl WHERE asset_id = ?", [asset_id])?;
    tx.execute("DELETE FROM meta_assets WHERE asset_id = ?", [asset_id])
}

fn insert_member(conn: &Connection, m: &TeamMember) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT OR REPLACE INTO meta_team_members (team_id, user_id, status, joined_at, doc) VALUES (?,?,?,?,?)",
        params![m.team_id, m.user_id, m.status.as_str(), m.joined_at, to_doc(m)?],
    )
}

fn insert_key(conn: &Connection, k: &UserKey) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO meta_user_keys (key_id, user_id, key_value, created_at, doc) VALUES (?,?,?,?,?)",
        params![k.key_id, k.user_id, k.key_value, k.created_at, to_doc(k)?],
    )
}

fn insert_link(conn: &Connection, l: &TaskAgent) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT OR REPLACE INTO meta_task_agents (task_id, agent_id, created_at, doc) VALUES (?,?,?,?)",
        params![l.task_id, l.agent_id, l.created_at, to_doc(l)?],
    )
}

fn insert_binding(conn: &Connection, b: &AgentBinding, replace: bool) -> rusqlite::Result<usize> {
    let verb = if replace {
        "INSERT OR REPLACE"
    } else {
        "INSERT OR IGNORE"
    };
    conn.execute(
        &format!(
            "{verb} INTO meta_agent_bindings (agent_id, asset_id, priority, doc) VALUES (?,?,?,?)"
        ),
        params![b.agent_id, b.asset_id, b.priority, to_doc(b)?],
    )
}

#[async_trait]
impl UserRepository for SqliteMetaStore {
    async fn create_user(
        &self,
        department: &DepartmentId,
        user: &User,
        default_key: &UserKey,
    ) -> DomainResult<()> {
        let (user, key) = (user.clone(), default_key.clone());
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO meta_users (user_id, username, user_type, status, created_at, doc) VALUES (?,?,?,?,?,?)",
                params![user.user_id, user.username, user.user_type.as_str(), user.status.as_str(), user.created_at, to_doc(&user)?],
            )?;
            insert_key(&tx, &key)?;
            tx.commit()
        })
        .await
    }

    async fn get_user(
        &self,
        department: &DepartmentId,
        user_id: &str,
    ) -> DomainResult<Option<User>> {
        let id = user_id.to_owned();
        self.with(department, move |c| {
            one(c, "SELECT doc FROM meta_users WHERE user_id = ?", [id])
        })
        .await
    }

    async fn find_user_by_name(
        &self,
        department: &DepartmentId,
        username: &str,
    ) -> DomainResult<Option<User>> {
        let name = username.to_owned();
        self.with(department, move |c| {
            one(c, "SELECT doc FROM meta_users WHERE username = ?", [name])
        })
        .await
    }

    async fn list_users(
        &self,
        department: &DepartmentId,
        filter: &UserFilter,
        page: Page,
    ) -> DomainResult<(Vec<User>, usize)> {
        let mut w = Where::default();
        if let Some(team) = &filter.team_id {
            w.raw("user_id IN (SELECT user_id FROM meta_team_members WHERE team_id = ? AND status = 'active')", [Value::from(team.clone())]);
        }
        if let Some(ids) = &filter.user_ids {
            w.in_list("user_id", ids);
        }
        if let Some(name) = &filter.username {
            w.raw("username LIKE ?", [Value::from(format!("%{name}%"))]);
        }
        w.eq_opt("user_type", filter.user_type.map(|t| t.as_str().to_owned()));
        w.eq_opt("status", filter.status.map(|s| s.as_str().to_owned()));
        self.with(department, move |c| {
            paged(c, "meta_users", &w, "created_at, user_id", page)
        })
        .await
    }

    async fn count_users(&self, department: &DepartmentId) -> DomainResult<usize> {
        self.with(department, |c| {
            c.query_row("SELECT COUNT(*) FROM meta_users", [], |r| r.get(0))
        })
        .await
    }

    async fn delete_users(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let ids = ids.to_vec();
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            let result = batch_delete(&tx, &ids, |tx, id| {
                tx.execute("DELETE FROM meta_user_keys WHERE user_id = ?", [id])?;
                tx.execute("DELETE FROM meta_team_members WHERE user_id = ?", [id])?;
                tx.execute(
                    "DELETE FROM meta_asset_acl WHERE subject_type = 'user' AND subject_id = ?",
                    [id],
                )?;
                tx.execute(
                    "DELETE FROM meta_config_params WHERE scope = 'user' AND user_id = ?",
                    [id],
                )?;
                tx.execute("DELETE FROM meta_users WHERE user_id = ?", [id])
            })?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }
}

#[async_trait]
impl UserKeyRepository for SqliteMetaStore {
    async fn insert_key(&self, department: &DepartmentId, key: &UserKey) -> DomainResult<()> {
        let key = key.clone();
        self.with(department, move |c| insert_key(c, &key).map(|_| ()))
            .await
    }

    async fn find_key_by_value(
        &self,
        department: &DepartmentId,
        value: &str,
    ) -> DomainResult<Option<UserKey>> {
        let v = value.to_owned();
        self.with(department, move |c| {
            one(c, "SELECT doc FROM meta_user_keys WHERE key_value = ?", [v])
        })
        .await
    }

    async fn get_key(
        &self,
        department: &DepartmentId,
        key_id: &str,
    ) -> DomainResult<Option<UserKey>> {
        let id = key_id.to_owned();
        self.with(department, move |c| {
            one(c, "SELECT doc FROM meta_user_keys WHERE key_id = ?", [id])
        })
        .await
    }

    async fn list_keys(
        &self,
        department: &DepartmentId,
        user_id: &str,
        page: Page,
    ) -> DomainResult<(Vec<UserKey>, usize)> {
        let mut w = Where::default();
        w.eq("user_id", user_id.to_owned());
        self.with(department, move |c| {
            paged(c, "meta_user_keys", &w, "created_at, key_id", page)
        })
        .await
    }

    async fn update_key(&self, department: &DepartmentId, key: &UserKey) -> DomainResult<()> {
        let key = key.clone();
        self.with(department, move |c| {
            c.execute(
                "UPDATE meta_user_keys SET doc = ? WHERE key_id = ?",
                params![to_doc(&key)?, key.key_id],
            )
            .map(|_| ())
        })
        .await
    }

    async fn delete_key(&self, department: &DepartmentId, key_id: &str) -> DomainResult<()> {
        let id = key_id.to_owned();
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            let removed: Option<UserKey> = one(
                &tx,
                "SELECT doc FROM meta_user_keys WHERE key_id = ?",
                [&id],
            )?;
            tx.execute("DELETE FROM meta_user_keys WHERE key_id = ?", [&id])?;
            if let Some(key) = removed.filter(|k| k.is_default) {
                let next: Option<UserKey> = one(
                    &tx,
                    "SELECT doc FROM meta_user_keys WHERE user_id = ? ORDER BY created_at LIMIT 1",
                    [&key.user_id],
                )?;
                if let Some(mut promoted) = next {
                    promoted.is_default = true;
                    tx.execute(
                        "UPDATE meta_user_keys SET doc = ? WHERE key_id = ?",
                        params![to_doc(&promoted)?, promoted.key_id],
                    )?;
                }
            }
            tx.commit()
        })
        .await
    }

    async fn touch_key(
        &self,
        department: &DepartmentId,
        key_id: &str,
        at: &str,
    ) -> DomainResult<()> {
        let (id, at) = (key_id.to_owned(), at.to_owned());
        self.with(department, move |c| {
            c.execute("UPDATE meta_user_keys SET doc = json_set(doc, '$.last_used_at', ?) WHERE key_id = ?", params![at, id]).map(|_| ())
        })
        .await
    }
}

#[async_trait]
impl TeamRepository for SqliteMetaStore {
    async fn create_team(
        &self,
        department: &DepartmentId,
        team: &Team,
        owner: &TeamMember,
    ) -> DomainResult<()> {
        let (team, owner) = (team.clone(), owner.clone());
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO meta_teams (team_id, name, created_at, doc) VALUES (?,?,?,?)",
                params![team.team_id, team.name, team.created_at, to_doc(&team)?],
            )?;
            insert_member(&tx, &owner)?;
            tx.commit()
        })
        .await
    }

    async fn get_team(
        &self,
        department: &DepartmentId,
        team_id: &str,
    ) -> DomainResult<Option<Team>> {
        let id = team_id.to_owned();
        self.with(department, move |c| {
            one(c, "SELECT doc FROM meta_teams WHERE team_id = ?", [id])
        })
        .await
    }

    async fn update_team(&self, department: &DepartmentId, team: &Team) -> DomainResult<()> {
        let team = team.clone();
        self.with(department, move |c| {
            c.execute(
                "UPDATE meta_teams SET name = ?, doc = ? WHERE team_id = ?",
                params![team.name, to_doc(&team)?, team.team_id],
            )
            .map(|_| ())
        })
        .await
    }

    async fn list_teams_of_user(
        &self,
        department: &DepartmentId,
        user_id: Option<&str>,
        name: Option<&str>,
        page: Page,
    ) -> DomainResult<(Vec<Team>, usize)> {
        let mut w = Where::default();
        if let Some(user_id) = user_id {
            w.raw("team_id IN (SELECT team_id FROM meta_team_members WHERE user_id = ? AND status = 'active')", [Value::from(user_id.to_owned())]);
        }
        if let Some(n) = name {
            w.raw("name LIKE ?", [Value::from(format!("%{n}%"))]);
        }
        self.with(department, move |c| {
            paged(c, "meta_teams", &w, "created_at, team_id", page)
        })
        .await
    }

    async fn count_teams(&self, department: &DepartmentId) -> DomainResult<usize> {
        self.with(department, |c| {
            c.query_row("SELECT COUNT(*) FROM meta_teams", [], |r| r.get(0))
        })
        .await
    }

    async fn delete_teams(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let ids = ids.to_vec();
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            let result = batch_delete(&tx, &ids, |tx, id| {
                let assets: Vec<String> = {
                    let mut stmt = tx.prepare("SELECT asset_id FROM meta_assets WHERE team_id = ?")?;
                    let rows = stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                    rows
                };
                for asset in &assets {
                    delete_asset_rows(tx, asset)?;
                }
                tx.execute("DELETE FROM meta_agent_bindings WHERE agent_id IN (SELECT agent_id FROM meta_agents WHERE team_id = ?)", [id])?;
                tx.execute("DELETE FROM meta_task_agents WHERE task_id IN (SELECT task_id FROM meta_tasks WHERE team_id = ?)", [id])?;
                tx.execute("DELETE FROM meta_tasks WHERE team_id = ?", [id])?;
                tx.execute("DELETE FROM meta_agents WHERE team_id = ?", [id])?;
                tx.execute("DELETE FROM meta_team_members WHERE team_id = ?", [id])?;
                tx.execute("DELETE FROM meta_teams WHERE team_id = ?", [id])
            })?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }
}

#[async_trait]
impl MemberRepository for SqliteMetaStore {
    async fn upsert_member(
        &self,
        department: &DepartmentId,
        member: &TeamMember,
    ) -> DomainResult<()> {
        let m = member.clone();
        self.with(department, move |c| insert_member(c, &m).map(|_| ()))
            .await
    }

    async fn get_member(
        &self,
        department: &DepartmentId,
        team_id: &str,
        user_id: &str,
    ) -> DomainResult<Option<TeamMember>> {
        let (t, u) = (team_id.to_owned(), user_id.to_owned());
        self.with(department, move |c| {
            one(
                c,
                "SELECT doc FROM meta_team_members WHERE team_id = ? AND user_id = ?",
                [t, u],
            )
        })
        .await
    }

    async fn list_members(
        &self,
        department: &DepartmentId,
        team_id: &str,
        page: Page,
    ) -> DomainResult<(Vec<TeamMember>, usize)> {
        let mut w = Where::default();
        w.eq("team_id", team_id.to_owned())
            .eq("status", "active".to_owned());
        self.with(department, move |c| {
            paged(c, "meta_team_members", &w, "joined_at, user_id", page)
        })
        .await
    }

    async fn list_memberships(
        &self,
        department: &DepartmentId,
        user_id: &str,
    ) -> DomainResult<Vec<TeamMember>> {
        let u = user_id.to_owned();
        self.with(department, move |c| {
            many(
                c,
                "SELECT doc FROM meta_team_members WHERE user_id = ?",
                &[Value::from(u)],
            )
        })
        .await
    }

    async fn remove_member(
        &self,
        department: &DepartmentId,
        team_id: &str,
        user_id: &str,
    ) -> DomainResult<bool> {
        let (t, u) = (team_id.to_owned(), user_id.to_owned());
        self.with(department, move |c| {
            c.execute(
                "DELETE FROM meta_team_members WHERE team_id = ? AND user_id = ?",
                [t, u],
            )
            .map(|n| n > 0)
        })
        .await
    }
}

#[async_trait]
impl AgentRepository for SqliteMetaStore {
    async fn insert_agent(&self, department: &DepartmentId, agent: &Agent) -> DomainResult<()> {
        let a = agent.clone();
        self.with(department, move |c| {
            c.execute(
                "INSERT INTO meta_agents (agent_id, team_id, owner_user_id, status, name, created_at, doc) VALUES (?,?,?,?,?,?,?)",
                params![a.agent_id, a.team_id, a.owner_user_id, a.status.as_str(), a.name, a.created_at, to_doc(&a)?],
            )
            .map(|_| ())
        })
        .await
    }

    async fn get_agent(
        &self,
        department: &DepartmentId,
        agent_id: &str,
    ) -> DomainResult<Option<Agent>> {
        let id = agent_id.to_owned();
        self.with(department, move |c| {
            one(c, "SELECT doc FROM meta_agents WHERE agent_id = ?", [id])
        })
        .await
    }

    async fn update_agent(&self, department: &DepartmentId, agent: &Agent) -> DomainResult<()> {
        let a = agent.clone();
        self.with(department, move |c| {
            c.execute(
                "UPDATE meta_agents SET status = ?, name = ?, doc = ? WHERE agent_id = ?",
                params![a.status.as_str(), a.name, to_doc(&a)?, a.agent_id],
            )
            .map(|_| ())
        })
        .await
    }

    async fn list_agents(
        &self,
        department: &DepartmentId,
        filter: &AgentFilter,
        page: Page,
    ) -> DomainResult<(Vec<Agent>, usize)> {
        let mut w = Where::default();
        w.eq_opt("team_id", filter.team_id.clone())
            .eq_opt("owner_user_id", filter.owner_user_id.clone())
            .eq_opt("status", filter.status.map(|s| s.as_str().to_owned()));
        if let Some(n) = &filter.name {
            w.raw("name LIKE ?", [Value::from(format!("%{n}%"))]);
        }
        self.with(department, move |c| {
            paged(c, "meta_agents", &w, "created_at, agent_id", page)
        })
        .await
    }

    async fn delete_agents(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let ids = ids.to_vec();
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            let result = batch_delete(&tx, &ids, |tx, id| {
                let team: Option<String> = tx
                    .query_row(
                        "SELECT team_id FROM meta_agents WHERE agent_id = ?",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(team) = team {
                    delete_asset_rows(tx, &chat_memory_asset_id(&team, id))?;
                }
                tx.execute("DELETE FROM meta_task_agents WHERE agent_id = ?", [id])?;
                tx.execute("DELETE FROM meta_agent_bindings WHERE agent_id = ?", [id])?;
                tx.execute("DELETE FROM meta_agents WHERE agent_id = ?", [id])
            })?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }
}

#[async_trait]
impl TaskRepository for SqliteMetaStore {
    async fn insert_task(
        &self,
        department: &DepartmentId,
        task: &Task,
        links: &[TaskAgent],
    ) -> DomainResult<()> {
        let (t, links) = (task.clone(), links.to_vec());
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO meta_tasks (task_id, team_id, creator_user_id, status, created_at, doc) VALUES (?,?,?,?,?,?)",
                params![t.task_id, t.team_id, t.creator_user_id, t.status.as_str(), t.created_at, to_doc(&t)?],
            )?;
            for l in &links {
                insert_link(&tx, l)?;
            }
            tx.commit()
        })
        .await
    }

    async fn get_task(
        &self,
        department: &DepartmentId,
        task_id: &str,
    ) -> DomainResult<Option<Task>> {
        let id = task_id.to_owned();
        self.with(department, move |c| {
            one(c, "SELECT doc FROM meta_tasks WHERE task_id = ?", [id])
        })
        .await
    }

    async fn update_task(&self, department: &DepartmentId, task: &Task) -> DomainResult<()> {
        let t = task.clone();
        self.with(department, move |c| {
            c.execute(
                "UPDATE meta_tasks SET status = ?, doc = ? WHERE task_id = ?",
                params![t.status.as_str(), to_doc(&t)?, t.task_id],
            )
            .map(|_| ())
        })
        .await
    }

    async fn list_tasks(
        &self,
        department: &DepartmentId,
        filter: &TaskFilter,
        page: Page,
    ) -> DomainResult<(Vec<Task>, usize)> {
        let mut w = Where::default();
        w.eq_opt("team_id", filter.team_id.clone())
            .eq_opt("creator_user_id", filter.creator_user_id.clone())
            .eq_opt("status", filter.status.map(|s| s.as_str().to_owned()));
        self.with(department, move |c| {
            paged(c, "meta_tasks", &w, "created_at DESC, task_id", page)
        })
        .await
    }

    async fn delete_tasks(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let ids = ids.to_vec();
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            let result = batch_delete(&tx, &ids, |tx, id| {
                tx.execute("DELETE FROM meta_task_agents WHERE task_id = ?", [id])?;
                tx.execute("DELETE FROM meta_tasks WHERE task_id = ?", [id])
            })?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }

    async fn link_agent(&self, department: &DepartmentId, link: &TaskAgent) -> DomainResult<()> {
        let l = link.clone();
        self.with(department, move |c| insert_link(c, &l).map(|_| ()))
            .await
    }

    async fn unlink_agent(
        &self,
        department: &DepartmentId,
        task_id: &str,
        agent_id: &str,
    ) -> DomainResult<bool> {
        let (t, a) = (task_id.to_owned(), agent_id.to_owned());
        self.with(department, move |c| {
            c.execute(
                "DELETE FROM meta_task_agents WHERE task_id = ? AND agent_id = ?",
                [t, a],
            )
            .map(|n| n > 0)
        })
        .await
    }

    async fn list_task_agents(
        &self,
        department: &DepartmentId,
        task_id: &str,
        page: Page,
    ) -> DomainResult<(Vec<TaskAgent>, usize)> {
        let mut w = Where::default();
        w.eq("task_id", task_id.to_owned());
        self.with(department, move |c| {
            paged(c, "meta_task_agents", &w, "created_at, agent_id", page)
        })
        .await
    }

    async fn append_participation(
        &self,
        department: &DepartmentId,
        log: &ParticipationLog,
    ) -> DomainResult<()> {
        let l = log.clone();
        self.with(department, move |c| {
            c.execute(
                "INSERT INTO meta_participation_logs (id, team_id, task_id, agent_id, user_id, created_at, doc) VALUES (?,?,?,?,?,?,?)",
                params![l.id, l.team_id, l.task_id, l.agent_id, l.user_id, l.created_at, to_doc(&l)?],
            )
            .map(|_| ())
        })
        .await
    }

    async fn list_participation(
        &self,
        department: &DepartmentId,
        filter: &ParticipationFilter,
        page: Page,
    ) -> DomainResult<(Vec<ParticipationLog>, usize)> {
        let mut w = Where::default();
        w.eq("team_id", filter.team_id.clone())
            .eq_opt("task_id", filter.task_id.clone())
            .eq_opt("agent_id", filter.agent_id.clone())
            .eq_opt("user_id", filter.user_id.clone());
        if let Some(a) = &filter.created_after {
            w.raw("created_at >= ?", [Value::from(a.clone())]);
        }
        if let Some(b) = &filter.created_before {
            w.raw("created_at <= ?", [Value::from(b.clone())]);
        }
        self.with(department, move |c| {
            paged(
                c,
                "meta_participation_logs",
                &w,
                "created_at DESC, id",
                page,
            )
        })
        .await
    }
}

#[async_trait]
impl AssetRepository for SqliteMetaStore {
    async fn insert_asset(&self, department: &DepartmentId, asset: &Asset) -> DomainResult<()> {
        let a = asset.clone();
        self.with(department, move |c| {
            c.execute(
                "INSERT INTO meta_assets (asset_id, team_id, asset_type, status, owner_user_id, visibility, created_at, doc) VALUES (?,?,?,?,?,?,?,?)",
                params![a.asset_id, a.team_id, a.asset_type.as_str(), a.status.as_str(), a.owner_user_id, a.visibility.as_str(), a.created_at, to_doc(&a)?],
            )
            .map(|_| ())
        })
        .await
    }

    async fn get_asset(
        &self,
        department: &DepartmentId,
        asset_id: &str,
    ) -> DomainResult<Option<Asset>> {
        let id = asset_id.to_owned();
        self.with(department, move |c| {
            one(c, "SELECT doc FROM meta_assets WHERE asset_id = ?", [id])
        })
        .await
    }

    async fn update_asset(&self, department: &DepartmentId, asset: &Asset) -> DomainResult<()> {
        let a = asset.clone();
        self.with(department, move |c| {
            c.execute(
                "UPDATE meta_assets SET status = ?, visibility = ?, doc = ? WHERE asset_id = ?",
                params![
                    a.status.as_str(),
                    a.visibility.as_str(),
                    to_doc(&a)?,
                    a.asset_id
                ],
            )
            .map(|_| ())
        })
        .await
    }

    async fn list_assets(
        &self,
        department: &DepartmentId,
        filter: &AssetFilter,
    ) -> DomainResult<Vec<Asset>> {
        let mut w = Where::default();
        w.eq_opt("team_id", filter.team_id.clone())
            .eq_opt(
                "asset_type",
                filter.asset_type.map(|t| t.as_str().to_owned()),
            )
            .eq_opt("status", filter.status.map(|s| s.as_str().to_owned()))
            .eq_opt("owner_user_id", filter.owner_user_id.clone());
        if let Some(vs) = &filter.visibility {
            let values: Vec<String> = vs.iter().map(|v| v.as_str().to_owned()).collect();
            w.in_list("visibility", &values);
        }
        self.with(department, move |c| {
            many(
                c,
                &format!(
                    "SELECT doc FROM meta_assets {} ORDER BY created_at DESC, asset_id",
                    w.sql()
                ),
                &w.params,
            )
        })
        .await
    }

    async fn delete_assets(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let ids = ids.to_vec();
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            for id in &ids {
                delete_asset_rows(&tx, id)?;
            }
            tx.commit()?;
            Ok(BatchDeleteResult {
                deleted_ids: ids,
                failed: Vec::new(),
            })
        })
        .await
    }

    async fn touch_asset(
        &self,
        department: &DepartmentId,
        asset_id: &str,
        at: &str,
    ) -> DomainResult<()> {
        let (id, at) = (asset_id.to_owned(), at.to_owned());
        self.with(department, move |c| {
            c.execute(
                "UPDATE meta_assets SET doc = json_set(doc, '$.last_used_at', ?, '$.usage_count', json_extract(doc, '$.usage_count') + 1) WHERE asset_id = ?",
                params![at, id],
            )
            .map(|_| ())
        })
        .await
    }
}

#[async_trait]
impl BindingRepository for SqliteMetaStore {
    async fn replace_bindings(
        &self,
        department: &DepartmentId,
        agent_id: &str,
        bindings: &[AgentBinding],
    ) -> DomainResult<()> {
        let (agent, bindings) = (agent_id.to_owned(), bindings.to_vec());
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM meta_agent_bindings WHERE agent_id = ?",
                [&agent],
            )?;
            for b in &bindings {
                insert_binding(&tx, b, true)?;
            }
            tx.commit()
        })
        .await
    }

    async fn ensure_binding(
        &self,
        department: &DepartmentId,
        binding: &AgentBinding,
    ) -> DomainResult<()> {
        let b = binding.clone();
        self.with(department, move |c| {
            insert_binding(c, &b, false).map(|_| ())
        })
        .await
    }

    async fn list_bindings(
        &self,
        department: &DepartmentId,
        agent_id: &str,
    ) -> DomainResult<Vec<AgentBinding>> {
        let a = agent_id.to_owned();
        self.with(department, move |c| many(c, "SELECT doc FROM meta_agent_bindings WHERE agent_id = ? ORDER BY priority DESC, asset_id", &[Value::from(a)])).await
    }
}

#[async_trait]
impl AclRepository for SqliteMetaStore {
    async fn grant(&self, department: &DepartmentId, entry: &AclEntry) -> DomainResult<AclEntry> {
        let e = entry.clone();
        self.with(department, move |conn| {
            let existing: Option<AclEntry> = one(
                conn,
                "SELECT doc FROM meta_asset_acl WHERE asset_id = ? AND subject_type = ? AND subject_id = ? AND permission = ?",
                params![e.asset_id, e.subject_type.as_str(), e.subject_id, e.permission.as_str()],
            )?;
            let stored = match existing {
                Some(mut old) => {
                    old.effect = e.effect;
                    old.granted_by = e.granted_by;
                    old.updated_at = e.updated_at;
                    old
                }
                None => e,
            };
            conn.execute(
                "INSERT OR REPLACE INTO meta_asset_acl (id, asset_id, subject_type, subject_id, permission, doc) VALUES (?,?,?,?,?,?)",
                params![stored.id, stored.asset_id, stored.subject_type.as_str(), stored.subject_id, stored.permission.as_str(), to_doc(&stored)?],
            )?;
            Ok(stored)
        })
        .await
    }

    async fn revoke(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<AclEntry>> {
        let id = id.to_owned();
        self.with(department, move |c| {
            let entry: Option<AclEntry> =
                one(c, "SELECT doc FROM meta_asset_acl WHERE id = ?", [&id])?;
            c.execute("DELETE FROM meta_asset_acl WHERE id = ?", [&id])?;
            Ok(entry)
        })
        .await
    }

    async fn get_acl(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<AclEntry>> {
        let id = id.to_owned();
        self.with(department, move |c| {
            one(c, "SELECT doc FROM meta_asset_acl WHERE id = ?", [id])
        })
        .await
    }

    async fn list_acl(
        &self,
        department: &DepartmentId,
        asset_id: &str,
    ) -> DomainResult<Vec<AclEntry>> {
        let a = asset_id.to_owned();
        self.with(department, move |c| {
            many(
                c,
                "SELECT doc FROM meta_asset_acl WHERE asset_id = ? ORDER BY id",
                &[Value::from(a)],
            )
        })
        .await
    }
}

#[async_trait]
impl ConfigRepository for SqliteMetaStore {
    async fn get_params(
        &self,
        department: &DepartmentId,
        user_id: Option<&str>,
        module: &str,
    ) -> DomainResult<Vec<ConfigParam>> {
        let (user, module) = (user_id.unwrap_or("").to_owned(), module.to_owned());
        self.with(department, move |c| {
            // User values shadow globals: the `user` scope sorts after `global`.
            let mut stmt = c.prepare(
                "SELECT scope, module, param_name, param_value FROM meta_config_params \
                 WHERE module = ? AND (scope = 'global' OR (scope = 'user' AND user_id = ?)) ORDER BY param_name, scope",
            )?;
            let rows = stmt
                .query_map(params![module, user], |r| {
                    Ok(ConfigParam { scope: r.get(0)?, module: r.get(1)?, param_name: r.get(2)?, param_value: r.get(3)? })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut effective: Vec<ConfigParam> = Vec::new();
            for p in rows {
                match effective.last_mut() {
                    Some(last) if last.param_name == p.param_name => *last = p,
                    _ => effective.push(p),
                }
            }
            Ok(effective)
        })
        .await
    }

    async fn set_user_params(
        &self,
        department: &DepartmentId,
        user_id: &str,
        module: &str,
        params_in: &[(String, String)],
    ) -> DomainResult<()> {
        let (user, module, values) = (user_id.to_owned(), module.to_owned(), params_in.to_vec());
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            for (name, value) in &values {
                tx.execute(
                    "INSERT OR REPLACE INTO meta_config_params VALUES ('user', ?, ?, ?, ?)",
                    params![user, module, name, value],
                )?;
            }
            tx.commit()
        })
        .await
    }
}

#[async_trait]
impl KnowledgeRepository for SqliteMetaStore {
    async fn upsert_knowledge(
        &self,
        department: &DepartmentId,
        entity: &KnowledgeEntity,
    ) -> DomainResult<()> {
        let e = entity.clone();
        self.with(department, move |c| {
            c.execute(
                "INSERT OR REPLACE INTO knowledge (id, team_id, kind, created_at, doc) VALUES (?,?,?,?,?)",
                params![e.knowledge_id, e.team_id, e.kind, e.created_at, to_doc(&e)?],
            )
            .map(|_| ())
        })
        .await
    }

    async fn get_knowledge(
        &self,
        department: &DepartmentId,
        id: &str,
    ) -> DomainResult<Option<KnowledgeEntity>> {
        let id = id.to_owned();
        self.with(department, move |c| {
            one(c, "SELECT doc FROM knowledge WHERE id = ?", [id])
        })
        .await
    }

    async fn delete_knowledge(
        &self,
        department: &DepartmentId,
        ids: &[String],
    ) -> DomainResult<BatchDeleteResult> {
        let ids = ids.to_vec();
        self.with(department, move |conn| {
            let tx = conn.transaction()?;
            let result = batch_delete(&tx, &ids, |tx, id| {
                tx.execute("DELETE FROM knowledge WHERE id = ?", [id])
            })?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }

    async fn list_knowledge(
        &self,
        department: &DepartmentId,
        team_id: &str,
        kind: Option<&str>,
        ids: Option<&[String]>,
        page: Page,
    ) -> DomainResult<(Vec<KnowledgeEntity>, usize)> {
        let mut w = Where::default();
        w.eq("team_id", team_id.to_owned())
            .eq_opt("kind", kind.map(str::to_owned));
        if let Some(ids) = ids {
            w.in_list("id", ids);
        }
        self.with(department, move |c| {
            paged(c, "knowledge", &w, "created_at DESC, id", page)
        })
        .await
    }
}

#[async_trait]
impl RedactionRuleRepository for SqliteMetaStore {
    async fn list_redaction_rules(
        &self,
        department: &DepartmentId,
    ) -> DomainResult<Vec<RedactionRule>> {
        self.with(department, |c| {
            many(
                c,
                "SELECT doc FROM meta_redaction_rules ORDER BY created_at, rule_id",
                &[],
            )
        })
        .await
    }

    async fn put_redaction_rule(
        &self,
        department: &DepartmentId,
        rule: &RedactionRule,
    ) -> DomainResult<()> {
        let r = rule.clone();
        self.with(department, move |c| {
            c.execute(
                "INSERT OR REPLACE INTO meta_redaction_rules (rule_id, created_at, doc) VALUES (?,?,?)",
                params![r.rule_id, r.created_at, to_doc(&r)?],
            )
            .map(|_| ())
        })
        .await
    }
}
