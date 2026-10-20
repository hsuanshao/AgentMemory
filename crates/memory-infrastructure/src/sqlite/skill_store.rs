//! SQLite adapter for skill versions (FTS on the head row), conversation
//! buffers, archives and extraction tasks.

use std::sync::Arc;

use async_trait::async_trait;
use memory_application::ports::skill::{
    BufferCounters, BufferKey, BufferedMessage, SkillBufferStore, SkillFilter, SkillRepository,
    SkillTask,
};
use memory_domain::error::DomainResult;
use memory_domain::scope::DepartmentId;
use memory_domain::skill::{SkillStatus, SkillVersion};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};

use super::db::DepartmentPool;
use super::sql::{from_doc, like_prefix, to_doc, Where};
use crate::text::{bm25_to_score, index_text, match_query};

pub struct SqliteSkillStore {
    pool: Arc<DepartmentPool>,
}

impl SqliteSkillStore {
    pub fn new(pool: Arc<DepartmentPool>) -> Self {
        Self { pool }
    }
}

fn status_str(s: SkillStatus) -> &'static str {
    match s {
        SkillStatus::Active => "active",
        SkillStatus::Archived => "archived",
    }
}

fn one_version(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Option<SkillVersion>> {
    let doc: Option<String> = conn.query_row(sql, params, |r| r.get(0)).optional()?;
    doc.map(|d| from_doc(&d)).transpose()
}

fn versions(conn: &Connection, sql: &str, params: &[Value]) -> rusqlite::Result<Vec<SkillVersion>> {
    let mut stmt = conn.prepare(sql)?;
    let docs = stmt
        .query_map(params_from_iter(params.iter()), |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    docs.iter().map(|d| from_doc(d)).collect()
}

fn head_where(f: &SkillFilter, prefix: &str) -> Where {
    let col = |c: &str| format!("{prefix}{c}");
    let mut w = Where::default();
    w.raw(&format!("{} = 1", col("is_head")), [])
        .eq_opt(&col("team_id"), f.team_id.clone())
        .eq_opt(&col("owner_agent_id"), f.owner_agent_id.clone())
        .eq_opt(&col("owner_user_id"), f.owner_user_id.clone());
    if let Some(p) = &f.name_prefix {
        w.raw(
            &format!("{} LIKE ? ESCAPE '\\'", col("name")),
            [Value::from(like_prefix(p))],
        );
    }
    let statuses: Vec<String> = f
        .statuses
        .iter()
        .map(|s| status_str(*s).to_owned())
        .collect();
    w.in_list(&col("status"), &statuses);
    w
}

fn snippet(v: &SkillVersion) -> String {
    let source = if v.description.is_empty() {
        &v.content
    } else {
        &v.description
    };
    source.chars().take(160).collect()
}

#[async_trait]
impl SkillRepository for SqliteSkillStore {
    async fn skill_id_exists(
        &self,
        department: &DepartmentId,
        skill_id: &str,
    ) -> DomainResult<bool> {
        let id = skill_id.to_owned();
        self.pool
            .get(department)?
            .call(move |c| {
                c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM skills WHERE skill_id = ?)",
                    [id],
                    |r| r.get(0),
                )
            })
            .await
    }

    async fn head(
        &self,
        department: &DepartmentId,
        skill_id: &str,
    ) -> DomainResult<Option<SkillVersion>> {
        let id = skill_id.to_owned();
        self.pool
            .get(department)?
            .call(move |c| {
                one_version(
                    c,
                    "SELECT doc FROM skills WHERE skill_id = ? AND is_head = 1",
                    [id],
                )
            })
            .await
    }

    async fn version(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
    ) -> DomainResult<Option<SkillVersion>> {
        let id = skill_id.to_owned();
        self.pool
            .get(department)?
            .call(move |c| {
                one_version(
                    c,
                    "SELECT doc FROM skills WHERE skill_id = ? AND version = ?",
                    params![id, version],
                )
            })
            .await
    }

    async fn find_head_by_name(
        &self,
        department: &DepartmentId,
        team_id: &str,
        agent_id: &str,
        name: &str,
    ) -> DomainResult<Option<SkillVersion>> {
        let (t, a, n) = (team_id.to_owned(), agent_id.to_owned(), name.to_owned());
        self.pool
            .get(department)?
            .call(move |c| {
                one_version(
                    c,
                    "SELECT doc FROM skills WHERE is_head = 1 AND status = 'active' AND team_id = ? AND owner_agent_id = ? AND name = ?",
                    [t, a, n],
                )
            })
            .await
    }

    async fn append_version(
        &self,
        department: &DepartmentId,
        next: &SkillVersion,
    ) -> DomainResult<()> {
        let v = next.clone();
        self.pool
            .get(department)?
            .call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute("UPDATE skills SET is_head = 0, doc = json_set(doc, '$.is_head', json('false')) WHERE skill_id = ? AND is_head = 1", [&v.skill_id])?;
                tx.execute(
                    "INSERT INTO skills (skill_id, version, is_head, name, status, team_id, owner_agent_id, owner_user_id, updated_at_ms, created_at_ms, doc) \
                     VALUES (?,?,1,?,?,?,?,?,?,?,?)",
                    params![v.skill_id, v.version, v.name, status_str(v.status), v.team_id, v.owner_agent_id, v.owner_user_id, v.updated_at_ms, v.created_at_ms, to_doc(&v)?],
                )?;
                tx.execute("DELETE FROM skills_fts WHERE skill_id = ?", [&v.skill_id])?;
                let body: String = v.content.chars().take(4000).collect();
                tx.execute(
                    "INSERT INTO skills_fts (tokens, skill_id) VALUES (?, ?)",
                    params![index_text(&format!("{} {} {}", v.name.replace('-', " "), v.description, body)), v.skill_id],
                )?;
                tx.commit()
            })
            .await
    }

    async fn list_heads(
        &self,
        department: &DepartmentId,
        filter: &SkillFilter,
        limit: usize,
        offset: usize,
    ) -> DomainResult<(Vec<SkillVersion>, usize)> {
        let w = head_where(filter, "");
        self.pool
            .get(department)?
            .call(move |c| {
                let total: usize = c.query_row(&format!("SELECT COUNT(*) FROM skills {}", w.sql()), params_from_iter(w.params.iter()), |r| r.get(0))?;
                let sql = format!("SELECT doc FROM skills {} ORDER BY updated_at_ms DESC, skill_id LIMIT ? OFFSET ?", w.sql());
                Ok((versions(c, &sql, &w.with([Value::from(limit as i64), Value::from(offset as i64)]))?, total))
            })
            .await
    }

    async fn search_heads(
        &self,
        department: &DepartmentId,
        filter: &SkillFilter,
        query: &str,
        limit: usize,
    ) -> DomainResult<Vec<(SkillVersion, f64, String)>> {
        let Some(fts) = match_query(query) else {
            return Ok(Vec::new());
        };
        let mut w = head_where(filter, "s.");
        w.raw("skills_fts MATCH ?", [Value::from(fts)]);
        self.pool
            .get(department)?
            .call(move |c| {
                let sql = format!(
                    "SELECT s.doc, bm25(skills_fts) AS rank FROM skills_fts JOIN skills s ON s.skill_id = skills_fts.skill_id {} ORDER BY rank LIMIT ?",
                    w.sql()
                );
                let mut stmt = c.prepare(&sql)?;
                let rows = stmt
                    .query_map(params_from_iter(w.with([Value::from(limit as i64)])), |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.into_iter()
                    .map(|(doc, rank)| {
                        let v: SkillVersion = from_doc(&doc)?;
                        let snip = snippet(&v);
                        Ok((v, bm25_to_score(rank), snip))
                    })
                    .collect()
            })
            .await
    }

    async fn list_versions(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        limit: usize,
        offset: usize,
    ) -> DomainResult<(Vec<SkillVersion>, usize)> {
        let id = skill_id.to_owned();
        self.pool
            .get(department)?
            .call(move |c| {
                let total: usize = c.query_row("SELECT COUNT(*) FROM skills WHERE skill_id = ?", [&id], |r| r.get(0))?;
                let items = versions(c, "SELECT doc FROM skills WHERE skill_id = ? ORDER BY version DESC LIMIT ? OFFSET ?", &[Value::from(id), Value::from(limit as i64), Value::from(offset as i64)])?;
                Ok((items, total))
            })
            .await
    }

    async fn delete_skill(&self, department: &DepartmentId, skill_id: &str) -> DomainResult<usize> {
        let id = skill_id.to_owned();
        self.pool
            .get(department)?
            .call(move |c| {
                let tx = c.transaction()?;
                tx.execute("DELETE FROM skills_fts WHERE skill_id = ?", [&id])?;
                let n = tx.execute("DELETE FROM skills WHERE skill_id = ?", [&id])?;
                tx.commit()?;
                Ok(n)
            })
            .await
    }

    async fn delete_versions(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        versions_in: &[u32],
    ) -> DomainResult<usize> {
        let (id, vs) = (skill_id.to_owned(), versions_in.to_vec());
        self.pool
            .get(department)?
            .call(move |c| {
                let mut n = 0;
                for v in vs {
                    n += c.execute(
                        "DELETE FROM skills WHERE skill_id = ? AND version = ? AND is_head = 0",
                        params![id, v],
                    )?;
                }
                Ok(n)
            })
            .await
    }

    async fn count_heads(&self, department: &DepartmentId) -> DomainResult<usize> {
        self.pool
            .get(department)?
            .call(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM skills WHERE is_head = 1 AND status = 'active'",
                    [],
                    |r| r.get(0),
                )
            })
            .await
    }
}

