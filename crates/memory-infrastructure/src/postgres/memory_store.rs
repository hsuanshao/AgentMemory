//! Postgres adapter for L0 conversations, L1 atoms and versioned profile
//! files.
//!
//! Sequence numbers come from `BIGSERIAL` columns. A rewrite of an existing
//! id takes a fresh value (`seq = DEFAULT` on conflict), matching SQLite's
//! `INSERT OR REPLACE` + `AUTOINCREMENT`. Writers of one table hold a
//! transaction-scoped advisory lock while they draw sequence values, so
//! rows become visible in sequence order and an `after_seq` cursor can
//! never step over a row that commits later with a smaller number.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memory_application::common::{Page, Scored};
use memory_application::ports::memory::{
    AtomFilter, AtomRepository, AtomSelector, ConversationFilter, ConversationRepository,
    MovedRows, ProfileFileRepository, ScopeMigrationStore, Sequenced,
};
use memory_domain::error::DomainResult;
use memory_domain::memory::{AtomType, AtomicMemory, ConversationMessage, Role};
use memory_domain::profile::{ProfileFile, ProfileScope};
use memory_domain::scope::{InstanceId, MemoryScope};
use pgvector::Vector;
use sqlx::postgres::PgRow;
use tracing::warn;

use super::db::{pg_err, xact_lock, PgDb};
use super::sql::{
    args, count, execute, fetch_all, fetch_optional, get, tsquery_literal, tsvector_literal, Arg,
    Where,
};
use crate::sqlite::sql::{from_ms, like_prefix, to_ms};

pub struct PostgresMemoryStore {
    db: Arc<PgDb>,
}

impl PostgresMemoryStore {
    pub fn new(db: Arc<PgDb>) -> Self {
        Self { db }
    }
}

/// Stored integers are `BIGINT`; domain versions are `u32`.
pub(super) fn to_u32(v: i64) -> u32 {
    u32::try_from(v).unwrap_or_default()
}

fn scope_from_row(row: &PgRow) -> DomainResult<MemoryScope> {
    let team: String = get(row, "team_id")?;
    let agent: String = get(row, "agent_id")?;
    let user: String = get(row, "user_id")?;
    let task: Option<String> = get(row, "task_id")?;
    MemoryScope::from_optional(Some(&team), Some(&agent), Some(&user), task.as_deref())
}

fn scope_where(w: &mut Where, scope: &MemoryScope) {
    w.eq("team_id", scope.team_id.to_string())
        .eq("agent_id", scope.agent_id.to_string())
        .eq("user_id", scope.user_id.to_string());
}

fn profile_where(w: &mut Where, profile: &ProfileScope) {
    w.eq("team_id", profile.team_id.to_string())
        .eq("agent_id", profile.agent_id.to_string());
}

fn time_where(
    w: &mut Where,
    column: &str,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) {
    if let Some(s) = start {
        w.raw(&format!("{column} >= ?"), [Arg::from(to_ms(s))]);
    }
    if let Some(e) = end {
        w.raw(&format!("{column} <= ?"), [Arg::from(to_ms(e))]);
    }
}

// ── L0 ──────────────────────────────────────────────────────────────────

const CONV_COLUMNS: &str = "id, team_id, agent_id, user_id, task_id, session_id, role, content, timestamp_ms, recorded_at_ms, version";

fn conversation_from_row(row: &PgRow) -> DomainResult<ConversationMessage> {
    let role: String = get(row, "role")?;
    Ok(ConversationMessage {
        id: get(row, "id")?,
        scope: scope_from_row(row)?,
        session_id: get(row, "session_id")?,
        role: if role == "assistant" {
            Role::Assistant
        } else {
            Role::User
        },
        content: get(row, "content")?,
        timestamp: from_ms(get(row, "timestamp_ms")?),
        recorded_at: from_ms(get(row, "recorded_at_ms")?),
        version: to_u32(get(row, "version")?),
    })
}

fn conversation_where(scope: Option<&MemoryScope>, filter: &ConversationFilter) -> Where {
    let mut w = Where::default();
    if let Some(s) = scope {
        scope_where(&mut w, s);
    }
    w.eq_opt("session_id", filter.session_id.clone());
    time_where(&mut w, "timestamp_ms", filter.time.start, filter.time.end);
    w
}

