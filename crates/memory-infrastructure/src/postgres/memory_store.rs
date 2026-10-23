//! Postgres adapter for L0 conversations, L1 atoms and the scene / persona
//! versions of L2 / L3.
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
    MovedRows, ProfileVersionStore, ScopeMigrationStore, Sequenced,
};
use memory_domain::error::DomainResult;
use memory_domain::memory::{AtomType, AtomicMemory, ConversationMessage, Role};
use memory_domain::profile::{
    NewPersonaVersion, NewSceneVersion, PersonaOp, PersonaVersion, ProfileScope, SceneOp,
    SceneVersion,
};
use memory_domain::scope::{DepartmentId, MemoryScope};
use pgvector::Vector;
use sqlx::postgres::PgRow;
use sqlx::PgConnection;
use tracing::warn;

use super::db::{pg_err, xact_lock, PgDb, Schema};
use super::sql::{
    args, count, doc, execute, fetch_all, fetch_optional, get, to_doc, tsquery_literal,
    tsvector_literal, Arg, Where,
};
use crate::profile_versions::{scene_changed, slug_taken, unknown_scene};
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
        department: &DepartmentId,
        messages: &[ConversationMessage],
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
        scope: &MemoryScope,
        filter: &ConversationFilter,
        page: Page,
    ) -> DomainResult<(Vec<ConversationMessage>, usize)> {
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        filter: &ConversationFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<ConversationMessage>>> {
        let Some(q) = tsquery_literal(query) else {
            return Ok(Vec::new());
        };
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
        scope: Option<&MemoryScope>,
        filter: &ConversationFilter,
    ) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
        scope: &MemoryScope,
        ids: &[String],
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        scope_where(&mut w, scope);
        w.in_list("id", ids);
        self.delete_from(department, "conversations", w).await
    }

    async fn delete_by_sessions(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        sessions: &[String],
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        scope_where(&mut w, scope);
        w.in_list("session_id", sessions);
        self.delete_from(department, "conversations", w).await
    }

    async fn delete_profile(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        profile_where(&mut w, profile);
        self.delete_from(department, "conversations", w).await
    }

    async fn after_seq(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        session_id: &str,
        after: Option<i64>,
        limit: usize,
    ) -> DomainResult<Vec<Sequenced<ConversationMessage>>> {
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
        table: &str,
        w: Where,
    ) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
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
    w.eq_opt("session_id", filter.session_id.clone());
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
        department: &DepartmentId,
        atom: &AtomicMemory,
        embedding: Option<&[f32]>,
    ) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
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

    async fn get(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<AtomicMemory>> {
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
        selector: AtomSelector<'_>,
        filter: &AtomFilter,
        page: Page,
    ) -> DomainResult<(Vec<AtomicMemory>, usize)> {
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
        profile: &ProfileScope,
        after: Option<i64>,
        limit: usize,
    ) -> DomainResult<Vec<Sequenced<AtomicMemory>>> {
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        filter: &AtomFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<AtomicMemory>>> {
        let Some(q) = tsquery_literal(query) else {
            return Ok(Vec::new());
        };
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
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
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
        scope: Option<&MemoryScope>,
        filter: &AtomFilter,
    ) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
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
        department: &DepartmentId,
        scope: &MemoryScope,
        ids: &[String],
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        scope_where(&mut w, scope);
        w.in_list("id", ids);
        self.delete_from(department, "atoms", w).await
    }

    async fn delete_profile(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        profile_where(&mut w, profile);
        self.delete_from(department, "atoms", w).await
    }

    async fn departments(&self) -> DomainResult<Vec<DepartmentId>> {
        Ok(self
            .db
            .known_departments()
            .await?
            .iter()
            .filter_map(|d| DepartmentId::parse("department", d).ok())
            .collect())
    }

    async fn missing_embeddings(
        &self,
        department: &DepartmentId,
        limit: usize,
    ) -> DomainResult<Vec<AtomicMemory>> {
        if self.db.dims().is_none() {
            return Ok(Vec::new());
        }
        let s = self.db.schema(department).await?;
        atoms_from_rows(
            &fetch_all(
                &mut *self.db.conn().await?,
                &format!("SELECT {ATOM_COLUMNS} FROM {s}.atoms WHERE embedding IS NULL ORDER BY seq LIMIT ?"),
                &args![limit as i64],
            )
            .await?,
        )
    }

    async fn set_embedding(
        &self,
        department: &DepartmentId,
        id: &str,
        updated_at: DateTime<Utc>,
        embedding: &[f32],
    ) -> DomainResult<bool> {
        let Some(vector) = self.storable_embedding(id, Some(embedding)) else {
            return Ok(false);
        };
        let s = self.db.schema(department).await?;
        let set = execute(
            &mut *self.db.conn().await?,
            &format!(
                "UPDATE {s}.atoms SET embedding = ? WHERE id = ? AND embedding IS NULL AND updated_at_ms = ?"
            ),
            &[Arg::Vector(Some(vector)), Arg::from(id), Arg::from(to_ms(updated_at))],
        )
        .await?;
        Ok(set == 1)
    }
}

