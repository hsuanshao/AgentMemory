//! SQLite adapter for L0 conversations, L1 atoms and versioned profile files.

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
use memory_domain::retrieval::cosine;
use memory_domain::scope::{DepartmentId, MemoryScope};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use super::db::DepartmentPool;
use super::sql::{blob_to_embedding, embedding_to_blob, from_ms, like_prefix, to_ms, Where};
use super::vec_index::{self, KnnFilter, VecIndex};
use crate::text::{bm25_to_score, index_text, match_query};

pub struct SqliteMemoryStore {
    pool: Arc<DepartmentPool>,
    /// `None` when embedding dims are unknown or sqlite-vec did not load;
    /// vector search is then brute-force cosine over the BLOBs.
    vec: Option<VecIndex>,
}

impl SqliteMemoryStore {
    /// `vector_dims` is the embedding provider's dimension; with it (and the
    /// extension available) atoms are also indexed in sqlite-vec.
    pub fn new(pool: Arc<DepartmentPool>, vector_dims: Option<usize>) -> Self {
        Self {
            pool,
            vec: VecIndex::new(vector_dims),
        }
    }

    /// How vector search is executed, for `/health`.
    pub fn vector_index(&self) -> &'static str {
        if self.vec.is_some() {
            "sqlite-vec"
        } else {
            "brute-force"
        }
    }
}

/// Brings the vector index up to date before an atom write, or marks it
/// stale when this store runs without one.
fn prepare_index(conn: &Connection, vec: Option<VecIndex>) -> rusqlite::Result<()> {
    match vec {
        Some(v) => v.ensure(conn),
        None => vec_index::invalidate(conn),
    }
}

fn conversion_err(e: memory_domain::error::DomainError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}

fn scope_from_row(row: &Row<'_>) -> rusqlite::Result<MemoryScope> {
    let team: String = row.get("team_id")?;
    let agent: String = row.get("agent_id")?;
    let user: String = row.get("user_id")?;
    let task: Option<String> = row.get("task_id")?;
    MemoryScope::from_optional(Some(&team), Some(&agent), Some(&user), task.as_deref())
        .map_err(conversion_err)
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
        w.raw(&format!("{column} >= ?"), [Value::from(to_ms(s))]);
    }
    if let Some(e) = end {
        w.raw(&format!("{column} <= ?"), [Value::from(to_ms(e))]);
    }
}

// ── L0 ──────────────────────────────────────────────────────────────────

const CONV_COLUMNS: &str = "id, team_id, agent_id, user_id, task_id, session_id, role, content, timestamp_ms, recorded_at_ms, version";

fn conversation_from_row(row: &Row<'_>) -> rusqlite::Result<ConversationMessage> {
    let role: String = row.get("role")?;
    Ok(ConversationMessage {
        id: row.get("id")?,
        scope: scope_from_row(row)?,
        session_id: row.get("session_id")?,
        role: if role == "assistant" {
            Role::Assistant
        } else {
            Role::User
        },
        content: row.get("content")?,
        timestamp: from_ms(row.get("timestamp_ms")?),
        recorded_at: from_ms(row.get("recorded_at_ms")?),
        version: row.get("version")?,
    })
}

fn conversation_where(
    scope: Option<&MemoryScope>,
    filter: &ConversationFilter,
    prefix: &str,
) -> Where {
    let col = |c: &str| format!("{prefix}{c}");
    let mut w = Where::default();
    if let Some(s) = scope {
        w.eq(&col("team_id"), s.team_id.to_string())
            .eq(&col("agent_id"), s.agent_id.to_string())
            .eq(&col("user_id"), s.user_id.to_string());
    }
    w.eq_opt(&col("session_id"), filter.session_id.clone());
    time_where(
        &mut w,
        &col("timestamp_ms"),
        filter.time.start,
        filter.time.end,
    );
    w
}