/// `SET a = EXCLUDED.a, …` for every column, plus a fresh sequence.
fn upsert_set(columns: &str) -> String {
    let mut set = vec!["seq = DEFAULT".to_owned()];
    set.extend(
        columns
            .split(", ")
            .filter(|c| *c != "id")
            .map(|c| format!("{c} = EXCLUDED.{c}")),
    );
    set.join(", ")
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

#[async_trait]
impl ConversationRepository for PostgresMemoryStore {
    async fn append(
        &self,
        instance: &InstanceId,
        messages: &[ConversationMessage],
    ) -> DomainResult<()> {
        let s = self.db.schema(instance).await?;
        let columns = format!("{CONV_COLUMNS}, tokens");
        let sql = format!(
            "INSERT INTO {s}.conversations ({columns}) VALUES ({}, ?::tsvector) ON CONFLICT (id) DO UPDATE SET {}",
            placeholders(11),
            upsert_set(&columns)
        );
        let mut tx = self.db.begin().await?;
        xact_lock(&mut tx, &format!("{s}.conversations")).await?;
        for m in messages {
            execute(
                &mut tx,
                &sql,
                &args![
                    m.id.as_str(),
                    m.scope.team_id.as_str(),
                    m.scope.agent_id.as_str(),
                    m.scope.user_id.as_str(),
                    m.scope.task_id.as_ref().map(|t| t.to_string()),
                    m.session_id.as_str(),
                    if m.role == Role::User {
                        "user"
                    } else {
                        "assistant"
                    },
                    m.content.as_str(),
                    to_ms(m.timestamp),
                    to_ms(m.recorded_at),
                    m.version,
                    tsvector_literal(&m.content)
                ],
            )
            .await?;
        }
        tx.commit().await.map_err(pg_err)
    }

    async fn query(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        filter: &ConversationFilter,
        page: Page,
    ) -> DomainResult<(Vec<ConversationMessage>, usize)> {
        let s = self.db.schema(instance).await?;
        let w = conversation_where(Some(scope), filter);
        let mut c = self.db.conn().await?;
        let total = count(
            &mut c,
            &format!("SELECT COUNT(*) FROM {s}.conversations {}", w.sql()),
            &w.params,
        )
        .await?;
        let rows = fetch_all(
            &mut c,
            &format!(
                "SELECT {CONV_COLUMNS} FROM {s}.conversations {} ORDER BY timestamp_ms DESC, id LIMIT ? OFFSET ?",
                w.sql()
            ),
            &w.with(args![page.limit, page.offset]),
        )
        .await?;
        let items = rows
            .iter()
            .map(conversation_from_row)
            .collect::<DomainResult<_>>()?;
        Ok((items, total))
    }

    async fn search(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        query: &str,
        filter: &ConversationFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<ConversationMessage>>> {
        let Some(q) = tsquery_literal(query) else {
            return Ok(Vec::new());
        };
        let s = self.db.schema(instance).await?;
        let mut w = conversation_where(Some(scope), filter);
        w.raw("tokens @@ q", []);
        // ts_rank_cd normalisation 32 maps the rank to rank/(rank+1), the
        // same 0..1 shape `bm25_to_score` gives the SQLite results.
        let sql = format!(
            "SELECT {CONV_COLUMNS}, ts_rank_cd(tokens, q, 32)::float8 AS rank FROM {s}.conversations, CAST(? AS tsquery) q {} ORDER BY rank DESC, id LIMIT ?",
            w.sql()
        );
        let mut params = args![q].to_vec();
        params.extend(w.with(args![limit]));
        let rows = fetch_all(&mut *self.db.conn().await?, &sql, &params).await?;
        rows.iter()
            .map(|row| {
                Ok(Scored {
                    item: conversation_from_row(row)?,
                    score: get(row, "rank")?,
                })
            })
            .collect()
    }

    async fn count(
        &self,
        instance: &InstanceId,
        scope: Option<&MemoryScope>,
        filter: &ConversationFilter,
    ) -> DomainResult<usize> {
        let s = self.db.schema(instance).await?;
        let w = conversation_where(scope, filter);
        count(
            &mut *self.db.conn().await?,
            &format!("SELECT COUNT(*) FROM {s}.conversations {}", w.sql()),
            &w.params,
        )
        .await
    }

    async fn delete_by_ids(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        ids: &[String],
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        scope_where(&mut w, scope);
        w.in_list("id", ids);
        self.delete_from(instance, "conversations", w).await
    }

    async fn delete_by_sessions(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        sessions: &[String],
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        scope_where(&mut w, scope);
        w.in_list("session_id", sessions);
        self.delete_from(instance, "conversations", w).await
    }

    async fn delete_profile(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        profile_where(&mut w, profile);
        self.delete_from(instance, "conversations", w).await
    }

    async fn after_seq(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        session_id: &str,
        after: Option<i64>,
        limit: usize,
    ) -> DomainResult<Vec<Sequenced<ConversationMessage>>> {
        let s = self.db.schema(instance).await?;
        let mut w = Where::default();
        scope_where(&mut w, scope);
        w.eq("session_id", session_id)
            .raw("seq > ?", [Arg::from(after.unwrap_or(0))]);
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT seq, {CONV_COLUMNS} FROM {s}.conversations {} ORDER BY seq LIMIT ?",
                w.sql()
            ),
            &w.with(args![limit]),
        )
        .await?;
        rows.iter()
            .map(|row| {
                Ok(Sequenced {
                    seq: get(row, "seq")?,
                    item: conversation_from_row(row)?,
                })
            })
            .collect()
    }
}

