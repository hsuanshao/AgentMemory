//! Pipeline state (global `system.db`), memory prompts and generation logs
//! (per-department databases).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memory_application::common::Page;
use memory_application::ports::pipeline::{
    GenerationCount, GenerationLog, GenerationLogFilter, GenerationLogRepository, MemoryPrompt,
    MemoryPromptRepository, PipelineStateRepository, ProfileState, PromptLogEntry, PromptSetting,
    PromptSettingFilter, PromptTarget, SessionState,
};
use memory_domain::error::DomainResult;
use memory_domain::memory::Layer;
use memory_domain::profile::ProfileScope;
use memory_domain::scope::{DepartmentId, MemoryScope};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, OptionalExtension};

use super::db::{Db, DepartmentPool};
use super::sql::{from_doc, like_prefix, to_doc, to_ms, Where};

pub struct SqlitePipelineState {
    db: Arc<Db>,
}

impl SqlitePipelineState {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

fn docs<T: serde::de::DeserializeOwned>(rows: Vec<String>) -> rusqlite::Result<Vec<T>> {
    rows.iter().map(|d| from_doc(d)).collect()
}

fn query_docs(
    conn: &rusqlite::Connection,
    sql: &str,
    params: &[Value],
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |r| r.get(0))?
        .collect();
    rows
}

#[async_trait]
impl PipelineStateRepository for SqlitePipelineState {
    async fn get_session(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        session_id: &str,
    ) -> DomainResult<Option<SessionState>> {
        let (i, k, s) = (department.to_string(), scope.key(), session_id.to_owned());
        self.db
            .call(move |c| {
                let doc: Option<String> = c
                    .query_row("SELECT doc FROM session_states WHERE department = ? AND scope_key = ? AND session_id = ?", [i, k, s], |r| r.get(0))
                    .optional()?;
                doc.map(|d| from_doc(&d)).transpose()
            })
            .await
    }

    async fn put_session(&self, state: &SessionState) -> DomainResult<()> {
        let st = state.clone();
        self.db
            .call(move |c| {
                c.execute(
                    "INSERT OR REPLACE INTO session_states (department, scope_key, session_id, l1_due_ms, last_active_ms, doc) VALUES (?,?,?,?,?,?)",
                    params![st.department.as_str(), st.scope.key(), st.session_id, st.l1_due_at.map(to_ms), to_ms(st.last_active_at), to_doc(&st)?],
                )
                .map(|_| ())
            })
            .await
    }

