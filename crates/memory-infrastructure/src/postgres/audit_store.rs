//! The audit log on Postgres: `agent_memory.audit_events` in the registry
//! schema, beside the departments it records (a destroyed department's
//! events stay).

use std::sync::Arc;

use async_trait::async_trait;
use memory_application::common::Page;
use memory_application::ports::audit::{AuditEvent, AuditFilter, AuditLog};
use memory_domain::error::DomainResult;

use super::db::{quote_ident, PgDb};
use super::schema::REGISTRY_SCHEMA;
use super::sql::{docs, execute, fetch_all, to_doc, Arg, Where};

pub struct PostgresAuditLog {
    db: Arc<PgDb>,
    table: String,
}

impl PostgresAuditLog {
    pub fn new(db: Arc<PgDb>) -> Self {
        Self {
            db,
            table: format!("{}.audit_events", quote_ident(REGISTRY_SCHEMA)),
        }
    }
}

#[async_trait]
impl AuditLog for PostgresAuditLog {
    async fn record(&self, e: &AuditEvent) -> DomainResult<()> {
        execute(
            &mut *self.db.conn().await?,
            &format!(
                "INSERT INTO {} (event_id, at_ms, kind, department, doc) VALUES (?,?,?,?,?) ON CONFLICT (event_id) DO NOTHING",
                self.table
            ),
            &[
                Arg::from(&e.event_id),
                Arg::from(e.at_ms),
                Arg::from(e.kind.as_str()),
                Arg::Text(e.department.clone()),
                to_doc(e)?,
            ],
        )
        .await
        .map(|_| ())
    }

    async fn list(&self, filter: &AuditFilter, page: Page) -> DomainResult<Vec<AuditEvent>> {
        let mut w = Where::default();
        w.eq_opt("department", filter.department.clone())
            .eq_opt("kind", filter.kind.map(|k| k.as_str()));
        if let Some(since) = filter.since_ms {
            w.raw("at_ms >= ?", [Arg::from(since)]);
        }
        let rows = fetch_all(
            &mut *self.db.conn().await?,
            &format!(
                "SELECT doc FROM {} {} ORDER BY at_ms DESC, event_id DESC LIMIT ? OFFSET ?",
                self.table,
                w.sql()
            ),
            &w.with([Arg::from(page.limit as i64), Arg::from(page.offset as i64)]),
        )
        .await?;
        docs(&rows)
    }

    async fn prune(&self, before_ms: i64) -> DomainResult<u64> {
        execute(
            &mut *self.db.conn().await?,
            &format!("DELETE FROM {} WHERE at_ms < ?", self.table),
            &[Arg::from(before_ms)],
        )
        .await
    }
}