impl PostgresMemoryStore {
    /// Deletes matching rows; their tokens go with them (same row).
    async fn delete_from(
        &self,
        instance: &InstanceId,
        table: &str,
        w: Where,
    ) -> DomainResult<usize> {
        let s = self.db.schema(instance).await?;
        let n = execute(
            &mut *self.db.conn().await?,
            &format!("DELETE FROM {s}.{table} {}", w.sql()),
            &w.params,
        )
        .await?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// The embedding to store: only when vectors are on and it fits the
    /// column (a mismatched provider must not fail the whole write).
    fn storable_embedding(&self, id: &str, embedding: Option<&[f32]>) -> Option<Vector> {
        let (dims, e) = (self.db.dims()?, embedding?);
        if e.len() != dims {
            warn!(
                atom = id,
                got = e.len(),
                expected = dims,
                "embedding dimension mismatch; stored without vector"
            );
            return None;
        }
        Some(Vector::from(e.to_vec()))
    }
}

// ── L1 ──────────────────────────────────────────────────────────────────

const ATOM_COLUMNS: &str = "id, team_id, agent_id, user_id, task_id, session_id, atom_type, content, background, priority, source_ids, version, created_at_ms, updated_at_ms";

fn atom_from_row(row: &PgRow) -> DomainResult<AtomicMemory> {
    let kind: String = get(row, "atom_type")?;
    let sources: String = get(row, "source_ids")?;
    let priority: i64 = get(row, "priority")?;
    Ok(AtomicMemory {
        id: get(row, "id")?,
        scope: scope_from_row(row)?,
        atom_type: AtomType::parse(&kind)?,
        content: get(row, "content")?,
        background: get(row, "background")?,
        priority: i32::try_from(priority).unwrap_or_default(),
        source_message_ids: serde_json::from_str(&sources).unwrap_or_default(),
        session_id: get(row, "session_id")?,
        version: to_u32(get(row, "version")?),
        created_at: from_ms(get(row, "created_at_ms")?),
        updated_at: from_ms(get(row, "updated_at_ms")?),
    })
}

fn atom_where(selector: Option<AtomSelector<'_>>, filter: &AtomFilter) -> Where {
    let mut w = Where::default();
    match selector {
        Some(AtomSelector::Scope(s)) => scope_where(&mut w, s),
        Some(AtomSelector::Profile(p)) => profile_where(&mut w, p),
        None => {}
    }
    w.eq_opt("atom_type", filter.atom_type.map(|t| t.as_str().to_owned()));
    time_where(&mut w, "updated_at_ms", filter.time.start, filter.time.end);
    w
}

fn atoms_from_rows(rows: &[PgRow]) -> DomainResult<Vec<AtomicMemory>> {
    rows.iter().map(atom_from_row).collect()
}

#[async_trait]
impl AtomRepository for PostgresMemoryStore {
    async fn upsert(
        &self,
        instance: &InstanceId,
        atom: &AtomicMemory,
        embedding: Option<&[f32]>,
    ) -> DomainResult<()> {
        let s = self.db.schema(instance).await?;
        let with_vector = self.db.dims().is_some();
        let columns = if with_vector {
            format!("{ATOM_COLUMNS}, tokens, embedding")
        } else {
            format!("{ATOM_COLUMNS}, tokens")
        };
        let values = if with_vector {
            format!("{}, ?::tsvector, ?", placeholders(14))
        } else {
            format!("{}, ?::tsvector", placeholders(14))
        };
        let text = format!(
            "{} {}",
            atom.content,
            atom.background.as_deref().unwrap_or("")
        );
        let mut params = args![
            atom.id.as_str(),
            atom.scope.team_id.as_str(),
            atom.scope.agent_id.as_str(),
            atom.scope.user_id.as_str(),
            atom.scope.task_id.as_ref().map(|t| t.to_string()),
            atom.session_id.clone(),
            atom.atom_type.as_str(),
            atom.content.as_str(),
            atom.background.clone(),
            i64::from(atom.priority),
            serde_json::to_string(&atom.source_message_ids).unwrap_or_else(|_| "[]".into()),
            atom.version,
            to_ms(atom.created_at),
            to_ms(atom.updated_at),
            tsvector_literal(&text)
        ]
        .to_vec();
        if with_vector {
            params.push(Arg::Vector(self.storable_embedding(&atom.id, embedding)));
        }
        let mut tx = self.db.begin().await?;
        xact_lock(&mut tx, &format!("{s}.atoms")).await?;
        execute(
            &mut tx,
            &format!(
                "INSERT INTO {s}.atoms ({columns}) VALUES ({values}) ON CONFLICT (id) DO UPDATE SET {}",
                upsert_set(&columns)
            ),
            &params,
        )
        .await?;
        tx.commit().await.map_err(pg_err)
    }

