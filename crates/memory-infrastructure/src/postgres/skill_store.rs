//! Postgres adapter for skill versions (keyword tokens on the head row
//! only), conversation buffers, archives and extraction tasks.

use std::sync::Arc;

use async_trait::async_trait;
use memory_application::ports::skill::{
    BufferCounters, BufferKey, BufferedMessage, SkillBufferStore, SkillFilter, SkillRepository,
    SkillTask,
};
use memory_domain::error::DomainResult;
use memory_domain::scope::DepartmentId;
use memory_domain::skill::{SkillStatus, SkillVersion};

use super::db::{pg_err, PgDb};
use super::sql::{
    args, count, doc, docs, execute, fetch_all, fetch_optional, get, to_doc, tsquery_literal,
    tsvector_literal, Arg, Where,
};
use crate::sqlite::sql::like_prefix;

pub struct PostgresSkillStore {
    db: Arc<PgDb>,
}

impl PostgresSkillStore {
    pub fn new(db: Arc<PgDb>) -> Self {
        Self { db }
    }
}

fn status_str(s: SkillStatus) -> &'static str {
    match s {
        SkillStatus::Active => "active",
        SkillStatus::Archived => "archived",
    }
}

fn head_where(f: &SkillFilter) -> Where {
    let mut w = Where::default();
    w.raw("is_head", [])
        .eq_opt("team_id", f.team_id.clone())
        .eq_opt("owner_agent_id", f.owner_agent_id.clone())
        .eq_opt("owner_user_id", f.owner_user_id.clone());
    if let Some(p) = &f.name_prefix {
        // SQLite's `LIKE` ignores ASCII case; `ILIKE` keeps that.
        w.raw("name ILIKE ? ESCAPE '\\'", [Arg::from(like_prefix(p))]);
    }
    let statuses: Vec<String> = f
        .statuses
        .iter()
        .map(|s| status_str(*s).to_owned())
        .collect();
    w.in_list("status", &statuses);
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

/// The text indexed for a head version (same as the SQLite FTS row).
pub(crate) fn index_source(v: &SkillVersion) -> String {
    let body: String = v.content.chars().take(4000).collect();
    format!("{} {} {}", v.name.replace('-', " "), v.description, body)
}

#[async_trait]
impl SkillRepository for PostgresSkillStore {
    async fn skill_id_exists(
        &self,
        department: &DepartmentId,
        skill_id: &str,
    ) -> DomainResult<bool> {
        let s = self.db.schema(department).await?;
        let row = fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT 1::bigint AS one FROM {s}.skills WHERE skill_id = ? LIMIT 1"),
            &args![skill_id],
        )
        .await?;
        Ok(row.is_some())
    }

    async fn head(
        &self,
        department: &DepartmentId,
        skill_id: &str,
    ) -> DomainResult<Option<SkillVersion>> {
        let s = self.db.schema(department).await?;
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.skills WHERE skill_id = ? AND is_head"),
            &args![skill_id],
        )
        .await?
        .map(|r| doc(&r, "doc"))
        .transpose()
    }

    async fn version(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
    ) -> DomainResult<Option<SkillVersion>> {
        let s = self.db.schema(department).await?;
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.skills WHERE skill_id = ? AND version = ?"),
            &args![skill_id, version],
        )
        .await?
        .map(|r| doc(&r, "doc"))
        .transpose()
    }

    async fn find_head_by_name(
        &self,
        department: &DepartmentId,
        team_id: &str,
        agent_id: &str,
        name: &str,
    ) -> DomainResult<Option<SkillVersion>> {
        let s = self.db.schema(department).await?;
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.skills WHERE is_head AND status = 'active' AND team_id = ? AND owner_agent_id = ? AND name = ? LIMIT 1"),
            &args![team_id, agent_id, name],
        )
        .await?
        .map(|r| doc(&r, "doc"))
        .transpose()
    }

    async fn append_version(
        &self,
        department: &DepartmentId,
        v: &SkillVersion,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        execute(
            &mut tx,
            &format!("UPDATE {s}.skills SET is_head = FALSE, tokens = NULL, doc = jsonb_set(doc, '{{is_head}}', 'false'::jsonb) WHERE skill_id = ? AND is_head"),
            &args![v.skill_id.as_str()],
        )
        .await?;
        execute(
            &mut tx,
            &format!(
                "INSERT INTO {s}.skills (skill_id, version, is_head, name, status, team_id, owner_agent_id, owner_user_id, updated_at_ms, created_at_ms, doc, tokens) \
                 VALUES (?,?,TRUE,?,?,?,?,?,?,?,?,?::tsvector)"
            ),
            &[
                Arg::from(&v.skill_id),
                Arg::from(v.version),
                Arg::from(&v.name),
                Arg::from(status_str(v.status)),
                Arg::from(&v.team_id),
                Arg::from(&v.owner_agent_id),
                Arg::from(&v.owner_user_id),
                Arg::from(v.updated_at_ms),
                Arg::from(v.created_at_ms),
                to_doc(v)?,
                Arg::from(tsvector_literal(&index_source(v))),
            ],
        )
        .await?;
        tx.commit().await.map_err(pg_err)
    }

    async fn list_heads(
        &self,
        department: &DepartmentId,
        filter: &SkillFilter,
        limit: usize,
        offset: usize,
    ) -> DomainResult<(Vec<SkillVersion>, usize)> {
        let s = self.db.schema(department).await?;
        let w = head_where(filter);
        let mut c = self.db.conn().await?;
        let total = count(
            &mut c,
            &format!("SELECT COUNT(*) FROM {s}.skills {}", w.sql()),
            &w.params,
        )
        .await?;
        let rows = fetch_all(
            &mut c,
            &format!(
                "SELECT doc FROM {s}.skills {} ORDER BY updated_at_ms DESC, skill_id LIMIT ? OFFSET ?",
                w.sql()
            ),
            &w.with(args![limit, offset]),
        )
        .await?;
        Ok((docs(&rows)?, total))
    }

    async fn search_heads(
        &self,
        department: &DepartmentId,
        filter: &SkillFilter,
        query: &str,
        limit: usize,
    ) -> DomainResult<Vec<(SkillVersion, f64, String)>> {
        let Some(q) = tsquery_literal(query) else {
            return Ok(Vec::new());
        };
        let s = self.db.schema(department).await?;
        let mut w = head_where(filter);
        w.raw("tokens @@ q", []);
        let sql = format!(
            "SELECT doc, ts_rank_cd(tokens, q, 32)::float8 AS rank FROM {s}.skills, CAST(? AS tsquery) q {} ORDER BY rank DESC, skill_id LIMIT ?",
            w.sql()
        );
        let mut params = args![q].to_vec();
        params.extend(w.with(args![limit]));
        let rows = fetch_all(&mut *self.db.conn().await?, &sql, &params).await?;
        rows.iter()
            .map(|r| {
                let v: SkillVersion = doc(r, "doc")?;
                let snip = snippet(&v);
                Ok((v, get(r, "rank")?, snip))
            })
            .collect()
    }

    async fn list_versions(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        limit: usize,
        offset: usize,
    ) -> DomainResult<(Vec<SkillVersion>, usize)> {
        let s = self.db.schema(department).await?;
        let mut c = self.db.conn().await?;
        let total = count(
            &mut c,
            &format!("SELECT COUNT(*) FROM {s}.skills WHERE skill_id = ?"),
            &args![skill_id],
        )
        .await?;
        let rows = fetch_all(
            &mut c,
            &format!("SELECT doc FROM {s}.skills WHERE skill_id = ? ORDER BY version DESC LIMIT ? OFFSET ?"),
            &args![skill_id, limit, offset],
        )
        .await?;
        Ok((docs(&rows)?, total))
    }

    async fn delete_skill(&self, department: &DepartmentId, skill_id: &str) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        let n = execute(
            &mut *self.db.conn().await?,
            &format!("DELETE FROM {s}.skills WHERE skill_id = ?"),
            &args![skill_id],
        )
        .await?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    async fn delete_versions(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        versions: &[u32],
    ) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        let wanted: Vec<i64> = versions.iter().map(|v| i64::from(*v)).collect();
        let mut c = self.db.conn().await?;
        let mut n = 0;
        for v in wanted {
            n += execute(
                &mut c,
                &format!(
                    "DELETE FROM {s}.skills WHERE skill_id = ? AND version = ? AND NOT is_head"
                ),
                &args![skill_id, v],
            )
            .await?;
        }
        Ok(usize::try_from(n).unwrap_or(0))
    }

    async fn count_heads(&self, department: &DepartmentId) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        count(
            &mut *self.db.conn().await?,
            &format!("SELECT COUNT(*) FROM {s}.skills WHERE is_head AND status = 'active'"),
            &[],
        )
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
impl SkillBufferStore for PostgresSkillStore {
    async fn load(
        &self,
        department: &DepartmentId,
        key: &BufferKey,
    ) -> DomainResult<(Vec<BufferedMessage>, BufferCounters)> {
        let s = self.db.schema(department).await?;
        let row = fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT messages, tool_call_count, byte_count FROM {s}.skill_buffers WHERE buffer_key = ?"),
            &args![buffer_key(key)],
        )
        .await?;
        let Some(row) = row else {
            return Ok((Vec::new(), BufferCounters::default()));
        };
        let tool_calls: i64 = get(&row, "tool_call_count")?;
        let bytes: i64 = get(&row, "byte_count")?;
        Ok((
            doc(&row, "messages")?,
            BufferCounters {
                tool_call_count: u32::try_from(tool_calls).unwrap_or_default(),
                byte_count: usize::try_from(bytes).unwrap_or_default(),
            },
        ))
    }

    async fn save(
        &self,
        department: &DepartmentId,
        key: &BufferKey,
        messages: &[BufferedMessage],
        counters: &BufferCounters,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!(
                "INSERT INTO {s}.skill_buffers VALUES (?,?,?,?) ON CONFLICT (buffer_key) DO UPDATE SET \
                 messages = EXCLUDED.messages, tool_call_count = EXCLUDED.tool_call_count, byte_count = EXCLUDED.byte_count"
            ),
            &[
                Arg::from(buffer_key(key)),
                to_doc(&messages)?,
                Arg::from(counters.tool_call_count),
                Arg::from(counters.byte_count),
            ],
        )
        .await
        .map(|_| ())
    }

    async fn clear(&self, department: &DepartmentId, key: &BufferKey) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("DELETE FROM {s}.skill_buffers WHERE buffer_key = ?"),
            &args![buffer_key(key)],
        )
        .await
        .map(|_| ())
    }

    async fn archive(
        &self,
        department: &DepartmentId,
        key: &BufferKey,
        at_ms: i64,
        messages: &[BufferedMessage],
    ) -> DomainResult<String> {
        let s = self.db.schema(department).await?;
        let archive_key = format!(
            "skill_buffer/{}/{}/{}/{}/data-{at_ms}.jsonl",
            key.user_id, key.team_id, key.agent_id, key.session_id
        );
        execute(
            &mut *self.db.conn().await?,
            &format!("INSERT INTO {s}.skill_archives VALUES (?,?,?) ON CONFLICT DO NOTHING"),
            &[
                Arg::from(&archive_key),
                to_doc(&messages)?,
                Arg::from(at_ms),
            ],
        )
        .await?;
        Ok(archive_key)
    }

    async fn read_archive(
        &self,
        department: &DepartmentId,
        archive_key: &str,
    ) -> DomainResult<Option<Vec<BufferedMessage>>> {
        let s = self.db.schema(department).await?;
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT messages FROM {s}.skill_archives WHERE archive_key = ?"),
            &args![archive_key],
        )
        .await?
        .map(|r| doc(&r, "messages"))
        .transpose()
    }

    async fn put_task(&self, department: &DepartmentId, task: &SkillTask) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("INSERT INTO {s}.skill_tasks VALUES (?,?) ON CONFLICT (task_id) DO UPDATE SET doc = EXCLUDED.doc"),
            &[Arg::from(&task.task_id), to_doc(task)?],
        )
        .await
        .map(|_| ())
    }

    async fn get_task(
        &self,
        department: &DepartmentId,
        task_id: &str,
    ) -> DomainResult<Option<SkillTask>> {
        let s = self.db.schema(department).await?;
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.skill_tasks WHERE task_id = ?"),
            &args![task_id],
        )
        .await?
        .map(|r| doc(&r, "doc"))
        .transpose()
    }
}