// ── L2 / L3 ─────────────────────────────────────────────────────────────

const SCENE_COLUMNS: &str = "v.scene_id, v.version, v.op, v.slug, v.summary, v.heat, v.body, v.attrs, v.parents, v.author, v.created_at_ms, s.created_at_ms AS scene_created_at_ms";

/// The latest version of each scene: `v` is a version, `s` its scene.
const IS_LATEST: &str =
    "v.version = (SELECT MAX(m.version) FROM {s}.scene_versions m WHERE m.scene_id = v.scene_id)";

fn is_latest(s: &Schema) -> String {
    let s = s.to_string();
    IS_LATEST.replace("{s}", &s)
}

fn scene_from_row(row: &PgRow) -> DomainResult<SceneVersion> {
    Ok(SceneVersion {
        scene_id: get(row, "scene_id")?,
        version: to_u32(get(row, "version")?),
        op: SceneOp::parse(&get::<String>(row, "op")?)?,
        slug: get(row, "slug")?,
        summary: get(row, "summary")?,
        heat: to_u32(get(row, "heat")?),
        body: get(row, "body")?,
        attrs: doc(row, "attrs")?,
        parents: doc(row, "parents")?,
        author: get(row, "author")?,
        scene_created_at: from_ms(get(row, "scene_created_at_ms")?),
        created_at: from_ms(get(row, "created_at_ms")?),
    })
}

fn persona_from_row(row: &PgRow) -> DomainResult<PersonaVersion> {
    Ok(PersonaVersion {
        version: to_u32(get(row, "version")?),
        op: PersonaOp::parse(&get::<String>(row, "op")?)?,
        body: get(row, "body")?,
        attrs: doc(row, "attrs")?,
        author: get(row, "author")?,
        created_at: from_ms(get(row, "created_at_ms")?),
    })
}

const PERSONA_COLUMNS: &str = "version, op, body, attrs, author, created_at_ms";

fn profile_ids(p: &ProfileScope) -> (String, String) {
    (p.team_id.to_string(), p.agent_id.to_string())
}

/// SQLite's `LIKE` ignores ASCII case; `ILIKE` keeps that behaviour.
const SLUG_PREFIX: &str = "v.slug ILIKE ? ESCAPE '\\'";

async fn latest_scene(
    conn: &mut PgConnection,
    s: &Schema,
    scene_id: &str,
) -> DomainResult<Option<SceneVersion>> {
    fetch_optional(
        conn,
        &format!("SELECT {SCENE_COLUMNS} FROM {s}.scene_versions v JOIN {s}.scenes s ON s.id = v.scene_id WHERE v.scene_id = ? ORDER BY v.version DESC LIMIT 1"),
        &args![scene_id],
    )
    .await?
    .as_ref()
    .map(scene_from_row)
    .transpose()
}

