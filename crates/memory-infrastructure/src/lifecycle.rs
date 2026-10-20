//! Department destruction composed of independent cleanup steps (storage,
//! files, scheduling state), so each backend contributes its own.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use memory_application::admin::DepartmentLifecycle;
use memory_application::ports::pipeline::PipelineStateRepository;
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::scope::DepartmentId;
use serde_json::{json, Map, Value};

use crate::sqlite::db::DepartmentPool;

/// Runs every step and merges their reports into one JSON object.
pub struct CompositeLifecycle {
    steps: Vec<(&'static str, Arc<dyn DepartmentLifecycle>)>,
}

impl CompositeLifecycle {
    pub fn new(steps: Vec<(&'static str, Arc<dyn DepartmentLifecycle>)>) -> Self {
        Self { steps }
    }
}

#[async_trait]
impl DepartmentLifecycle for CompositeLifecycle {
    async fn destroy(&self, department: &DepartmentId) -> DomainResult<Value> {
        let mut report = Map::new();
        for (name, step) in &self.steps {
            report.insert((*name).to_owned(), step.destroy(department).await?);
        }
        Ok(Value::Object(report))
    }
}

/// Deletes `<data>/departments/<id>/` (files, and the SQLite database when
/// SQLite is the storage backend).
pub struct LocalDirCleanup {
    data_dir: PathBuf,
    sqlite: Option<Arc<DepartmentPool>>,
}

impl LocalDirCleanup {
    pub fn new(data_dir: PathBuf, sqlite: Option<Arc<DepartmentPool>>) -> Self {
        Self { data_dir, sqlite }
    }
}

#[async_trait]
impl DepartmentLifecycle for LocalDirCleanup {
    async fn destroy(&self, department: &DepartmentId) -> DomainResult<Value> {
        if let Some(pool) = &self.sqlite {
            pool.evict(department);
        }
        let dir = self.data_dir.join("departments").join(department.as_str());
        let existed = dir.exists();
        if existed {
            tokio::fs::remove_dir_all(&dir).await.map_err(|e| {
                DomainError::unavailable(format!("failed to remove department data: {e}"))
            })?;
        }
        Ok(json!({"removed": existed}))
    }
}

/// Forgets pipeline timers and cursors of the department.
pub struct StateCleanup {
    state: Arc<dyn PipelineStateRepository>,
}

impl StateCleanup {
    pub fn new(state: Arc<dyn PipelineStateRepository>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl DepartmentLifecycle for StateCleanup {
    async fn destroy(&self, department: &DepartmentId) -> DomainResult<Value> {
        self.state.delete_department(department).await?;
        Ok(json!({"removed": true}))
    }
}