    async fn get(&self, instance: &InstanceId, id: &str) -> DomainResult<Option<AtomicMemory>> {
        let s = self.db.schema(instance).await?;
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT {ATOM_COLUMNS} FROM {s}.atoms WHERE id = ?"),
            &args![id],
        )
        .await?
        .as_ref()
        .map(atom_from_row)
        .transpose()
    }

    async fn query(
        &self,
        instance: &InstanceId,
        selector: AtomSelector<'_>,
        filter: &AtomFilter,
        page: Page,
    ) -> DomainResult<(Vec<AtomicMemory>, usize)> {
        let s = self.db.schema(instance).await?;
        let w = atom_where(Some(selector), filter);
        let mut c = self.db.conn().await?;
        let total = count(
            &mut c,
            &format!("SELECT COUNT(*) FROM {s}.atoms {}", w.sql()),
            &w.params,
        )
        .await?;
        let rows = fetch_all(
            &mut c,
            &format!(
                "SELECT {ATOM_COLUMNS} FROM {s}.atoms {} ORDER BY updated_at_ms DESC, id LIMIT ? OFFSET ?",
                w.sql()
            ),
            &w.with(args![page.limit, page.offset]),
        )
        .await?;
        Ok((atoms_from_rows(&rows)?, total))
    }