/// Appends one version inside the caller's transaction (see
/// [`ProfileVersionStore::append_scenes`]).
async fn append_scene(
    tx: &mut PgConnection,
    s: &Schema,
    team: &str,
    agent: &str,
    new: NewSceneVersion,
    now: i64,
) -> DomainResult<SceneVersion> {
    let owner = fetch_optional(
        tx,
        &format!("SELECT team_id, agent_id FROM {s}.scenes WHERE id = ?"),
        &args![new.scene_id.as_str()],
    )
    .await?
    .map(|r| -> DomainResult<(String, String)> { Ok((get(&r, "team_id")?, get(&r, "agent_id")?)) })
    .transpose()?;
    if owner.as_ref().is_some_and(|(t, a)| t != team || a != agent) {
        return Err(unknown_scene(&new.scene_id));
    }
    let latest = latest_scene(tx, s, &new.scene_id).await?;
    if let Some(expected) = new.expected_version {
        let found = latest.as_ref().map_or(0, |l| l.version);
        if found != expected {
            return Err(scene_changed(&new.slug, expected, found));
        }
    }
    if let Some(latest) = &latest {
        if latest.same_content(&new) || (!latest.is_live() && new.op == SceneOp::Retire) {
            return Ok(latest.clone());
        }
    }
    if new.op != SceneOp::Retire {
        let taken = count(
            tx,
            &format!(
                "SELECT COUNT(*) FROM {s}.scene_versions v JOIN {s}.scenes s ON s.id = v.scene_id \
                 WHERE s.team_id = ? AND s.agent_id = ? AND v.scene_id <> ? AND v.slug = ? AND v.op <> 'retire' AND {}",
                is_latest(s)
            ),
            &args![team, agent, new.scene_id.as_str(), new.slug.as_str()],
        )
        .await?;
        if taken > 0 {
            return Err(slug_taken(&new.slug));
        }
    }
    if owner.is_none() {
        execute(
            tx,
            &format!(
                "INSERT INTO {s}.scenes (id, team_id, agent_id, created_at_ms) VALUES (?,?,?,?)"
            ),
            &args![new.scene_id.as_str(), team, agent, now],
        )
        .await?;
    }
    let version = latest.as_ref().map_or(1, |l| l.version + 1);
    execute(
        tx,
        &format!("INSERT INTO {s}.scene_versions (scene_id, version, op, slug, summary, heat, body, attrs, parents, author, created_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?)"),
        &[
            Arg::from(new.scene_id.as_str()),
            Arg::from(version),
            Arg::from(new.op.as_str()),
            Arg::from(new.slug.as_str()),
            Arg::from(new.summary.as_str()),
            Arg::from(new.heat),
            Arg::from(new.body.as_str()),
            to_doc(&new.attrs)?,
            to_doc(&new.parents)?,
            Arg::from(new.author.clone()),
            Arg::from(now),
        ],
    )
    .await?;
    Ok(SceneVersion {
        version,
        scene_created_at: latest.map_or(from_ms(now), |l| l.scene_created_at),
        created_at: from_ms(now),
        scene_id: new.scene_id,
        op: new.op,
        slug: new.slug,
        summary: new.summary,
        heat: new.heat,
        body: new.body,
        attrs: new.attrs,
        parents: new.parents,
        author: new.author,
    })
}

