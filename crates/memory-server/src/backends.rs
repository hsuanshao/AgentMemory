//! Backend selection: which adapters implement storage and coordination.
//! Each backend contributes one arm here; nothing else in the composition
//! root knows which one is active.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use memory_application::admin::{DepartmentDirectory, DepartmentLifecycle};
use memory_application::ports::audit::{AuditLog, FailureCounter};
use memory_application::ports::coordination::KeyLock;
use memory_application::ports::memory::{
    AtomRepository, ConversationRepository, ProfileVersionStore, ScopeMigrationStore,
};
use memory_application::ports::meta::MetaStore;
use memory_application::ports::pipeline::{
    GenerationLogRepository, MemoryPromptRepository, PipelineStateRepository,
};
use memory_application::ports::skill::{SkillBufferStore, SkillRepository};
use memory_infrastructure::lifecycle::{CompositeLifecycle, LocalDirCleanup};
use memory_infrastructure::local_lock::LocalKeyLock;
use memory_infrastructure::local_throttle::LocalFailureCounter;
use memory_infrastructure::postgres::audit_store::PostgresAuditLog;
use memory_infrastructure::postgres::db::PgDb;
use memory_infrastructure::postgres::lifecycle::PostgresCleanup;
use memory_infrastructure::postgres::memory_store::PostgresMemoryStore;
use memory_infrastructure::postgres::meta_store::PostgresMetaStore;
use memory_infrastructure::postgres::pipeline_store::{
    PostgresGenerationLogStore, PostgresPromptStore,
};
use memory_infrastructure::postgres::skill_store::PostgresSkillStore;
use memory_infrastructure::redis::{
    self as redis_backend, KeySpace, RedisFailureCounter, RedisKeyLock, RedisPipelineState,
    RedisQueue,
};
use memory_infrastructure::sqlite::audit_store::{migrate_audit, SqliteAuditLog};
use memory_infrastructure::sqlite::db::{Db, DepartmentPool};
use memory_infrastructure::sqlite::memory_store::SqliteMemoryStore;
use memory_infrastructure::sqlite::meta_store::SqliteMetaStore;
use memory_infrastructure::sqlite::pipeline_store::{
    SqliteGenerationLogStore, SqlitePipelineState, SqlitePromptStore,
};
use memory_infrastructure::sqlite::schema;
use memory_infrastructure::sqlite::skill_store::SqliteSkillStore;
use memory_infrastructure::worker::{LocalQueue, QueueBackend};

use crate::config::Config;

/// Every persistence port, from one backend.
pub struct Storage {
    pub conversations: Arc<dyn ConversationRepository>,
    pub atoms: Arc<dyn AtomRepository>,
    pub profiles: Arc<dyn ProfileVersionStore>,
    /// Upgrade path for the legacy `default` buckets.
    pub scope_migration: Arc<dyn ScopeMigrationStore>,
    pub meta: Arc<dyn MetaStore>,
    pub skills: Arc<dyn SkillRepository>,
    pub skill_buffers: Arc<dyn SkillBufferStore>,
    pub prompts: Arc<dyn MemoryPromptRepository>,
    pub generation_logs: Arc<dyn GenerationLogRepository>,
    /// Removes the department's stored data (and its local files).
    pub cleanup: Arc<dyn DepartmentLifecycle>,
    /// Which departments have storage.
    pub departments: Arc<dyn DepartmentDirectory>,
    /// Credential failures and issued keys, across departments.
    pub audit: Arc<dyn AuditLog>,
    pub name: &'static str,
    pub fts_available: bool,
    /// How vector search is executed, for `/health`.
    pub vector_index: String,
}

/// Scheduling state, the job queue and locks.
pub struct Coordination {
    pub state: Arc<dyn PipelineStateRepository>,
    pub queue: Arc<dyn QueueBackend>,
    /// For pipeline state read-modify-write.
    pub state_locks: Arc<dyn KeyLock>,
    /// For per-job exclusivity in the worker (a separate lock space).
    pub job_locks: Arc<dyn KeyLock>,
    /// Credential failures per client address.
    pub failures: Arc<dyn FailureCounter>,
    pub name: &'static str,
}

pub async fn build_storage(
    config: &Config,
    data_dir: &Path,
    vector_dims: Option<usize>,
) -> Result<Storage> {
    match config.storage.backend.as_str() {
        "sqlite" => sqlite_storage(config, data_dir, vector_dims),
        "postgres" => postgres_storage(config, data_dir, vector_dims).await,
        other => bail!("storage backend `{other}` is not available in this build"),
    }
}

