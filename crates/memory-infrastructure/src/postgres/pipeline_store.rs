//! Postgres adapter for memory prompts and generation logs (per department
//! schema). Pipeline scheduling state belongs to the coordination backend.

use std::sync::Arc;

use async_trait::async_trait;
use memory_application::common::Page;
use memory_application::ports::pipeline::{
    GenerationCount, GenerationLog, GenerationLogFilter, GenerationLogRepository, MemoryPrompt,
    MemoryPromptRepository, PromptLogEntry, PromptSetting, PromptSettingFilter, PromptTarget,
};
use memory_domain::error::DomainResult;
use memory_domain::memory::Layer;
use memory_domain::meta::visibility::TeamView;
use memory_domain::scope::DepartmentId;

use super::db::{pg_err, PgDb};
use super::sql::{
    args, count, doc, docs, execute, fetch_all, fetch_optional, get, to_doc, Arg, Where,
};

pub struct PostgresPromptStore {
    db: Arc<PgDb>,
}

impl PostgresPromptStore {
    pub fn new(db: Arc<PgDb>) -> Self {
        Self { db }
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
        .eq_opt("target_type", f.target_type.map(target_str))
        .eq_opt("team_id", f.team_id.clone())
        .eq_opt("agent_id", f.agent_id.clone())
        .eq_opt("layer", f.layer.map(|l| l.as_str()));
    w
}

#[async_trait]
impl MemoryPromptRepository for PostgresPromptStore {
    async fn insert(&self, department: &DepartmentId, p: &MemoryPrompt) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("INSERT INTO {s}.memory_prompts (id, layer, status, created_at_ms, doc) VALUES (?,?,?,?,?)"),
            &[
                Arg::from(&p.memory_prompt_id),
                Arg::from(p.layer.as_str()),
                Arg::from(&p.status),
                Arg::from(p.created_at_ms),
                to_doc(p)?,
            ],
        )
        .await
        .map(|_| ())
    }

    async fn get(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<MemoryPrompt>> {
        let s = self.db.schema(department).await?;
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.memory_prompts WHERE id = ?"),
            &args![id],
        )
        .await?
        .map(|r| doc(&r, "doc"))
        .transpose()
    }

    async fn update(&self, department: &DepartmentId, p: &MemoryPrompt) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("UPDATE {s}.memory_prompts SET status = ?, doc = ? WHERE id = ?"),
            &[
                Arg::from(&p.status),
                to_doc(p)?,
                Arg::from(&p.memory_prompt_id),
            ],
        )
        .await
        .map(|_| ())
    }

    async fn delete(&self, department: &DepartmentId, ids: &[String]) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        let mut n = 0;
        for id in ids {
            execute(
                &mut tx,
                &format!("DELETE FROM {s}.prompt_settings WHERE memory_prompt_id = ?"),
                &args![id],
            )
            .await?;
            n += execute(
                &mut tx,
                &format!("DELETE FROM {s}.memory_prompts WHERE id = ?"),
                &args![id],
            )
            .await?;
        }
        tx.commit().await.map_err(pg_err)?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    async fn list(
        &self,
        department: &DepartmentId,
        layer: Option<Layer>,
        page: Page,
        newest_first: bool,
    ) -> DomainResult<Vec<MemoryPrompt>> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.eq("status", "active")
            .eq_opt("layer", layer.map(|l| l.as_str()));
        let order = if newest_first { "DESC" } else { "ASC" };
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT doc FROM {s}.memory_prompts {} ORDER BY created_at_ms {order}, id {order} LIMIT ? OFFSET ?",
                w.sql()
            ),
            &w.with(args![page.limit, page.offset]),
        )
        .await?;
        docs(&rows)
    }

    async fn count(&self, department: &DepartmentId) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        count(
            &mut *self.db.conn().await?,
            &format!("SELECT COUNT(*) FROM {s}.memory_prompts"),
            &[],
        )
        .await
    }

    async fn put_setting(&self, department: &DepartmentId, st: &PromptSetting) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!(
                "INSERT INTO {s}.prompt_settings VALUES (?,?,?,?,?,?) ON CONFLICT (target_type, team_id, agent_id, layer) \
                 DO UPDATE SET memory_prompt_id = EXCLUDED.memory_prompt_id, updated_at_ms = EXCLUDED.updated_at_ms"
            ),
            &args![
                target_str(st.target_type),
                st.team_id.clone().unwrap_or_default(),
                st.agent_id.clone().unwrap_or_default(),
                st.layer.as_str(),
                st.memory_prompt_id.as_str(),
                st.updated_at_ms
            ],
        )
        .await
        .map(|_| ())
    }

    async fn clear_setting(
        &self,
        department: &DepartmentId,
        target: PromptTarget,
        team_id: Option<&str>,
        agent_id: Option<&str>,
        layer: Layer,
    ) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        let n = execute(
            &mut *self.db.conn().await?,
            &format!("DELETE FROM {s}.prompt_settings WHERE target_type = ? AND team_id = ? AND agent_id = ? AND layer = ?"),
            &args![
                target_str(target),
                team_id.unwrap_or(""),
                agent_id.unwrap_or(""),
                layer.as_str()
            ],
        )
        .await?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    async fn list_settings(
        &self,
        department: &DepartmentId,
        filter: &PromptSettingFilter,
        page: Page,
    ) -> DomainResult<Vec<PromptSetting>> {
        let s = self.db.schema(department).await?;
        let w = setting_where(filter);
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT target_type, team_id, agent_id, layer, memory_prompt_id, updated_at_ms FROM {s}.prompt_settings {} \
                 ORDER BY updated_at_ms DESC, target_type, team_id, agent_id, layer LIMIT ? OFFSET ?",
                w.sql()
            ),
            &w.with(args![page.limit, page.offset]),
        )
        .await?;
        rows.iter()
            .map(|r| {
                let target: String = get(r, "target_type")?;
                let team: String = get(r, "team_id")?;
                let agent: String = get(r, "agent_id")?;
                let layer: String = get(r, "layer")?;
                Ok(PromptSetting {
                    target_type: parse_target(&target),
                    team_id: (!team.is_empty()).then_some(team),
                    agent_id: (!agent.is_empty()).then_some(agent),
                    layer: Layer::parse(&layer).unwrap_or(Layer::L1),
                    memory_prompt_id: get(r, "memory_prompt_id")?,
                    updated_at_ms: get(r, "updated_at_ms")?,
                })
            })
            .collect()
    }

    async fn append_log(&self, department: &DepartmentId, e: &PromptLogEntry) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        execute(
            &mut *self.db.conn().await?,
            &format!("INSERT INTO {s}.prompt_logs VALUES (?,?,?,?,?,?,?,?,?)"),
            &[
                Arg::from(&e.log_id),
                Arg::from(e.memory_prompt_id.clone()),
                Arg::from(e.layer.as_str()),
                Arg::from(target_str(e.target_type)),
                Arg::from(e.team_id.clone()),
                Arg::from(e.agent_id.clone()),
                Arg::from(&e.action),
                Arg::from(e.created_at_ms),
                to_doc(e)?,
            ],
        )
        .await
        .map(|_| ())
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
        let s = self.db.schema(department).await?;
        let mut w = setting_where(filter);
        w.eq_opt("action", action)
            .raw("created_at_ms BETWEEN ? AND ?", args![from_ms, to_ms]);
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT doc FROM {s}.prompt_logs {} ORDER BY created_at_ms DESC, id LIMIT ? OFFSET ?",
                w.sql()
            ),
            &w.with(args![page.limit, page.offset]),
        )
        .await?;
        docs(&rows)
    }
}