#[async_trait]
impl ProfileVersionStore for PostgresMemoryStore {
    async fn scene_heads(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        prefix: &str,
    ) -> DomainResult<Vec<SceneVersion>> {
        let s = self.db.schema(department).await?;
        let (team, agent) = profile_ids(profile);
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT {SCENE_COLUMNS} FROM {s}.scene_versions v JOIN {s}.scenes s ON s.id = v.scene_id \
                 WHERE s.team_id = ? AND s.agent_id = ? AND {} AND v.op <> 'retire' AND {SLUG_PREFIX} ORDER BY v.slug",
                is_latest(&s)
            ),
            &args![team, agent, like_prefix(prefix)],
        )
        .await?;
        rows.iter().map(scene_from_row).collect()
    }

    async fn scene(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        slug: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<SceneVersion>> {
        let s = self.db.schema(department).await?;
        let (team, agent) = profile_ids(profile);
        let mut conn = self.db.conn().await?;
        let scene_id: Option<String> = fetch_optional(
            &mut conn,
            &format!(
                "SELECT v.scene_id FROM {s}.scene_versions v JOIN {s}.scenes s ON s.id = v.scene_id \
                 WHERE s.team_id = ? AND s.agent_id = ? AND v.slug = ? AND {} \
                 ORDER BY v.op = 'retire', v.created_at_ms DESC LIMIT 1",
                is_latest(&s)
            ),
            &args![team, agent, slug],
        )
        .await?
        .map(|r| get(&r, "scene_id"))
        .transpose()?;
        let Some(scene_id) = scene_id else {
            return Ok(None);
        };
        match version {
            None => latest_scene(&mut conn, &s, &scene_id).await,
            Some(v) => fetch_optional(
                &mut conn,
                &format!("SELECT {SCENE_COLUMNS} FROM {s}.scene_versions v JOIN {s}.scenes s ON s.id = v.scene_id WHERE v.scene_id = ? AND v.version = ?"),
                &args![scene_id, v],
            )
            .await?
            .as_ref()
            .map(scene_from_row)
            .transpose(),
        }
    }

    async fn scene_by_id(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scene_id: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<SceneVersion>> {
        let s = self.db.schema(department).await?;
        let (team, agent) = profile_ids(profile);
        let mut params = args![scene_id, team, agent].to_vec();
        let cond = match version {
            Some(v) => {
                params.push(Arg::from(v));
                "AND v.version = ?"
            }
            None => "",
        };
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT {SCENE_COLUMNS} FROM {s}.scene_versions v JOIN {s}.scenes s ON s.id = v.scene_id \
                 WHERE v.scene_id = ? AND s.team_id = ? AND s.agent_id = ? {cond} ORDER BY v.version DESC LIMIT 1"
            ),
            &params,
        )
        .await?
        .as_ref()
        .map(scene_from_row)
        .transpose()
    }

    async fn append_scenes(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        versions: Vec<NewSceneVersion>,
    ) -> DomainResult<Vec<SceneVersion>> {
        let s = self.db.schema(department).await?;
        let (team, agent) = profile_ids(profile);
        let now = to_ms(Utc::now());
        let mut tx = self.db.begin().await?;
        // Concurrent writers of one profile would both derive `latest + 1`
        // and could both claim a free slug.
        xact_lock(&mut tx, &format!("{s}.scenes:{team}/{agent}")).await?;
        let mut written = Vec::with_capacity(versions.len());
        for v in versions {
            written.push(append_scene(&mut tx, &s, &team, &agent, v, now).await?);
        }
        tx.commit().await.map_err(pg_err)?;
        Ok(written)
    }

    async fn count_scenes(
        &self,
        department: &DepartmentId,
        profile: Option<&ProfileScope>,
        prefix: &str,
    ) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        if let Some(p) = profile {
            w.eq("s.team_id", p.team_id.to_string())
                .eq("s.agent_id", p.agent_id.to_string());
        }
        w.raw(&is_latest(&s), [])
            .raw("v.op <> 'retire'", [])
            .raw(SLUG_PREFIX, [Arg::from(like_prefix(prefix))]);
        count(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT COUNT(*) FROM {s}.scene_versions v JOIN {s}.scenes s ON s.id = v.scene_id {}",
                w.sql()
            ),
            &w.params,
        )
        .await
    }

    async fn persona(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        version: Option<u32>,
    ) -> DomainResult<Option<PersonaVersion>> {
        let s = self.db.schema(department).await?;
        let (team, agent) = profile_ids(profile);
        let mut params = args![team, agent].to_vec();
        let cond = match version {
            Some(v) => {
                params.push(Arg::from(v));
                "AND version = ?"
            }
            None => "",
        };
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT {PERSONA_COLUMNS} FROM {s}.persona_versions WHERE team_id = ? AND agent_id = ? {cond} ORDER BY version DESC LIMIT 1"),
            &params,
        )
        .await?
        .as_ref()
        .map(persona_from_row)
        .transpose()
    }

    async fn append_persona(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        new: NewPersonaVersion,
    ) -> DomainResult<PersonaVersion> {
        let s = self.db.schema(department).await?;
        let (team, agent) = profile_ids(profile);
        let now = to_ms(Utc::now());
        let mut tx = self.db.begin().await?;
        xact_lock(&mut tx, &format!("{s}.persona_versions:{team}/{agent}")).await?;
        let latest = fetch_optional(
            &mut tx,
            &format!("SELECT {PERSONA_COLUMNS} FROM {s}.persona_versions WHERE team_id = ? AND agent_id = ? ORDER BY version DESC LIMIT 1"),
            &args![team.as_str(), agent.as_str()],
        )
        .await?
        .as_ref()
        .map(persona_from_row)
        .transpose()?;
        if let Some(latest) = latest.as_ref().filter(|l| l.body == new.body) {
            return Ok(latest.clone());
        }
        let version = latest.map_or(1, |l| l.version + 1);
        execute(
            &mut tx,
            &format!("INSERT INTO {s}.persona_versions (team_id, agent_id, version, op, body, attrs, author, created_at_ms) VALUES (?,?,?,?,?,?,?,?)"),
            &[
                Arg::from(team.as_str()),
                Arg::from(agent.as_str()),
                Arg::from(version),
                Arg::from(new.op.as_str()),
                Arg::from(new.body.as_str()),
                to_doc(&new.attrs)?,
                Arg::from(new.author.clone()),
                Arg::from(now),
            ],
        )
        .await?;
        tx.commit().await.map_err(pg_err)?;
        Ok(PersonaVersion {
            version,
            op: new.op,
            body: new.body,
            attrs: new.attrs,
            author: new.author,
            created_at: from_ms(now),
        })
    }

    async fn count_personas(
        &self,
        department: &DepartmentId,
        profile: Option<&ProfileScope>,
    ) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        if let Some(p) = profile {
            profile_where(&mut w, p);
        }
        count(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT COUNT(*) FROM (SELECT DISTINCT team_id, agent_id FROM {s}.persona_versions {}) p",
                w.sql()
            ),
            &w.params,
        )
        .await
    }

    async fn purge(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        let (team, agent) = profile_ids(profile);
        let ids = args![team.as_str(), agent.as_str()];
        let mut tx = self.db.begin().await?;
        // The writers' locks: a concurrent append would otherwise insert a
        // version after its scene is gone, out of reach of any later purge.
        xact_lock(&mut tx, &format!("{s}.scenes:{team}/{agent}")).await?;
        xact_lock(&mut tx, &format!("{s}.persona_versions:{team}/{agent}")).await?;
        let scenes = count(
            &mut tx,
            &format!("SELECT COUNT(*) FROM {s}.scenes WHERE team_id = ? AND agent_id = ?"),
            &ids,
        )
        .await?;
        let persona = count(
            &mut tx,
            &format!("SELECT COUNT(*) FROM (SELECT 1 FROM {s}.persona_versions WHERE team_id = ? AND agent_id = ? LIMIT 1) p"),
            &ids,
        )
        .await?;
        // Versions go with their scene (ON DELETE CASCADE); `profile_files`
        // is the pre-version storage, which would otherwise keep a copy.
        for table in ["scenes", "persona_versions", "profile_files"] {
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.{table} WHERE team_id = ? AND agent_id = ?"),
                &ids,
            )
            .await?;
        }
        tx.commit().await.map_err(pg_err)?;
        Ok(scenes + persona)
    }
}

#[async_trait]
impl ScopeMigrationStore for PostgresMemoryStore {
    async fn reassign_default(
        &self,
        department: &DepartmentId,
        from: &MemoryScope,
        to: &MemoryScope,
        dry_run: bool,
    ) -> DomainResult<MovedRows> {
        let s = self.db.schema(department).await?;
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
        run(
            format!("UPDATE {s}.profile_files SET team_id = $1, agent_id = $2 WHERE team_id = $3 AND agent_id = $4"),
            &[team, agent, ft, fa],
        )
        .await?;
        let mut profile_files = 0;
        for table in ["scenes", "persona_versions"] {
            profile_files += run(
                format!("UPDATE {s}.{table} SET team_id = $1, agent_id = $2 WHERE team_id = $3 AND agent_id = $4"),
                &[team, agent, ft, fa],
            )
            .await?;
        }
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
