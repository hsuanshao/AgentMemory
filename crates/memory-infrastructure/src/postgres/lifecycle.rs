//! Department existence and destruction for the Postgres backend: the department schema goes
//! with everything in it, and so does its registry row. Local files are a
//! separate step (`LocalDirCleanup`) composed in by the server.

use std::sync::Arc;

use async_trait::async_trait;
use memory_application::admin::{DepartmentDirectory, DepartmentLifecycle};
use memory_domain::error::DomainResult;
use memory_domain::scope::DepartmentId;
use serde_json::{json, Value};

use super::db::PgDb;

pub struct PostgresCleanup {
    db: Arc<PgDb>,
}

impl PostgresCleanup {
    pub fn new(db: Arc<PgDb>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl DepartmentLifecycle for PostgresCleanup {
    async fn destroy(&self, department: &DepartmentId) -> DomainResult<Value> {
        let removed = self.db.drop_department(department).await?;
        Ok(json!({"removed": removed}))
    }
}

#[async_trait]
impl DepartmentDirectory for PgDb {
    async fn exists(&self, department: &DepartmentId) -> DomainResult<bool> {
        self.is_registered(department).await
    }
}