fn buffer_key(k: &BufferKey) -> String {
    format!(
        "{}|{}|{}|{}",
        k.user_id, k.team_id, k.agent_id, k.session_id
    )
}

#[async_trait]
impl SkillBufferStore for SqliteSkillStore {
    async fn load(
        &self,
        department: &DepartmentId,
        key: &BufferKey,
    ) -> DomainResult<(Vec<BufferedMessage>, BufferCounters)> {
        let k = buffer_key(key);
        self.pool
            .get(department)?
            .call(move |c| {
                let row: Option<(String, u32, usize)> = c
                    .query_row("SELECT messages, tool_call_count, byte_count FROM skill_buffers WHERE buffer_key = ?", [k], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .optional()?;
                Ok(match row {
                    Some((messages, tool_call_count, byte_count)) => (from_doc(&messages)?, BufferCounters { tool_call_count, byte_count }),
                    None => (Vec::new(), BufferCounters::default()),
                })
            })
            .await
    }

    async fn save(
        &self,
        department: &DepartmentId,
        key: &BufferKey,
        messages: &[BufferedMessage],
        counters: &BufferCounters,
    ) -> DomainResult<()> {
        let (k, m, c) = (buffer_key(key), messages.to_vec(), counters.clone());
        self.pool
            .get(department)?
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO skill_buffers VALUES (?,?,?,?)",
                    params![k, to_doc(&m)?, c.tool_call_count, c.byte_count],
                )
                .map(|_| ())
            })
            .await
    }

    async fn clear(&self, department: &DepartmentId, key: &BufferKey) -> DomainResult<()> {
        let k = buffer_key(key);
        self.pool
            .get(department)?
            .call(move |c| {
                c.execute("DELETE FROM skill_buffers WHERE buffer_key = ?", [k])
                    .map(|_| ())
            })
            .await
    }

    async fn archive(
        &self,
        department: &DepartmentId,
        key: &BufferKey,
        at_ms: i64,
        messages: &[BufferedMessage],
    ) -> DomainResult<String> {
        let archive_key = format!(
            "skill_buffer/{}/{}/{}/{}/data-{at_ms}.jsonl",
            key.user_id, key.team_id, key.agent_id, key.session_id
        );
        let (ak, m) = (archive_key.clone(), messages.to_vec());
        self.pool
            .get(department)?
            .call(move |c| {
                c.execute(
                    "INSERT OR IGNORE INTO skill_archives VALUES (?,?,?)",
                    params![ak, to_doc(&m)?, at_ms],
                )
                .map(|_| ())
            })
            .await?;
        Ok(archive_key)
    }

    async fn read_archive(
        &self,
        department: &DepartmentId,
        archive_key: &str,
    ) -> DomainResult<Option<Vec<BufferedMessage>>> {
        let k = archive_key.to_owned();
        self.pool
            .get(department)?
            .call(move |c| {
                let doc: Option<String> = c
                    .query_row(
                        "SELECT messages FROM skill_archives WHERE archive_key = ?",
                        [k],
                        |r| r.get(0),
                    )
                    .optional()?;
                doc.map(|d| from_doc(&d)).transpose()
            })
            .await
    }

    async fn put_task(&self, department: &DepartmentId, task: &SkillTask) -> DomainResult<()> {
        let t = task.clone();
        self.pool
            .get(department)?
            .call(move |c| {
                c.execute(
                    "INSERT OR REPLACE INTO skill_tasks VALUES (?,?)",
                    params![t.task_id, to_doc(&t)?],
                )
                .map(|_| ())
            })
            .await
    }

    async fn get_task(
        &self,
        department: &DepartmentId,
        task_id: &str,
    ) -> DomainResult<Option<SkillTask>> {
        let id = task_id.to_owned();
        self.pool
            .get(department)?
            .call(move |c| {
                let doc: Option<String> = c
                    .query_row("SELECT doc FROM skill_tasks WHERE task_id = ?", [id], |r| {
                        r.get(0)
                    })
                    .optional()?;
                doc.map(|d| from_doc(&d)).transpose()
            })
            .await
    }
}