#[async_trait]
impl ConversationRepository for SqliteMemoryStore {
    async fn append(
        &self,
        department: &DepartmentId,
        messages: &[ConversationMessage],
    ) -> DomainResult<()> {
        let messages = messages.to_vec();
        self.pool
            .get(department)?
            .call(move |conn| {
                let tx = conn.transaction()?;
                for m in &messages {
                    tx.execute(
                        &format!("INSERT OR REPLACE INTO conversations ({CONV_COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?,?)"),
                        params![
                            m.id,
                            m.scope.team_id.as_str(),
                            m.scope.agent_id.as_str(),
                            m.scope.user_id.as_str(),
                            m.scope.task_id.as_ref().map(|t| t.to_string()),
                            m.session_id,
                            if m.role == Role::User { "user" } else { "assistant" },
                            m.content,
                            to_ms(m.timestamp),
                            to_ms(m.recorded_at),
                            m.version
                        ],
                    )?;
                    tx.execute("DELETE FROM conversations_fts WHERE id = ?", [&m.id])?;
                    tx.execute("INSERT INTO conversations_fts (tokens, id) VALUES (?, ?)", params![index_text(&m.content), m.id])?;
                }
                tx.commit()
            })
            .await
    }

    async fn query(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        filter: &ConversationFilter,
        page: Page,
    ) -> DomainResult<(Vec<ConversationMessage>, usize)> {
        let w = conversation_where(Some(scope), filter, "");
        self.pool
            .get(department)?
            .call(move |conn| {
                let total: usize = conn.query_row(&format!("SELECT COUNT(*) FROM conversations {}", w.sql()), params_from_iter(w.params.iter()), |r| r.get(0))?;
                let sql = format!("SELECT {CONV_COLUMNS} FROM conversations {} ORDER BY timestamp_ms DESC, id LIMIT ? OFFSET ?", w.sql());
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(params_from_iter(w.with([Value::from(page.limit as i64), Value::from(page.offset as i64)])), conversation_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((rows, total))
            })
            .await
    }

    async fn search(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        filter: &ConversationFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<ConversationMessage>>> {
        let Some(fts) = match_query(query) else {
            return Ok(Vec::new());
        };
        let mut w = conversation_where(Some(scope), filter, "c.");
        w.raw("conversations_fts MATCH ?", [Value::from(fts)]);
        self.pool
            .get(department)?
            .call(move |conn| {
                let cols: Vec<String> = CONV_COLUMNS.split(", ").map(|c| format!("c.{c}")).collect();
                let sql = format!(
                    "SELECT {}, bm25(conversations_fts) AS rank FROM conversations_fts JOIN conversations c ON c.id = conversations_fts.id {} ORDER BY rank LIMIT ?",
                    cols.join(", "),
                    w.sql()
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(params_from_iter(w.with([Value::from(limit as i64)])), |row| {
                        Ok(Scored { item: conversation_from_row(row)?, score: bm25_to_score(row.get("rank")?) })
                    })?
                    .collect();
                rows
            })
            .await
    }

    async fn count(
        &self,
        department: &DepartmentId,
        scope: Option<&MemoryScope>,
        filter: &ConversationFilter,
    ) -> DomainResult<usize> {
        let w = conversation_where(scope, filter, "");
        self.pool
            .get(department)?
            .call(move |conn| {
                conn.query_row(
                    &format!("SELECT COUNT(*) FROM conversations {}", w.sql()),
                    params_from_iter(w.params.iter()),
                    |r| r.get(0),
                )
            })
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
        delete_conversations(&self.pool, department, w).await
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
        delete_conversations(&self.pool, department, w).await
    }

    async fn delete_profile(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        profile_where(&mut w, profile);
        delete_conversations(&self.pool, department, w).await
    }

    async fn after_seq(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        session_id: &str,
        after: Option<i64>,
        limit: usize,
    ) -> DomainResult<Vec<Sequenced<ConversationMessage>>> {
        let mut w = Where::default();
        scope_where(&mut w, scope);
        w.eq("session_id", session_id.to_owned())
            .raw("seq > ?", [Value::from(after.unwrap_or(0))]);
        self.pool
            .get(department)?
            .call(move |conn| {
                let sql = format!(
                    "SELECT seq, {CONV_COLUMNS} FROM conversations {} ORDER BY seq LIMIT ?",
                    w.sql()
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(
                        params_from_iter(w.with([Value::from(limit as i64)])),
                        |row| {
                            Ok(Sequenced {
                                seq: row.get("seq")?,
                                item: conversation_from_row(row)?,
                            })
                        },
                    )?
                    .collect();
                rows
            })
            .await
    }
}

async fn delete_conversations(
    pool: &DepartmentPool,
    department: &DepartmentId,
    w: Where,
) -> DomainResult<usize> {
    pool.get(department)?
        .call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                &format!(
                    "DELETE FROM conversations_fts WHERE id IN (SELECT id FROM conversations {})",
                    w.sql()
                ),
                params_from_iter(w.params.iter()),
            )?;
            let n = tx.execute(
                &format!("DELETE FROM conversations {}", w.sql()),
                params_from_iter(w.params.iter()),
            )?;
            tx.commit()?;
            Ok(n)
        })
        .await
}

// ── L1 ──────────────────────────────────────────────────────────────────

const ATOM_COLUMNS: &str = "id, team_id, agent_id, user_id, task_id, session_id, atom_type, content, background, priority, source_ids, version, created_at_ms, updated_at_ms";

fn atom_from_row(row: &Row<'_>) -> rusqlite::Result<AtomicMemory> {
    let kind: String = row.get("atom_type")?;
    let sources: String = row.get("source_ids")?;
    Ok(AtomicMemory {
        id: row.get("id")?,
        scope: scope_from_row(row)?,
        atom_type: AtomType::parse(&kind).map_err(conversion_err)?,
        content: row.get("content")?,
        background: row.get("background")?,
        priority: row.get("priority")?,
        source_message_ids: serde_json::from_str(&sources).unwrap_or_default(),
        session_id: row.get("session_id")?,
        version: row.get("version")?,
        created_at: from_ms(row.get("created_at_ms")?),
        updated_at: from_ms(row.get("updated_at_ms")?),
    })
}

fn atom_where(selector: Option<AtomSelector<'_>>, filter: &AtomFilter, prefix: &str) -> Where {
    let mut w = Where::default();
    let col = |c: &str| format!("{prefix}{c}");
    match selector {
        Some(AtomSelector::Scope(s)) => {
            w.eq(&col("team_id"), s.team_id.to_string())
                .eq(&col("agent_id"), s.agent_id.to_string())
                .eq(&col("user_id"), s.user_id.to_string());
        }
        Some(AtomSelector::Profile(p)) => {
            w.eq(&col("team_id"), p.team_id.to_string())
                .eq(&col("agent_id"), p.agent_id.to_string());
        }
        None => {}
    }
    w.eq_opt(
        &col("atom_type"),
        filter.atom_type.map(|t| t.as_str().to_owned()),
    );
    time_where(
        &mut w,
        &col("updated_at_ms"),
        filter.time.start,
        filter.time.end,
    );
    w
}

#[async_trait]
impl AtomRepository for SqliteMemoryStore {
    async fn upsert(
        &self,
        department: &DepartmentId,
        atom: &AtomicMemory,
        embedding: Option<&[f32]>,
    ) -> DomainResult<()> {
        let atom = atom.clone();
        let blob = embedding.map(embedding_to_blob);
        let vec = self.vec;
        self.pool
            .get(department)?
            .call(move |conn| {
                let tx = conn.transaction()?;
                prepare_index(&tx, vec)?;
                // The replace below drops the old row (and its seq), so its
                // index entry has to go too.
                let old_seq: Option<i64> = match vec {
                    Some(_) => tx.query_row("SELECT seq FROM atoms WHERE id = ?", [&atom.id], |r| r.get(0)).optional()?,
                    None => None,
                };
                tx.execute(
                    &format!("INSERT OR REPLACE INTO atoms ({ATOM_COLUMNS}, embedding) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"),
                    params![
                        atom.id,
                        atom.scope.team_id.as_str(),
                        atom.scope.agent_id.as_str(),
                        atom.scope.user_id.as_str(),
                        atom.scope.task_id.as_ref().map(|t| t.to_string()),
                        atom.session_id,
                        atom.atom_type.as_str(),
                        atom.content,
                        atom.background,
                        atom.priority,
                        serde_json::to_string(&atom.source_message_ids).unwrap_or_else(|_| "[]".into()),
                        atom.version,
                        to_ms(atom.created_at),
                        to_ms(atom.updated_at),
                        blob
                    ],
                )?;
                if let Some(v) = vec {
                    let seq = tx.last_insert_rowid();
                    v.remove(&tx, old_seq.as_slice())?;
                    v.insert(&tx, seq)?;
                }
                tx.execute("DELETE FROM atoms_fts WHERE id = ?", [&atom.id])?;
                let text = format!("{} {}", atom.content, atom.background.as_deref().unwrap_or(""));
                tx.execute("INSERT INTO atoms_fts (tokens, id) VALUES (?, ?)", params![index_text(&text), atom.id])?;
                tx.commit()
            })
            .await
    }