fn sqlite_storage(config: &Config, data_dir: &Path, vector_dims: Option<usize>) -> Result<Storage> {
    let pool = Arc::new(DepartmentPool::new(
        data_dir.to_path_buf(),
        config.data.max_open_departments,
    ));
    let memory = Arc::new(SqliteMemoryStore::new(Arc::clone(&pool), vector_dims));
    let vector_index = memory.vector_index().to_owned();
    let skills = Arc::new(SqliteSkillStore::new(Arc::clone(&pool)));
    let audit = Db::open(&data_dir.join("audit.db"), migrate_audit).context("audit.db")?;
    Ok(Storage {
        conversations: memory.clone(),
        atoms: memory.clone(),
        profiles: memory.clone(),
        scope_migration: memory,
        meta: Arc::new(SqliteMetaStore::new(Arc::clone(&pool))),
        skills: skills.clone(),
        skill_buffers: skills,
        prompts: Arc::new(SqlitePromptStore::new(Arc::clone(&pool))),
        generation_logs: Arc::new(SqliteGenerationLogStore::new(Arc::clone(&pool))),
        fts_available: pool.fts_available(),
        vector_index,
        cleanup: Arc::new(LocalDirCleanup::new(
            data_dir.to_path_buf(),
            Some(Arc::clone(&pool)),
        )),
        departments: pool,
        audit: Arc::new(SqliteAuditLog::new(Arc::new(audit))),
        name: "sqlite",
    })
}

/// One shared pool, one schema per department. Files (skill resources,
/// session objects) still live under the local data directory, so department
/// cleanup drops the schema and then the directory.
async fn postgres_storage(
    config: &Config,
    data_dir: &Path,
    vector_dims: Option<usize>,
) -> Result<Storage> {
    let pg = &config.storage.postgres;
    let db = Arc::new(
        PgDb::connect(&pg.url, pg.max_connections, vector_dims)
            .await
            .context("storage backend `postgres`")?,
    );
    let memory = Arc::new(PostgresMemoryStore::new(Arc::clone(&db)));
    let skills = Arc::new(PostgresSkillStore::new(Arc::clone(&db)));
    let cleanup = CompositeLifecycle::new(vec![
        (
            "postgres",
            Arc::new(PostgresCleanup::new(Arc::clone(&db))) as _,
        ),
        (
            "files",
            Arc::new(LocalDirCleanup::new(data_dir.to_path_buf(), None)) as _,
        ),
    ]);
    Ok(Storage {
        conversations: memory.clone(),
        atoms: memory.clone(),
        profiles: memory.clone(),
        scope_migration: memory,
        meta: Arc::new(PostgresMetaStore::new(Arc::clone(&db))),
        skills: skills.clone(),
        skill_buffers: skills,
        prompts: Arc::new(PostgresPromptStore::new(Arc::clone(&db))),
        generation_logs: Arc::new(PostgresGenerationLogStore::new(Arc::clone(&db))),
        fts_available: true,
        vector_index: db.vector_index().into(),
        cleanup: Arc::new(cleanup),
        audit: Arc::new(PostgresAuditLog::new(Arc::clone(&db))),
        departments: db,
        name: "postgres",
    })
}

pub async fn build_coordination(config: &Config, data_dir: &Path) -> Result<Coordination> {
    match config.coordination.backend.as_str() {
        "local" => {
            let system_db = Arc::new(Db::open(
                &data_dir.join("system.db"),
                schema::migrate_system,
            )?);
            Ok(Coordination {
                state: Arc::new(SqlitePipelineState::new(system_db)),
                queue: LocalQueue::new(),
                state_locks: Arc::new(LocalKeyLock::default()),
                job_locks: Arc::new(LocalKeyLock::default()),
                failures: Arc::new(LocalFailureCounter::default()),
                name: "local",
            })
        }
        "redis" => {
            let redis = &config.coordination.redis;
            // Fail the boot rather than serve without shared coordination.
            let conn = redis_backend::connect(&redis.url)
                .await
                .context("coordination backend `redis`: cannot reach coordination.redis.url")?;
            let keys = KeySpace::new(&redis.key_prefix);
            redis_backend::migrate::migrate(&conn, &keys)
                .await
                .context("migrating the coordination data in redis")?;
            let ttl = Duration::from_millis(redis.lock_ttl_ms);
            Ok(Coordination {
                state: Arc::new(RedisPipelineState::new(conn.clone(), &keys)),
                queue: Arc::new(RedisQueue::new(
                    conn.clone(),
                    &keys,
                    Duration::from_millis(redis.queue_lease_ms),
                )),
                state_locks: Arc::new(RedisKeyLock::new(
                    conn.clone(),
                    keys.child("lock:state"),
                    ttl,
                )),
                job_locks: Arc::new(RedisKeyLock::new(conn.clone(), keys.child("lock:job"), ttl)),
                failures: Arc::new(RedisFailureCounter::new(conn, &keys)),
                name: "redis",
            })
        }
        other => bail!("coordination backend `{other}` is not available in this build"),
    }
}