    async fn get_profile(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<Option<ProfileState>> {
        let (i, k) = (department.to_string(), profile.key());
        self.db
            .call(move |c| {
                let doc: Option<String> = c
                    .query_row(
                        "SELECT doc FROM profile_states WHERE department = ? AND profile_key = ?",
                        [i, k],
                        |r| r.get(0),
                    )
                    .optional()?;
                doc.map(|d| from_doc(&d)).transpose()
            })
            .await
    }

    async fn put_profile(&self, state: &ProfileState) -> DomainResult<()> {
        let st = state.clone();
        self.db
            .call(move |c| {
                c.execute(
                    "INSERT OR REPLACE INTO profile_states (department, profile_key, l2_due_ms, updated_ms, doc) VALUES (?,?,?,?,?)",
                    params![st.department.as_str(), st.profile.key(), st.l2_due_at.map(to_ms), to_ms(Utc::now()), to_doc(&st)?],
                )
                .map(|_| ())
            })
            .await
    }

    async fn due_sessions(&self, now: DateTime<Utc>) -> DomainResult<Vec<SessionState>> {
        self.db
            .call(move |c| docs(query_docs(c, "SELECT doc FROM session_states WHERE l1_due_ms IS NOT NULL AND l1_due_ms <= ? LIMIT 500", &[Value::from(to_ms(now))])?))
            .await
    }

    async fn due_profiles(&self, now: DateTime<Utc>) -> DomainResult<Vec<ProfileState>> {
        self.db
            .call(move |c| docs(query_docs(c, "SELECT doc FROM profile_states WHERE l2_due_ms IS NOT NULL AND l2_due_ms <= ? LIMIT 500", &[Value::from(to_ms(now))])?))
            .await
    }

    async fn list_sessions(
        &self,
        department: &DepartmentId,
        limit: usize,
    ) -> DomainResult<Vec<SessionState>> {
        let i = department.to_string();
        self.db
            .call(move |c| docs(query_docs(c, "SELECT doc FROM session_states WHERE department = ? ORDER BY last_active_ms DESC LIMIT ?", &[Value::from(i), Value::from(limit as i64)])?))
            .await
    }

    async fn list_profiles(
        &self,
        department: &DepartmentId,
        limit: usize,
    ) -> DomainResult<Vec<ProfileState>> {
        let i = department.to_string();
        self.db
            .call(move |c| docs(query_docs(c, "SELECT doc FROM profile_states WHERE department = ? ORDER BY updated_ms DESC LIMIT ?", &[Value::from(i), Value::from(limit as i64)])?))
            .await
    }

    async fn delete_profile(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<()> {
        let (i, k) = (department.to_string(), profile.key());
        let team_prefix = like_prefix(&format!("{}/{}/", profile.team_id, profile.agent_id));
        self.db
            .call(move |c| {
                c.execute(
                    "DELETE FROM profile_states WHERE department = ? AND profile_key = ?",
                    [&i, &k],
                )?;
                c.execute(
                    "DELETE FROM session_states WHERE department = ? AND scope_key LIKE ? ESCAPE '\\'",
                    [&i, &team_prefix],
                )?;
                Ok(())
            })
            .await
    }

    async fn delete_department(&self, department: &DepartmentId) -> DomainResult<()> {
        let i = department.to_string();
        self.db
            .call(move |c| {
                c.execute("DELETE FROM session_states WHERE department = ?", [&i])?;
                c.execute("DELETE FROM profile_states WHERE department = ?", [&i])?;
                Ok(())
            })
            .await
    }
}

pub struct SqlitePromptStore {
    pool: Arc<DepartmentPool>,
}

impl SqlitePromptStore {
    pub fn new(pool: Arc<DepartmentPool>) -> Self {
        Self { pool }
    }
}

fn target_str(t: PromptTarget) -> &'static str {
    match t {
        PromptTarget::Department => "department",
        PromptTarget::Team => "team",
        PromptTarget::Agent => "agent",
    }
}

fn parse_target(s: &str) -> PromptTarget {
    match s {
        "team" => PromptTarget::Team,
        "agent" => PromptTarget::Agent,
        _ => PromptTarget::Department,
    }
}

fn setting_where(f: &PromptSettingFilter) -> Where {
    let mut w = Where::default();
    w.eq_opt("memory_prompt_id", f.memory_prompt_id.clone())
        .eq_opt(
            "target_type",
            f.target_type.map(|t| target_str(t).to_owned()),
        )
        .eq_opt("team_id", f.team_id.clone())
        .eq_opt("agent_id", f.agent_id.clone())
        .eq_opt("layer", f.layer.map(|l| l.as_str().to_owned()));
    w
}

#[async_trait]
impl MemoryPromptRepository for SqlitePromptStore {
    async fn insert(&self, department: &DepartmentId, p: &MemoryPrompt) -> DomainResult<()> {
        let p = p.clone();
        self.pool
            .get(department)?
            .call(move |c| {
                c.execute(
                    "INSERT INTO memory_prompts (id, layer, status, created_at_ms, doc) VALUES (?,?,?,?,?)",
                    params![p.memory_prompt_id, p.layer.as_str(), p.status, p.created_at_ms, to_doc(&p)?],
                )
                .map(|_| ())
            })
            .await
    }