    async fn get(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<AtomicMemory>> {
        let id = id.to_owned();
        self.pool
            .get(department)?
            .call(move |conn| {
                let mut stmt =
                    conn.prepare(&format!("SELECT {ATOM_COLUMNS} FROM atoms WHERE id = ?"))?;
                let mut rows = stmt.query_map([id], atom_from_row)?;
                rows.next().transpose()
            })
            .await
    }

    async fn query(
        &self,
        department: &DepartmentId,
        selector: AtomSelector<'_>,
        filter: &AtomFilter,
        page: Page,
    ) -> DomainResult<(Vec<AtomicMemory>, usize)> {
        let w = atom_where(Some(selector), filter, "");
        self.pool
            .get(department)?
            .call(move |conn| {
                let total: usize = conn.query_row(&format!("SELECT COUNT(*) FROM atoms {}", w.sql()), params_from_iter(w.params.iter()), |r| r.get(0))?;
                let sql = format!("SELECT {ATOM_COLUMNS} FROM atoms {} ORDER BY updated_at_ms DESC, id LIMIT ? OFFSET ?", w.sql());
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(params_from_iter(w.with([Value::from(page.limit as i64), Value::from(page.offset as i64)])), atom_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((rows, total))
            })
            .await
    }

    async fn after_seq(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        after: Option<i64>,
        limit: usize,
    ) -> DomainResult<Vec<Sequenced<AtomicMemory>>> {
        let mut w = Where::default();
        profile_where(&mut w, profile);
        w.raw("seq > ?", [Value::from(after.unwrap_or(0))]);
        self.pool
            .get(department)?
            .call(move |conn| {
                let sql = format!(
                    "SELECT seq, {ATOM_COLUMNS} FROM atoms {} ORDER BY seq LIMIT ?",
                    w.sql()
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(
                        params_from_iter(w.with([Value::from(limit as i64)])),
                        |row| {
                            Ok(Sequenced {
                                seq: row.get("seq")?,
                                item: atom_from_row(row)?,
                            })
                        },
                    )?
                    .collect();
                rows
            })
            .await
    }

    async fn keyword_search(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        filter: &AtomFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<AtomicMemory>>> {
        let Some(fts) = match_query(query) else {
            return Ok(Vec::new());
        };
        let mut w = atom_where(Some(AtomSelector::Scope(scope)), filter, "a.");
        w.raw("atoms_fts MATCH ?", [Value::from(fts)]);
        self.pool
            .get(department)?
            .call(move |conn| {
                let cols: Vec<String> = ATOM_COLUMNS.split(", ").map(|c| format!("a.{c}")).collect();
                let sql = format!(
                    "SELECT {}, bm25(atoms_fts) AS rank FROM atoms_fts JOIN atoms a ON a.id = atoms_fts.id {} ORDER BY rank LIMIT ?",
                    cols.join(", "),
                    w.sql()
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(params_from_iter(w.with([Value::from(limit as i64)])), |row| {
                        Ok(Scored { item: atom_from_row(row)?, score: bm25_to_score(row.get("rank")?) })
                    })?
                    .collect();
                rows
            })
            .await
    }

    /// sqlite-vec KNN when the index is enabled and the query has its
    /// dimension; otherwise brute-force cosine over the scope's embedded
    /// atoms, which is adequate for small standalone deployments.
    async fn vector_search(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        embedding: &[f32],
        filter: &AtomFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<AtomicMemory>>> {
        if let Some(v) = self.vec.filter(|v| v.dims() == embedding.len()) {
            return self
                .knn_search(v, department, scope, embedding, filter, limit)
                .await;
        }
        let mut w = atom_where(Some(AtomSelector::Scope(scope)), filter, "");
        w.raw("embedding IS NOT NULL", []);
        let query = embedding.to_vec();
        self.pool
            .get(department)?
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {ATOM_COLUMNS}, embedding FROM atoms {}",
                    w.sql()
                ))?;
                let mut scored = stmt
                    .query_map(params_from_iter(w.params.iter()), |row| {
                        let blob: Vec<u8> = row.get("embedding")?;
                        Ok(Scored {
                            item: atom_from_row(row)?,
                            score: cosine(&query, &blob_to_embedding(&blob)) as f64,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                scored.sort_by(|a, b| b.score.total_cmp(&a.score));
                scored.truncate(limit);
                Ok(scored)
            })
            .await
    }

    async fn count(
        &self,
        department: &DepartmentId,
        scope: Option<&MemoryScope>,
        filter: &AtomFilter,
    ) -> DomainResult<usize> {
        let w = atom_where(scope.map(AtomSelector::Scope), filter, "");
        self.pool
            .get(department)?
            .call(move |conn| {
                conn.query_row(
                    &format!("SELECT COUNT(*) FROM atoms {}", w.sql()),
                    params_from_iter(w.params.iter()),
                    |r| r.get(0),
                )
            })
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
        delete_atoms(&self.pool, self.vec, department, w).await
    }

    async fn delete_profile(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        profile_where(&mut w, profile);
        delete_atoms(&self.pool, self.vec, department, w).await
    }
}

impl SqliteMemoryStore {
    /// KNN through the vec0 index. Isolation and the filter are applied in
    /// the index scan; the join back to `atoms` re-checks them so a stale
    /// or foreign index row can never leak into the result.
    async fn knn_search(
        &self,
        vec: VecIndex,
        department: &DepartmentId,
        scope: &MemoryScope,
        embedding: &[f32],
        filter: &AtomFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<AtomicMemory>>> {
        let knn = KnnFilter {
            team_id: scope.team_id.to_string(),
            agent_id: scope.agent_id.to_string(),
            user_id: scope.user_id.to_string(),
            atom_type: filter.atom_type.map(|t| t.as_str().to_owned()),
            updated_from_ms: filter.time.start.map(to_ms),
            updated_to_ms: filter.time.end.map(to_ms),
        };
        let w = atom_where(Some(AtomSelector::Scope(scope)), filter, "");
        let query = embedding.to_vec();
        self.pool
            .get(department)?
            .call(move |conn| {
                let tx = conn.transaction()?;
                vec.ensure(&tx)?;
                let hits = vec.knn(&tx, &query, &knn, limit)?;
                let scored = load_hits(&tx, w, &hits)?;
                tx.commit()?;
                Ok(scored)
            })
            .await
    }
}

/// Loads the atoms behind KNN hits, in hit order, scored as cosine
/// similarity (`1 - cosine distance`) like the brute-force path.
fn load_hits(
    conn: &Connection,
    mut w: Where,
    hits: &[(i64, f64)],
) -> rusqlite::Result<Vec<Scored<AtomicMemory>>> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; hits.len()].join(",");
    w.raw(
        &format!("seq IN ({marks})"),
        hits.iter().map(|(seq, _)| Value::from(*seq)),
    );
    let mut stmt = conn.prepare(&format!(
        "SELECT seq, {ATOM_COLUMNS} FROM atoms {}",
        w.sql()
    ))?;
    let mut atoms: std::collections::HashMap<i64, AtomicMemory> = stmt
        .query_map(params_from_iter(w.params.iter()), |row| {
            Ok((row.get("seq")?, atom_from_row(row)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(hits
        .iter()
        .filter_map(|(seq, distance)| {
            let similarity = 1.0 - distance;
            atoms.remove(seq).map(|item| Scored {
                item,
                score: if similarity.is_finite() {
                    similarity
                } else {
                    0.0
                },
            })
        })
        .collect())
}

async fn delete_atoms(
    pool: &DepartmentPool,
    vec: Option<VecIndex>,
    department: &DepartmentId,
    w: Where,
) -> DomainResult<usize> {
    pool.get(department)?
        .call(move |conn| {
            let tx = conn.transaction()?;
            prepare_index(&tx, vec)?;
            if let Some(v) = vec {
                let seqs = tx
                    .prepare(&format!("SELECT seq FROM atoms {}", w.sql()))?
                    .query_map(params_from_iter(w.params.iter()), |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<i64>>>()?;
                v.remove(&tx, &seqs)?;
            }
            tx.execute(
                &format!(
                    "DELETE FROM atoms_fts WHERE id IN (SELECT id FROM atoms {})",
                    w.sql()
                ),
                params_from_iter(w.params.iter()),
            )?;
            let n = tx.execute(
                &format!("DELETE FROM atoms {}", w.sql()),
                params_from_iter(w.params.iter()),
            )?;
            tx.commit()?;
            Ok(n)
        })
        .await
}

// ── L2 / L3 ─────────────────────────────────────────────────────────────

fn file_from_row(row: &Row<'_>) -> rusqlite::Result<ProfileFile> {
    Ok(ProfileFile {
        path: row.get("path")?,
        content: row.get("content")?,
        version: row.get("version")?,
        created_at: from_ms(row.get("created_at_ms")?),
        updated_at: from_ms(row.get("updated_at_ms")?),
    })
}

fn profile_ids(p: &ProfileScope) -> (String, String) {
    (p.team_id.to_string(), p.agent_id.to_string())
}

#[async_trait]
impl ProfileFileRepository for SqliteMemoryStore {
    async fn list(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        prefix: &str,
    ) -> DomainResult<Vec<ProfileFile>> {
        let (team, agent) = profile_ids(profile);
        let like = like_prefix(prefix);
        self.pool
            .get(department)?
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT path, content, version, created_at_ms, updated_at_ms FROM profile_files \
                     WHERE team_id = ? AND agent_id = ? AND is_head = 1 AND path LIKE ? ESCAPE '\\' ORDER BY path",
                )?;
                let rows = stmt.query_map(params![team, agent, like], file_from_row)?.collect();
                rows
            })
            .await
    }

    async fn read(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        path: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<ProfileFile>> {
        let (team, agent) = profile_ids(profile);
        let path = path.to_owned();
        self.pool
            .get(department)?
            .call(move |conn| {
                let (cond, v) = match version {
                    Some(v) => ("version = ?", Value::from(v as i64)),
                    None => ("is_head = ?", Value::from(1i64)),
                };
                let sql = format!("SELECT path, content, version, created_at_ms, updated_at_ms FROM profile_files WHERE team_id = ? AND agent_id = ? AND path = ? AND {cond}");
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query_map(params![team, agent, path, v], file_from_row)?;
                rows.next().transpose()
            })
            .await
    }

    async fn write(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        path: &str,
        content: &str,
    ) -> DomainResult<ProfileFile> {
        let (team, agent) = profile_ids(profile);
        let (path, content) = (path.to_owned(), content.to_owned());
        let now = to_ms(Utc::now());
        self.pool
            .get(department)?
            .call(move |conn| {
                let tx = conn.transaction()?;
                let head: Option<(u32, String, i64)> = tx
                    .query_row(
                        "SELECT version, content, created_at_ms FROM profile_files WHERE team_id = ? AND agent_id = ? AND path = ? AND is_head = 1",
                        params![team, agent, path],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .ok();
                let file = match head {
                    Some((v, old, created)) if old == content => ProfileFile { path, content, version: v, created_at: from_ms(created), updated_at: from_ms(now) },
                    other => {
                        let (version, created) = other.map_or((1, now), |(v, _, c)| (v + 1, c));
                        tx.execute("UPDATE profile_files SET is_head = 0 WHERE team_id = ? AND agent_id = ? AND path = ?", params![team, agent, path])?;
                        tx.execute(
                            "INSERT INTO profile_files (team_id, agent_id, path, version, content, is_head, created_at_ms, updated_at_ms) VALUES (?,?,?,?,?,1,?,?)",
                            params![team, agent, path, version, content, created, now],
                        )?;
                        ProfileFile { path, content, version, created_at: from_ms(created), updated_at: from_ms(now) }
                    }
                };
                tx.commit()?;
                Ok(file)
            })
            .await
    }

    async fn remove(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        path: &str,
    ) -> DomainResult<usize> {
        let (team, agent) = profile_ids(profile);
        let (cond, value) = if path.ends_with('/') {
            ("path LIKE ? ESCAPE '\\'", like_prefix(path))
        } else {
            ("path = ?", path.to_owned())
        };
        self.pool
            .get(department)?
            .call(move |conn| {
                let heads: usize = conn.query_row(
                    &format!("SELECT COUNT(*) FROM profile_files WHERE team_id = ? AND agent_id = ? AND is_head = 1 AND {cond}"),
                    params![team, agent, value],
                    |r| r.get(0),
                )?;
                conn.execute(&format!("DELETE FROM profile_files WHERE team_id = ? AND agent_id = ? AND {cond}"), params![team, agent, value])?;
                Ok(heads)
            })
            .await
    }

    async fn count(
        &self,
        department: &DepartmentId,
        profile: Option<&ProfileScope>,
        prefix: &str,
    ) -> DomainResult<usize> {
        let mut w = Where::default();
        if let Some(p) = profile {
            profile_where(&mut w, p);
        }
        w.raw("is_head = 1", []).raw(
            "path LIKE ? ESCAPE '\\'",
            [Value::from(like_prefix(prefix))],
        );
        self.pool
            .get(department)?
            .call(move |conn| {
                conn.query_row(
                    &format!("SELECT COUNT(*) FROM profile_files {}", w.sql()),
                    params_from_iter(w.params.iter()),
                    |r| r.get(0),
                )
            })
            .await
    }
}

#[async_trait]
impl ScopeMigrationStore for SqliteMemoryStore {
    async fn reassign_default(
        &self,
        department: &DepartmentId,
        from: &MemoryScope,
        to: &MemoryScope,
        dry_run: bool,
    ) -> DomainResult<MovedRows> {
        let (team, agent, user) = (
            to.team_id.to_string(),
            to.agent_id.to_string(),
            to.user_id.to_string(),
        );
        let (ft, fa, fu) = (
            from.team_id.to_string(),
            from.agent_id.to_string(),
            from.user_id.to_string(),
        );
        self.pool
            .get(department)?
            .call(move |conn| {
                let tx = conn.transaction()?;
                let triple = "team_id = ?4 AND agent_id = ?5 AND user_id = ?6";
                let conversations = tx.execute(
                    &format!("UPDATE conversations SET team_id = ?1, agent_id = ?2, user_id = ?3 WHERE {triple}"),
                    params![team, agent, user, ft, fa, fu],
                )?;
                let atoms = tx.execute(
                    &format!("UPDATE atoms SET team_id = ?1, agent_id = ?2, user_id = ?3 WHERE {triple}"),
                    params![team, agent, user, ft, fa, fu],
                )?;
                let profile_files = tx.execute(
                    "UPDATE profile_files SET team_id = ?1, agent_id = ?2 WHERE team_id = ?3 AND agent_id = ?4",
                    params![team, agent, ft, fa],
                )?;
                let skill_versions = tx.execute(
                    "UPDATE skills SET team_id = ?1, owner_agent_id = ?2, \
                     owner_user_id = CASE owner_user_id WHEN ?6 THEN ?3 ELSE owner_user_id END, \
                     doc = json_set(doc, '$.team_id', ?1, '$.owner_agent_id', ?2, '$.owner_user_id', \
                                    CASE owner_user_id WHEN ?6 THEN ?3 ELSE owner_user_id END) \
                     WHERE team_id = ?4 AND owner_agent_id = ?5",
                    params![team, agent, user, ft, fa, fu],
                )?;
                // The vec0 index partitions by team; rebuild it from the rows.
                if atoms > 0 {
                    vec_index::invalidate(&tx)?;
                }
                let moved = MovedRows { conversations, atoms, profile_files, skill_versions };
                if dry_run {
                    tx.rollback()?;
                } else {
                    tx.commit()?;
                }
                Ok(moved)
            })
            .await
    }
}
