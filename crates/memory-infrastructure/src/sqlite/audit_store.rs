//! The audit log on SQLite: its own database, `audit.db` in the data
//! directory, kept apart from department data (a destroyed department's
//! events stay) and from `system.db` (which exists only with local
//! coordination).

use std::sync::Arc;

use async_trait::async_trait;
use memory_application::common::Page;
use memory_application::ports::audit::{AuditEvent, AuditFilter, AuditLog};
use memory_domain::error::DomainResult;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection};

use super::db::Db;
use super::sql::{from_doc, to_doc, Where};

const AUDIT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS audit_events (
    event_id TEXT PRIMARY KEY, at_ms INTEGER NOT NULL, kind TEXT NOT NULL, department TEXT,
    doc TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_audit_at ON audit_events(at_ms);
CREATE INDEX IF NOT EXISTS ix_audit_department ON audit_events(department, at_ms);
"#;

pub fn migrate_audit(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(AUDIT_SCHEMA)
}

pub struct SqliteAuditLog {
    db: Arc<Db>,
}

impl SqliteAuditLog {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AuditLog for SqliteAuditLog {
    async fn record(&self, event: &AuditEvent) -> DomainResult<()> {
        let e = event.clone();
        self.db
            .call(move |c| {
                c.execute(
                    "INSERT OR IGNORE INTO audit_events (event_id, at_ms, kind, department, doc) VALUES (?,?,?,?,?)",
                    params![e.event_id, e.at_ms, e.kind.as_str(), e.department, to_doc(&e)?],
                )
                .map(|_| ())
            })
            .await
    }

    async fn list(&self, filter: &AuditFilter, page: Page) -> DomainResult<Vec<AuditEvent>> {
        let mut w = Where::default();
        w.eq_opt("department", filter.department.clone())
            .eq_opt("kind", filter.kind.map(|k| k.as_str().to_owned()));
        if let Some(since) = filter.since_ms {
            w.raw("at_ms >= ?", [Value::from(since)]);
        }
        let sql = format!(
            "SELECT doc FROM audit_events {} ORDER BY at_ms DESC, event_id DESC LIMIT ? OFFSET ?",
            w.sql()
        );
        let params = w.with([
            Value::from(page.limit as i64),
            Value::from(page.offset as i64),
        ]);
        self.db
            .call(move |c| {
                let mut stmt = c.prepare(&sql)?;
                let rows = stmt.query_map(params_from_iter(params), |r| r.get::<_, String>(0))?;
                rows.map(|d| from_doc(&d?)).collect()
            })
            .await
    }

    async fn prune(&self, before_ms: i64) -> DomainResult<u64> {
        self.db
            .call(move |c| {
                c.execute("DELETE FROM audit_events WHERE at_ms < ?", [before_ms])
                    .map(|n| n as u64)
            })
            .await
    }
}