pub struct PostgresGenerationLogStore {
    db: Arc<PgDb>,
}

impl PostgresGenerationLogStore {
    pub fn new(db: Arc<PgDb>) -> Self {
        Self { db }
    }
}

fn log_visibility(view: &TeamView) -> (String, Vec<String>) {
    crate::visibility_sql::clause("doc->>'team_id'", "doc->>'user_id'", view)
}

#[async_trait]
impl GenerationLogRepository for PostgresGenerationLogStore {
    async fn append(&self, department: &DepartmentId, log: &GenerationLog) -> DomainResult<()> {
        let s = self.db.schema(department).await?;
        let mut tx = self.db.begin().await?;
        execute(
            &mut tx,
            &format!("INSERT INTO {s}.generation_logs (log_id, layer, status, started_at_ms, doc) VALUES (?,?,?,?,?)"),
            &[
                Arg::from(&log.log_id),
                Arg::from(log.layer.as_str()),
                Arg::from(&log.status),
                Arg::from(log.started_at_ms),
                to_doc(log)?,
            ],
        )
        .await?;
        for r in &log.output_refs {
            execute(
                &mut tx,
                &format!("INSERT INTO {s}.generation_refs (layer, record_id, log_id, started_at_ms) VALUES (?,?,?,?)"),
                &args![
                    r.layer.as_str(),
                    r.record_id.as_str(),
                    log.log_id.as_str(),
                    log.started_at_ms
                ],
            )
            .await?;
        }
        tx.commit().await.map_err(pg_err)
    }