    async fn get(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<MemoryPrompt>> {
        let id = id.to_owned();
        self.pool
            .get(department)?
            .call(move |c| {
                let doc: Option<String> = c
                    .query_row("SELECT doc FROM memory_prompts WHERE id = ?", [id], |r| {
                        r.get(0)
                    })
                    .optional()?;
                doc.map(|d| from_doc(&d)).transpose()
            })
            .await
    }

    async fn update(&self, department: &DepartmentId, p: &MemoryPrompt) -> DomainResult<()> {
        let p = p.clone();
        self.pool
            .get(department)?
            .call(move |c| {
                c.execute(
                    "UPDATE memory_prompts SET status = ?, doc = ? WHERE id = ?",
                    params![p.status, to_doc(&p)?, p.memory_prompt_id],
                )
                .map(|_| ())
            })
            .await
    }

    async fn delete(&self, department: &DepartmentId, ids: &[String]) -> DomainResult<usize> {
        let ids = ids.to_vec();
        self.pool
            .get(department)?
            .call(move |c| {
                let tx = c.transaction()?;
                let mut n = 0;
                for id in &ids {
                    tx.execute(
                        "DELETE FROM prompt_settings WHERE memory_prompt_id = ?",
                        [id],
                    )?;
                    n += tx.execute("DELETE FROM memory_prompts WHERE id = ?", [id])?;
                }
                tx.commit()?;
                Ok(n)
            })
            .await
    }

    async fn list(
        &self,
        department: &DepartmentId,
        layer: Option<Layer>,
        page: Page,
        newest_first: bool,
    ) -> DomainResult<Vec<MemoryPrompt>> {
        let mut w = Where::default();
        w.eq("status", "active".to_owned())
            .eq_opt("layer", layer.map(|l| l.as_str().to_owned()));
        let order = if newest_first { "DESC" } else { "ASC" };
        self.pool
            .get(department)?
            .call(move |c| {
                let sql = format!("SELECT doc FROM memory_prompts {} ORDER BY created_at_ms {order} LIMIT ? OFFSET ?", w.sql());
                docs(query_docs(c, &sql, &w.with([Value::from(page.limit as i64), Value::from(page.offset as i64)]))?)
            })
            .await
    }

    async fn count(&self, department: &DepartmentId) -> DomainResult<usize> {
        self.pool
            .get(department)?
            .call(|c| c.query_row("SELECT COUNT(*) FROM memory_prompts", [], |r| r.get(0)))
            .await
    }

    async fn put_setting(&self, department: &DepartmentId, s: &PromptSetting) -> DomainResult<()> {
        let s = s.clone();
        self.pool
            .get(department)?
            .call(move |c| {
                c.execute(
                    "INSERT OR REPLACE INTO prompt_settings VALUES (?,?,?,?,?,?)",
                    params![
                        target_str(s.target_type),
                        s.team_id.clone().unwrap_or_default(),
                        s.agent_id.clone().unwrap_or_default(),
                        s.layer.as_str(),
                        s.memory_prompt_id,
                        s.updated_at_ms
                    ],
                )
                .map(|_| ())
            })
            .await
    }

    async fn clear_setting(
        &self,
        department: &DepartmentId,
        target: PromptTarget,
        team_id: Option<&str>,
        agent_id: Option<&str>,
        layer: Layer,
    ) -> DomainResult<usize> {
        let (t, team, agent, l) = (
            target_str(target),
            team_id.unwrap_or("").to_owned(),
            agent_id.unwrap_or("").to_owned(),
            layer.as_str(),
        );
        self.pool
            .get(department)?
            .call(move |c| c.execute("DELETE FROM prompt_settings WHERE target_type = ? AND team_id = ? AND agent_id = ? AND layer = ?", params![t, team, agent, l]))
            .await
    }

    async fn list_settings(
        &self,
        department: &DepartmentId,
        filter: &PromptSettingFilter,
        page: Page,
    ) -> DomainResult<Vec<PromptSetting>> {
        let w = setting_where(filter);
        self.pool
            .get(department)?
            .call(move |c| {
                let sql = format!(
                    "SELECT target_type, team_id, agent_id, layer, memory_prompt_id, updated_at_ms FROM prompt_settings {} ORDER BY updated_at_ms DESC LIMIT ? OFFSET ?",
                    w.sql()
                );
                let mut stmt = c.prepare(&sql)?;
                let rows = stmt
                    .query_map(params_from_iter(w.with([Value::from(page.limit as i64), Value::from(page.offset as i64)])), |r| {
                        let team: String = r.get(1)?;
                        let agent: String = r.get(2)?;
                        let layer: String = r.get(3)?;
                        Ok(PromptSetting {
                            target_type: parse_target(&r.get::<_, String>(0)?),
                            team_id: (!team.is_empty()).then_some(team),
                            agent_id: (!agent.is_empty()).then_some(agent),
                            layer: Layer::parse(&layer).unwrap_or(Layer::L1),
                            memory_prompt_id: r.get(4)?,
                            updated_at_ms: r.get(5)?,
                        })
                    })?
                    .collect();
                rows
            })
            .await
    }

    async fn append_log(&self, department: &DepartmentId, e: &PromptLogEntry) -> DomainResult<()> {
        let e = e.clone();
        self.pool
            .get(department)?
            .call(move |c| {
                c.execute(
                    "INSERT INTO prompt_logs VALUES (?,?,?,?,?,?,?,?,?)",
                    params![
                        e.log_id,
                        e.memory_prompt_id,
                        e.layer.as_str(),
                        target_str(e.target_type),
                        e.team_id,
                        e.agent_id,
                        e.action,
                        e.created_at_ms,
                        to_doc(&e)?
                    ],
                )
                .map(|_| ())
            })
            .await
    }

    async fn list_logs(
        &self,
        department: &DepartmentId,
        filter: &PromptSettingFilter,
        action: Option<&str>,
        from_ms: i64,
        to_ms: i64,
        page: Page,
    ) -> DomainResult<Vec<PromptLogEntry>> {
        let mut w = setting_where(filter);
        w.eq_opt("action", action.map(str::to_owned)).raw(
            "created_at_ms BETWEEN ? AND ?",
            [Value::from(from_ms), Value::from(to_ms)],
        );
        self.pool
            .get(department)?
            .call(move |c| {
                let sql = format!(
                    "SELECT doc FROM prompt_logs {} ORDER BY created_at_ms DESC LIMIT ? OFFSET ?",
                    w.sql()
                );
                docs(query_docs(
                    c,
                    &sql,
                    &w.with([
                        Value::from(page.limit as i64),
                        Value::from(page.offset as i64),
                    ]),
                )?)
            })
            .await
    }
}

pub struct SqliteGenerationLogStore {
    pool: Arc<DepartmentPool>,
}

impl SqliteGenerationLogStore {
    pub fn new(pool: Arc<DepartmentPool>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl GenerationLogRepository for SqliteGenerationLogStore {
    async fn append(&self, department: &DepartmentId, log: &GenerationLog) -> DomainResult<()> {
        let log = log.clone();
        self.pool
            .get(department)?
            .call(move |c| {
                let tx = c.transaction()?;
                tx.execute(
                    "INSERT INTO generation_logs (log_id, layer, status, started_at_ms, doc) VALUES (?,?,?,?,?)",
                    params![log.log_id, log.layer.as_str(), log.status, log.started_at_ms, to_doc(&log)?],
                )?;
                for r in &log.output_refs {
                    tx.execute("INSERT INTO generation_refs VALUES (?,?,?,?)", params![r.layer, r.record_id, log.log_id, log.started_at_ms])?;
                }
                tx.commit()
            })
            .await
    }

    async fn get(
        &self,
        department: &DepartmentId,
        log_id: &str,
    ) -> DomainResult<Option<GenerationLog>> {
        let id = log_id.to_owned();
        self.pool
            .get(department)?
            .call(move |c| {
                let doc: Option<String> = c
                    .query_row(
                        "SELECT doc FROM generation_logs WHERE log_id = ?",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?;
                doc.map(|d| from_doc(&d)).transpose()
            })
            .await
    }

    async fn find_by_output(
        &self,
        department: &DepartmentId,
        layer: Layer,
        memory_id: &str,
    ) -> DomainResult<Option<GenerationLog>> {
        let (l, id) = (layer.as_str(), memory_id.to_owned());
        self.pool
            .get(department)?
            .call(move |c| {
                let doc: Option<String> = c
                    .query_row(
                        "SELECT g.doc FROM generation_refs r JOIN generation_logs g ON g.log_id = r.log_id \
                         WHERE r.layer = ? AND r.record_id = ? ORDER BY r.started_at_ms DESC LIMIT 1",
                        params![l, id],
                        |r| r.get(0),
                    )
                    .optional()?;
                doc.map(|d| from_doc(&d)).transpose()
            })
            .await
    }

    async fn list(
        &self,
        department: &DepartmentId,
        filter: &GenerationLogFilter,
        limit: usize,
    ) -> DomainResult<Vec<GenerationLog>> {
        let mut w = Where::default();
        w.eq_opt("layer", filter.layer.map(|l| l.as_str().to_owned()))
            .eq_opt("status", filter.status.clone())
            .raw(
                "started_at_ms BETWEEN ? AND ?",
                [Value::from(filter.from_ms), Value::from(filter.to_ms)],
            );
        if let Some((ts, id)) = &filter.before {
            w.raw(
                "(started_at_ms < ? OR (started_at_ms = ? AND log_id < ?))",
                [Value::from(*ts), Value::from(*ts), Value::from(id.clone())],
            );
        }
        self.pool
            .get(department)?
            .call(move |c| {
                let sql = format!("SELECT doc FROM generation_logs {} ORDER BY started_at_ms DESC, log_id DESC LIMIT ?", w.sql());
                docs(query_docs(c, &sql, &w.with([Value::from(limit as i64)]))?)
            })
            .await
    }

    async fn count(&self, department: &DepartmentId) -> DomainResult<usize> {
        self.pool
            .get(department)?
            .call(|c| c.query_row("SELECT COUNT(*) FROM generation_logs", [], |r| r.get(0)))
            .await
    }

    async fn summarize(
        &self,
        department: &DepartmentId,
        from_ms: i64,
    ) -> DomainResult<Vec<GenerationCount>> {
        self.pool
            .get(department)?
            .call(move |c| {
                let mut stmt = c.prepare(
                    "SELECT layer, status, COUNT(*) FROM generation_logs \
                     WHERE started_at_ms >= ? GROUP BY layer, status ORDER BY layer, status",
                )?;
                let rows = stmt.query_map(params![from_ms], |r| {
                    Ok((r.get::<_, String>(0)?, r.get(1)?, r.get::<_, i64>(2)?))
                })?;
                let mut out = Vec::new();
                for row in rows {
                    let (layer, status, count) = row?;
                    if let Ok(layer) = Layer::parse(&layer) {
                        out.push(GenerationCount {
                            layer,
                            status,
                            count: count as usize,
                        });
                    }
                }
                Ok(out)
            })
            .await
    }
}