    async fn after_seq(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        after: Option<i64>,
        limit: usize,
    ) -> DomainResult<Vec<Sequenced<AtomicMemory>>> {
        let s = self.db.schema(instance).await?;
        let mut w = Where::default();
        profile_where(&mut w, profile);
        w.raw("seq > ?", [Arg::from(after.unwrap_or(0))]);
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT seq, {ATOM_COLUMNS} FROM {s}.atoms {} ORDER BY seq LIMIT ?",
                w.sql()
            ),
            &w.with(args![limit]),
        )
        .await?;
        rows.iter()
            .map(|row| {
                Ok(Sequenced {
                    seq: get(row, "seq")?,
                    item: atom_from_row(row)?,
                })
            })
            .collect()
    }

    async fn keyword_search(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        query: &str,
        filter: &AtomFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<AtomicMemory>>> {
        let Some(q) = tsquery_literal(query) else {
            return Ok(Vec::new());
        };
        let s = self.db.schema(instance).await?;
        let mut w = atom_where(Some(AtomSelector::Scope(scope)), filter);
        w.raw("tokens @@ q", []);
        let sql = format!(
            "SELECT {ATOM_COLUMNS}, ts_rank_cd(tokens, q, 32)::float8 AS rank FROM {s}.atoms, CAST(? AS tsquery) q {} ORDER BY rank DESC, id LIMIT ?",
            w.sql()
        );
        let mut params = args![q].to_vec();
        params.extend(w.with(args![limit]));
        let rows = fetch_all(&mut *self.db.conn().await?, &sql, &params).await?;
        rows.iter()
            .map(|row| {
                Ok(Scored {
                    item: atom_from_row(row)?,
                    score: get(row, "rank")?,
                })
            })
            .collect()
    }

    /// Nearest neighbours by cosine distance through the HNSW index; the
    /// isolation filters apply inside the scan (pgvector ≥ 0.8 iterates the
    /// index until `limit` rows pass them).
    async fn vector_search(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        embedding: &[f32],
        filter: &AtomFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<AtomicMemory>>> {
        let Some(dims) = self.db.dims() else {
            return Ok(Vec::new());
        };
        if embedding.len() != dims || limit == 0 {
            return Ok(Vec::new());
        }
        let s = self.db.schema(instance).await?;
        let mut w = atom_where(Some(AtomSelector::Scope(scope)), filter);
        w.raw("embedding IS NOT NULL", []);
        let query = Vector::from(embedding.to_vec());
        let sql = format!(
            "SELECT {ATOM_COLUMNS}, (1 - (embedding <=> ?))::float8 AS score FROM {s}.atoms {} ORDER BY embedding <=> ? LIMIT ?",
            w.sql()
        );
        let mut params = vec![Arg::Vector(Some(query.clone()))];
        params.extend(w.with([Arg::Vector(Some(query)), Arg::from(limit)]));

        let mut tx = self.db.begin().await?;
        // SET cannot take parameters; both values are integers/keywords.
        let ef_search = limit.clamp(40, 1000);
        execute(
            &mut tx,
            &format!("SET LOCAL hnsw.ef_search = {ef_search}"),
            &[],
        )
        .await?;
        if self.db.iterative_scan() {
            execute(&mut tx, "SET LOCAL hnsw.iterative_scan = strict_order", &[]).await?;
        }
        let rows = fetch_all(&mut tx, &sql, &params).await?;
        tx.commit().await.map_err(pg_err)?;
        rows.iter()
            .map(|row| {
                Ok(Scored {
                    item: atom_from_row(row)?,
                    score: get(row, "score")?,
                })
            })
            .collect()
    }

    async fn count(
        &self,
        instance: &InstanceId,
        scope: Option<&MemoryScope>,
        filter: &AtomFilter,
    ) -> DomainResult<usize> {
        let s = self.db.schema(instance).await?;
        let w = atom_where(scope.map(AtomSelector::Scope), filter);
        count(
            &mut *self.db.conn().await?,
            &format!("SELECT COUNT(*) FROM {s}.atoms {}", w.sql()),
            &w.params,
        )
        .await
    }

    async fn delete(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        ids: &[String],
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        scope_where(&mut w, scope);
        w.in_list("id", ids);
        self.delete_from(instance, "atoms", w).await
    }

    async fn delete_profile(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        profile_where(&mut w, profile);
        self.delete_from(instance, "atoms", w).await
    }
}

// ── L2 / L3 ─────────────────────────────────────────────────────────────

const FILE_COLUMNS: &str = "path, content, version, created_at_ms, updated_at_ms";

fn file_from_row(row: &PgRow) -> DomainResult<ProfileFile> {
    Ok(ProfileFile {
        path: get(row, "path")?,
        content: get(row, "content")?,
        version: to_u32(get(row, "version")?),
        created_at: from_ms(get(row, "created_at_ms")?),
        updated_at: from_ms(get(row, "updated_at_ms")?),
    })
}

fn profile_ids(p: &ProfileScope) -> (String, String) {
    (p.team_id.to_string(), p.agent_id.to_string())
}

/// SQLite's `LIKE` ignores ASCII case; `ILIKE` keeps that behaviour.
const PREFIX_MATCH: &str = "path ILIKE ? ESCAPE '\\'";

#[async_trait]
impl ProfileFileRepository for PostgresMemoryStore {
    async fn list(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        prefix: &str,
    ) -> DomainResult<Vec<ProfileFile>> {
        let s = self.db.schema(instance).await?;
        let (team, agent) = profile_ids(profile);
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT {FILE_COLUMNS} FROM {s}.profile_files WHERE team_id = ? AND agent_id = ? AND is_head AND {PREFIX_MATCH} ORDER BY path"
            ),
            &args![team, agent, like_prefix(prefix)],
        )
        .await?;
        rows.iter().map(file_from_row).collect()
    }

    async fn read(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        path: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<ProfileFile>> {
        let s = self.db.schema(instance).await?;
        let (team, agent) = profile_ids(profile);
        let (cond, v) = match version {
            Some(v) => ("version = ?", vec![Arg::from(v)]),
            None => ("is_head", vec![]),
        };
        let mut params = args![team, agent, path].to_vec();
        params.extend(v);
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT {FILE_COLUMNS} FROM {s}.profile_files WHERE team_id = ? AND agent_id = ? AND path = ? AND {cond}"),
            &params,
        )
        .await?
        .as_ref()
        .map(file_from_row)
        .transpose()
    }

    async fn write(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        path: &str,
        content: &str,
    ) -> DomainResult<ProfileFile> {
        let s = self.db.schema(instance).await?;
        let (team, agent) = profile_ids(profile);
        let now = to_ms(Utc::now());
        let mut tx = self.db.begin().await?;
        // Concurrent writers of one file would both derive `head + 1`.
        xact_lock(&mut tx, &format!("{s}.profile_files:{team}/{agent}/{path}")).await?;
        let head = fetch_optional(
            &mut tx,
            &format!("SELECT version, content, created_at_ms FROM {s}.profile_files WHERE team_id = ? AND agent_id = ? AND path = ? AND is_head"),
            &args![team.as_str(), agent.as_str(), path],
        )
        .await?
        .map(|r| -> DomainResult<(u32, String, i64)> {
            Ok((to_u32(get(&r, "version")?), get(&r, "content")?, get(&r, "created_at_ms")?))
        })
        .transpose()?;
        let file = match head {
            Some((version, old, created)) if old == content => ProfileFile {
                path: path.to_owned(),
                content: content.to_owned(),
                version,
                created_at: from_ms(created),
                updated_at: from_ms(now),
            },
            other => {
                let (version, created) = other.map_or((1, now), |(v, _, c)| (v + 1, c));
                execute(
                    &mut tx,
                    &format!("UPDATE {s}.profile_files SET is_head = FALSE WHERE team_id = ? AND agent_id = ? AND path = ?"),
                    &args![team.as_str(), agent.as_str(), path],
                )
                .await?;
                execute(
                    &mut tx,
                    &format!("INSERT INTO {s}.profile_files (team_id, agent_id, path, version, content, is_head, created_at_ms, updated_at_ms) VALUES (?,?,?,?,?,TRUE,?,?)"),
                    &args![team.as_str(), agent.as_str(), path, version, content, created, now],
                )
                .await?;
                ProfileFile {
                    path: path.to_owned(),
                    content: content.to_owned(),
                    version,
                    created_at: from_ms(created),
                    updated_at: from_ms(now),
                }
            }
        };
        tx.commit().await.map_err(pg_err)?;
        Ok(file)
    }

    async fn remove(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        path: &str,
    ) -> DomainResult<usize> {
        let s = self.db.schema(instance).await?;
        let (team, agent) = profile_ids(profile);
        let (cond, value) = if path.ends_with('/') {
            (PREFIX_MATCH, like_prefix(path))
        } else {
            ("path = ?", path.to_owned())
        };
        let params = args![team, agent, value];
        let mut tx = self.db.begin().await?;
        let heads = count(
            &mut tx,
            &format!("SELECT COUNT(*) FROM {s}.profile_files WHERE team_id = ? AND agent_id = ? AND is_head AND {cond}"),
            &params,
        )
        .await?;
        execute(
            &mut tx,
            &format!("DELETE FROM {s}.profile_files WHERE team_id = ? AND agent_id = ? AND {cond}"),
            &params,
        )
        .await?;
        tx.commit().await.map_err(pg_err)?;
        Ok(heads)
    }

    async fn count(
        &self,
        instance: &InstanceId,
        profile: Option<&ProfileScope>,
        prefix: &str,
    ) -> DomainResult<usize> {
        let s = self.db.schema(instance).await?;
        let mut w = Where::default();
        if let Some(p) = profile {
            profile_where(&mut w, p);
        }
        w.raw("is_head", [])
            .raw(PREFIX_MATCH, [Arg::from(like_prefix(prefix))]);
        count(
            &mut *self.db.conn().await?,
            &format!("SELECT COUNT(*) FROM {s}.profile_files {}", w.sql()),
            &w.params,
        )
        .await
    }
}