    async fn get(
        &self,
        department: &DepartmentId,
        log_id: &str,
    ) -> DomainResult<Option<GenerationLog>> {
        let s = self.db.schema(department).await?;
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!("SELECT doc FROM {s}.generation_logs WHERE log_id = ?"),
            &args![log_id],
        )
        .await?
        .map(|r| doc(&r, "doc"))
        .transpose()
    }

    async fn find_by_output(
        &self,
        department: &DepartmentId,
        layer: Layer,
        memory_id: &str,
    ) -> DomainResult<Option<GenerationLog>> {
        let s = self.db.schema(department).await?;
        // Ties on the start time go to the latest-written reference, which
        // is what SQLite's rowid order yields.
        fetch_optional(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT g.doc FROM {s}.generation_refs r JOIN {s}.generation_logs g ON g.log_id = r.log_id \
                 WHERE r.layer = ? AND r.record_id = ? ORDER BY r.started_at_ms DESC, r.seq DESC LIMIT 1"
            ),
            &args![layer.as_str(), memory_id],
        )
        .await?
        .map(|r| doc(&r, "doc"))
        .transpose()
    }

    async fn list(
        &self,
        department: &DepartmentId,
        filter: &GenerationLogFilter,
        limit: usize,
    ) -> DomainResult<Vec<GenerationLog>> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.eq_opt("layer", filter.layer.map(|l| l.as_str()))
            .eq_opt("status", filter.status.clone())
            .raw(
                "started_at_ms BETWEEN ? AND ?",
                args![filter.from_ms, filter.to_ms],
            );
        if let Some((ts, id)) = &filter.before {
            w.raw(
                "(started_at_ms < ? OR (started_at_ms = ? AND log_id < ?))",
                args![*ts, *ts, id],
            );
        }
        if let Some(view) = &filter.visible {
            let (sql, params) = log_visibility(view);
            w.raw(&sql, params.into_iter().map(Arg::from));
        }
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT doc FROM {s}.generation_logs {} ORDER BY started_at_ms DESC, log_id DESC LIMIT ?",
                w.sql()
            ),
            &w.with(args![limit]),
        )
        .await?;
        docs(&rows)
    }

    async fn count(&self, department: &DepartmentId) -> DomainResult<usize> {
        let s = self.db.schema(department).await?;
        count(
            &mut *self.db.conn().await?,
            &format!("SELECT COUNT(*) FROM {s}.generation_logs"),
            &[],
        )
        .await
    }

    async fn summarize(
        &self,
        department: &DepartmentId,
        from_ms: i64,
        visible: Option<&TeamView>,
    ) -> DomainResult<Vec<GenerationCount>> {
        let s = self.db.schema(department).await?;
        let mut w = Where::default();
        w.raw("started_at_ms >= ?", args![from_ms]);
        if let Some(view) = visible {
            let (sql, params) = log_visibility(view);
            w.raw(&sql, params.into_iter().map(Arg::from));
        }
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT layer, status, COUNT(*) AS n FROM {s}.generation_logs {} \
                 GROUP BY layer, status ORDER BY layer, status",
                w.sql()
            ),
            &w.params,
        )
        .await?;
        let mut out = Vec::new();
        for row in &rows {
            let layer: String = get(row, "layer")?;
            let n: i64 = get(row, "n")?;
            if let Ok(layer) = Layer::parse(&layer) {
                out.push(GenerationCount {
                    layer,
                    status: get(row, "status")?,
                    count: n as usize,
                });
            }
        }
        Ok(out)
    }
}