#[async_trait]
impl ScopeMigrationStore for PostgresMemoryStore {
    async fn reassign_default(
        &self,
        instance: &InstanceId,
        from: &MemoryScope,
        to: &MemoryScope,
        dry_run: bool,
    ) -> DomainResult<MovedRows> {
        let s = self.db.schema(instance).await?;
        let (team, agent, user) = (
            to.team_id.as_str(),
            to.agent_id.as_str(),
            to.user_id.as_str(),
        );
        let (ft, fa, fu) = (
            from.team_id.as_str(),
            from.agent_id.as_str(),
            from.user_id.as_str(),
        );
        let mut tx = self.db.begin().await?;
        let mut run = async |sql: String, binds: &[&str]| -> DomainResult<usize> {
            let mut q = sqlx::query(&sql);
            for b in binds {
                q = q.bind(*b);
            }
            Ok(q.execute(&mut *tx).await.map_err(pg_err)?.rows_affected() as usize)
        };
        let triple = "team_id = $4 AND agent_id = $5 AND user_id = $6";
        let conversations = run(
            format!("UPDATE {s}.conversations SET team_id = $1, agent_id = $2, user_id = $3 WHERE {triple}"),
            &[team, agent, user, ft, fa, fu],
        )
        .await?;
        let atoms = run(
            format!(
                "UPDATE {s}.atoms SET team_id = $1, agent_id = $2, user_id = $3 WHERE {triple}"
            ),
            &[team, agent, user, ft, fa, fu],
        )
        .await?;
        let profile_files = run(
            format!("UPDATE {s}.profile_files SET team_id = $1, agent_id = $2 WHERE team_id = $3 AND agent_id = $4"),
            &[team, agent, ft, fa],
        )
        .await?;
        let skill_versions = run(
            format!(
                "UPDATE {s}.skills SET team_id = $1, owner_agent_id = $2, \
                 owner_user_id = CASE WHEN owner_user_id = $6 THEN $3 ELSE owner_user_id END, \
                 doc = jsonb_set(jsonb_set(jsonb_set(doc, '{{team_id}}', to_jsonb($1::text)), \
                       '{{owner_agent_id}}', to_jsonb($2::text)), '{{owner_user_id}}', \
                       to_jsonb(CASE WHEN owner_user_id = $6 THEN $3 ELSE owner_user_id END)) \
                 WHERE team_id = $4 AND owner_agent_id = $5"
            ),
            &[team, agent, user, ft, fa, fu],
        )
        .await?;
        drop(run);
        if dry_run {
            tx.rollback().await.map_err(pg_err)?;
        } else {
            tx.commit().await.map_err(pg_err)?;
        }
        Ok(MovedRows {
            conversations,
            atoms,
            profile_files,
            skill_versions,
        })
    }
}
