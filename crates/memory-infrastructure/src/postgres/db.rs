//! Connection pool, error mapping and lazily migrated per-department schemas.
//!
//! Every department lives in its own schema, `am_<16 hex of sha256(id)>`:
//! department ids are arbitrary strings (up to 256 bytes), schema names must
//! be short identifiers, and a hash keeps the mapping deterministic across
//! nodes. The registry table `agent_memory.departments` records which
//! department owns which schema, so a (practically impossible) hash collision
//! surfaces as a unique violation instead of two tenants sharing tables.
//! All SQL names tables fully qualified (`"am_…".table`) — `search_path` is
//! per connection and a pool cannot be trusted to carry it.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use memory_domain::error::{DomainError, DomainResult};
use memory_domain::profile::{import_legacy_files, LegacyProfileFile};
use memory_domain::scope::DepartmentId;
use sha2::{Digest, Sha256};
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions};
use sqlx::{Connection, Postgres, Transaction};
use tracing::{info, warn};

use super::schema::{self, HNSW_MAX_DIMS, REGISTRY_SCHEMA};
use super::sql::{
    args, count, execute, execute_script, fetch_all, fetch_optional, get, to_doc, Arg,
};
use crate::profile_versions::legacy_scene_id;

/// How long a request waits for a pooled connection (and how long start-up
/// waits for the first one) before reporting storage as unavailable.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

pub fn pg_err(e: sqlx::Error) -> DomainError {
    match &e {
        // Class 23 = integrity constraint violation, like SQLite's
        // `ConstraintViolation`.
        sqlx::Error::Database(db) if db.code().is_some_and(|c| c.starts_with("23")) => {
            DomainError::conflict("duplicate_entry", e.to_string())
        }
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => {
            DomainError::unavailable(format!("postgres unreachable: {e}"))
        }
        _ => DomainError::unavailable(format!("storage error: {e}")),
    }
}

/// Double-quotes an identifier.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The schema name owned by `department`.
pub fn schema_name(department: &DepartmentId) -> String {
    let digest = Sha256::digest(department.as_str().as_bytes());
    format!("am_{}", &hex::encode(digest)[..16])
}

/// A migrated department schema; displays as its quoted name for use in SQL.
#[derive(Debug, Clone)]
pub struct Schema(Arc<str>);

impl fmt::Display for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Takes a transaction-scoped advisory lock named `key`.
pub async fn xact_lock(tx: &mut Transaction<'_, Postgres>, key: &str) -> DomainResult<()> {
    execute(
        tx,
        "SELECT pg_advisory_xact_lock(hashtextextended(?, 0))",
        &args![key],
    )
    .await
    .map(|_| ())
}

/// The shared pool plus the set of department schemas known to be migrated.
pub struct PgDb {
    pool: PgPool,
    dims: Option<usize>,
    /// pgvector ≥ 0.8 can keep scanning HNSW until enough rows pass the
    /// scope filter; older versions may return fewer than `limit` rows.
    iterative_scan: bool,
    ready: RwLock<HashMap<DepartmentId, Schema>>,
    /// Serialises first-use migrations within this process (the advisory
    /// lock serialises them across processes).
    migrating: tokio::sync::Mutex<()>,
}

impl PgDb {
    /// Connects (failing fast when the server is unreachable), installs
    /// pgvector when embeddings are on, and creates the registry.
    pub async fn connect(
        url: &str,
        max_connections: u32,
        dims: Option<usize>,
    ) -> DomainResult<Self> {
        let options = PgConnectOptions::from_str(url)
            .map_err(|e| DomainError::invalid(format!("storage.postgres.url: {e}")))?;
        let target = format!(
            "{}:{}/{}",
            options.get_host(),
            options.get_port(),
            options.get_database().unwrap_or("")
        );
        let unreachable = |e: &dyn fmt::Display| {
            DomainError::unavailable(format!("cannot connect to postgres at {target}: {e}"))
        };
        // One direct attempt first: the pool retries refused connections
        // until its acquire timeout, which would only delay a clear error.
        let probe = tokio::time::timeout(ACQUIRE_TIMEOUT, PgConnection::connect_with(&options))
            .await
            .map_err(|e| unreachable(&e))?
            .map_err(|e| unreachable(&e))?;
        probe.close().await.map_err(pg_err)?;
        let pool = PgPoolOptions::new()
            .max_connections(max_connections.max(1))
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect_with(options)
            .await
            .map_err(|e| unreachable(&e))?;
        let mut tx = pool.begin().await.map_err(pg_err)?;
        xact_lock(&mut tx, "agent_memory:bootstrap").await?;
        if dims.is_some() {
            execute(&mut tx, "CREATE EXTENSION IF NOT EXISTS vector", &[])
                .await
                .map_err(|e| {
                    DomainError::unavailable(format!(
                        "pgvector is required when embeddings are enabled ({}); install it or run `CREATE EXTENSION vector` as a privileged user",
                        e.detail
                    ))
                })?;
        }
        migrate(&mut tx).await?;
        let version: Option<String> = match fetch_optional(
            &mut tx,
            "SELECT extversion FROM pg_extension WHERE extname = 'vector'",
            &[],
        )
        .await?
        {
            Some(row) => Some(get(&row, "extversion")?),
            None => None,
        };
        tx.commit().await.map_err(pg_err)?;
        info!(%target, pgvector = ?version, "connected to postgres");
        Ok(Self {
            pool,
            dims,
            iterative_scan: version.as_deref().is_some_and(supports_iterative_scan),
            ready: RwLock::new(HashMap::new()),
            migrating: tokio::sync::Mutex::new(()),
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn begin(&self) -> DomainResult<Transaction<'static, Postgres>> {
        self.pool.begin().await.map_err(pg_err)
    }

    /// A pooled connection for a single statement or a short sequence.
    pub async fn conn(&self) -> DomainResult<PoolConnection<Postgres>> {
        self.pool.acquire().await.map_err(pg_err)
    }

    pub fn dims(&self) -> Option<usize> {
        self.dims
    }

    pub fn iterative_scan(&self) -> bool {
        self.iterative_scan
    }

    /// `pgvector-hnsw`, `pgvector-exact` (too many dimensions for HNSW) or
    /// `none`, for `/health`.
    pub fn vector_index(&self) -> &'static str {
        match self.dims {
            Some(d) if d <= HNSW_MAX_DIMS => "pgvector-hnsw",
            Some(_) => "pgvector-exact",
            None => "none",
        }
    }

    fn cached(&self, department: &DepartmentId) -> Option<Schema> {
        self.ready.read().ok()?.get(department).cloned()
    }

    /// The department's schema, created and migrated on first use.
    pub async fn schema(&self, department: &DepartmentId) -> DomainResult<Schema> {
        if let Some(s) = self.cached(department) {
            return Ok(s);
        }
        let _guard = self.migrating.lock().await;
        if let Some(s) = self.cached(department) {
            return Ok(s);
        }
        let name = schema_name(department);
        let quoted = quote_ident(&name);
        let mut tx = self.pool.begin().await.map_err(pg_err)?;
        xact_lock(&mut tx, &format!("agent_memory:migrate:{name}")).await?;
        let registry = quote_ident(REGISTRY_SCHEMA);
        execute(
            &mut tx,
            &format!("INSERT INTO {registry}.departments (department_id, schema_name) VALUES (?, ?) ON CONFLICT (department_id) DO NOTHING"),
            &args![department.as_str(), name.as_str()],
        )
        .await?;
        execute_script(&mut tx, &schema::department_ddl(&quoted)).await?;
        if let Some(dims) = self.dims {
            migrate_embedding(&mut tx, &quoted, dims).await?;
        }
        tx.commit().await.map_err(pg_err)?;
        let schema = Schema(quoted.into());
        if let Ok(mut ready) = self.ready.write() {
            ready.insert(department.clone(), schema.clone());
        }
        Ok(schema)
    }

    /// Drops the department schema and its registry row. Returns whether the
    /// department was registered.
    pub async fn drop_department(&self, department: &DepartmentId) -> DomainResult<bool> {
        let _guard = self.migrating.lock().await;
        if let Ok(mut ready) = self.ready.write() {
            ready.remove(department);
        }
        let name = schema_name(department);
        let mut tx = self.pool.begin().await.map_err(pg_err)?;
        xact_lock(&mut tx, &format!("agent_memory:migrate:{name}")).await?;
        execute(
            &mut tx,
            &format!("DROP SCHEMA IF EXISTS {} CASCADE", quote_ident(&name)),
            &[],
        )
        .await?;
        let registry = quote_ident(REGISTRY_SCHEMA);
        let removed = execute(
            &mut tx,
            &format!("DELETE FROM {registry}.departments WHERE department_id = ?"),
            &args![department.as_str()],
        )
        .await?;
        tx.commit().await.map_err(pg_err)?;
        Ok(removed > 0)
    }

    /// Departments registered in the database.
    pub async fn known_departments(&self) -> DomainResult<Vec<String>> {
        let registry = quote_ident(REGISTRY_SCHEMA);
        let rows = super::sql::fetch_all(
            &mut *self.conn().await?,
            &format!("SELECT department_id FROM {registry}.departments ORDER BY department_id"),
            &[],
        )
        .await?;
        rows.iter().map(|r| get(r, "department_id")).collect()
    }
}

/// Brings the registry and every registered department schema to
/// [`schema::SCHEMA_VERSION`], recording each step in
/// `agent_memory.schema_migrations`. Runs under the bootstrap lock, so one
/// replica migrates while the others wait.
async fn migrate(tx: &mut Transaction<'_, Postgres>) -> DomainResult<()> {
    let registry = quote_ident(REGISTRY_SCHEMA);
    execute_script(tx, &schema::migrations_ddl(&registry)).await?;
    let applied = match fetch_optional(
        tx,
        &format!("SELECT max(version) AS v FROM {registry}.schema_migrations"),
        &[],
    )
    .await?
    {
        Some(row) => get::<Option<i64>>(&row, "v")?.unwrap_or(0),
        None => 0,
    };
    // A database written by a newer memory-server: running on it would
    // ignore what that version stores (and lose what is written meanwhile
    // on the next upgrade, whose migrations are already recorded).
    if applied > schema::SCHEMA_VERSION {
        return Err(DomainError::unavailable(format!(
            "the postgres schema is at version {applied}, newer than this memory-server knows \
             ({}): run the newer version, or restore the backup taken before the upgrade \
             (docs/UPGRADE.md, Roll back)",
            schema::SCHEMA_VERSION
        )));
    }
    if applied < 1 {
        rename_instances(tx, &registry).await?;
    }
    execute_script(tx, &schema::registry_ddl(&registry)).await?;
    if applied < 2 {
        update_departments(tx, &registry, |s| {
            vec![(
                "meta_team_members",
                format!(
                    "UPDATE {s}.meta_team_members SET doc = jsonb_set(doc, '{{role}}', '\"member\"') WHERE doc->>'role' = 'reviewer'"
                ),
            )]
        })
        .await?;
    }
    if applied < 3 {
        import_profile_files(tx, &registry).await?;
    }
    for (version, name) in schema::MIGRATIONS.iter().filter(|(v, _)| *v > applied) {
        execute(
            tx,
            &format!("INSERT INTO {registry}.schema_migrations (version, name) VALUES (?, ?)"),
            &args![*version, *name],
        )
        .await?;
        info!(version, name, "migrated postgres schema");
    }
    Ok(())
}

/// Migration 1: the `instances` registry becomes `departments`, and prompt
/// settings that targeted the instance now target the department.
async fn rename_instances(tx: &mut Transaction<'_, Postgres>, registry: &str) -> DomainResult<()> {
    let old = fetch_optional(
        tx,
        &format!("SELECT 1 AS x WHERE to_regclass('{REGISTRY_SCHEMA}.instances') IS NOT NULL AND to_regclass('{REGISTRY_SCHEMA}.departments') IS NULL"),
        &[],
    )
    .await?;
    if old.is_none() {
        return Ok(());
    }
    execute_script(
        tx,
        &format!(
            "ALTER TABLE {registry}.instances RENAME COLUMN instance_id TO department_id;
             ALTER TABLE {registry}.instances RENAME TO departments;
             ALTER INDEX IF EXISTS {registry}.instances_pkey RENAME TO departments_pkey;
             ALTER INDEX IF EXISTS {registry}.instances_schema_name_key RENAME TO departments_schema_name_key;"
        ),
    )
    .await?;
    let departments = update_departments(tx, registry, |s| {
        vec![
            ("prompt_settings", format!("UPDATE {s}.prompt_settings SET target_type = 'department' WHERE target_type = 'instance'")),
            ("prompt_logs", format!("UPDATE {s}.prompt_logs SET target_type = 'department' WHERE target_type = 'instance'")),
            // The operator's quota replaces the default seeded under the new name.
            ("meta_config_params", format!(
                "DELETE FROM {s}.meta_config_params n USING {s}.meta_config_params o
                 WHERE o.module = 'quota' AND o.param_name IN ('max_users_per_instance', 'max_teams_per_instance')
                   AND n.scope = o.scope AND n.user_id = o.user_id AND n.module = o.module
                   AND n.param_name = replace(o.param_name, '_per_instance', '_per_department');
                 UPDATE {s}.meta_config_params SET param_name = replace(param_name, '_per_instance', '_per_department')
                 WHERE module = 'quota' AND param_name IN ('max_users_per_instance', 'max_teams_per_instance');"
            )),
        ]
    })
    .await?;
    info!(
        departments,
        "renamed the postgres instance registry to departments"
    );
    Ok(())
}

/// Migration 3: every stored file version of each department becomes a
/// scene or persona version. `ON CONFLICT DO NOTHING` keeps a repeated run
/// harmless.
async fn import_profile_files(
    tx: &mut Transaction<'_, Postgres>,
    registry: &str,
) -> DomainResult<()> {
    let rows = fetch_all(
        tx,
        &format!("SELECT schema_name FROM {registry}.departments"),
        &[],
    )
    .await?;
    for row in &rows {
        let s = quote_ident(&get::<String>(row, "schema_name")?);
        let exists = fetch_optional(
            tx,
            "SELECT 1 AS x WHERE to_regclass(?) IS NOT NULL",
            &args![format!("{s}.profile_files")],
        )
        .await?;
        if exists.is_none() {
            continue;
        }
        // The department's tables are otherwise created on first use.
        execute_script(tx, &schema::department_ddl(&s)).await?;
        let files = fetch_all(
            tx,
            &format!("SELECT team_id, agent_id, path, version, content, created_at_ms, updated_at_ms FROM {s}.profile_files"),
            &[],
        )
        .await?
        .iter()
        .map(|r| -> DomainResult<LegacyProfileFile> {
            Ok(LegacyProfileFile {
                team_id: get(r, "team_id")?,
                agent_id: get(r, "agent_id")?,
                path: get(r, "path")?,
                version: u32::try_from(get::<i64>(r, "version")?).unwrap_or_default(),
                content: get(r, "content")?,
                created_at_ms: get(r, "created_at_ms")?,
                updated_at_ms: get(r, "updated_at_ms")?,
            })
        })
        .collect::<DomainResult<Vec<_>>>()?;
        let (scenes, personas) = import_legacy_files(files, legacy_scene_id);
        for i in &scenes {
            let v = &i.scene;
            execute(
                tx,
                &format!("INSERT INTO {s}.scenes (id, team_id, agent_id, created_at_ms) VALUES (?,?,?,?) ON CONFLICT DO NOTHING"),
                &args![v.scene_id.as_str(), i.team_id.as_str(), i.agent_id.as_str(), v.scene_created_at.timestamp_millis()],
            )
            .await?;
            execute(
                tx,
                &format!("INSERT INTO {s}.scene_versions (scene_id, version, op, slug, summary, heat, body, attrs, parents, author, created_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT DO NOTHING"),
                &[
                    Arg::from(v.scene_id.as_str()),
                    Arg::from(v.version),
                    Arg::from(v.op.as_str()),
                    Arg::from(v.slug.as_str()),
                    Arg::from(v.summary.as_str()),
                    Arg::from(v.heat),
                    Arg::from(v.body.as_str()),
                    to_doc(&v.attrs)?,
                    to_doc(&v.parents)?,
                    Arg::from(v.author.clone()),
                    Arg::from(v.created_at.timestamp_millis()),
                ],
            )
            .await?;
        }
        for i in &personas {
            let v = &i.persona;
            execute(
                tx,
                &format!("INSERT INTO {s}.persona_versions (team_id, agent_id, version, op, body, attrs, author, created_at_ms) VALUES (?,?,?,?,?,?,?,?) ON CONFLICT DO NOTHING"),
                &[
                    Arg::from(i.team_id.as_str()),
                    Arg::from(i.agent_id.as_str()),
                    Arg::from(v.version),
                    Arg::from(v.op.as_str()),
                    Arg::from(v.body.as_str()),
                    to_doc(&v.attrs)?,
                    Arg::from(v.author.clone()),
                    Arg::from(v.created_at.timestamp_millis()),
                ],
            )
            .await?;
        }
        info!(
            schema = %s,
            scene_versions = scenes.len(),
            persona_versions = personas.len(),
            "imported profile files as scene and persona versions"
        );
    }
    Ok(())
}

/// Runs `steps(schema)` in every registered department schema: each step is
/// a table and the statements to run when that table exists (a department
/// creates its tables on first use). Returns the number of departments.
async fn update_departments(
    tx: &mut Transaction<'_, Postgres>,
    registry: &str,
    steps: impl Fn(&str) -> Vec<(&'static str, String)>,
) -> DomainResult<usize> {
    let rows = fetch_all(
        tx,
        &format!("SELECT schema_name FROM {registry}.departments"),
        &[],
    )
    .await?;
    for row in &rows {
        let s = quote_ident(&get::<String>(row, "schema_name")?);
        for (table, sql) in steps(&s) {
            let exists = fetch_optional(
                tx,
                "SELECT 1 AS x WHERE to_regclass(?) IS NOT NULL",
                &args![format!("{s}.{table}")],
            )
            .await?;
            if exists.is_some() {
                execute_script(tx, &sql).await?;
            }
        }
    }
    Ok(rows.len())
}

fn supports_iterative_scan(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    (major, minor) >= (0, 8)
}

/// Adds the `vector(dims)` column and index. When the configured
/// dimensionality changed, stored embeddings came from another model and
/// cannot be compared with new queries, so they are cleared.
async fn migrate_embedding(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    dims: usize,
) -> DomainResult<()> {
    execute(tx, &schema::embedding_column_ddl(schema, dims), &[]).await?;
    // The embedding backfill's queue.
    execute(
        tx,
        &format!("CREATE INDEX IF NOT EXISTS ix_atoms_missing_embedding ON {schema}.atoms(seq) WHERE embedding IS NULL"),
        &[],
    )
    .await?;
    let current = count(
        tx,
        "SELECT atttypmod::bigint FROM pg_attribute WHERE attrelid = (?)::regclass AND attname = 'embedding'",
        &[Arg::from(format!("{schema}.atoms"))],
    )
    .await?;
    if current != dims {
        warn!(%schema, from = current, to = dims, "embedding dimensions changed; clearing stored embeddings");
        execute(
            tx,
            &format!("DROP INDEX IF EXISTS {schema}.ix_atoms_embedding"),
            &[],
        )
        .await?;
        execute(
            tx,
            &format!(
                "ALTER TABLE {schema}.atoms ALTER COLUMN embedding TYPE vector({dims}) USING NULL"
            ),
            &[],
        )
        .await?;
    }
    if dims <= HNSW_MAX_DIMS {
        execute(tx, &schema::embedding_index_ddl(schema), &[]).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_domain::scope::SegmentId;

    #[test]
    fn schema_names_are_short_safe_identifiers() {
        let id = SegmentId::parse("department", "Weird \"name\"; DROP").unwrap();
        let name = schema_name(&id);
        assert_eq!(name.len(), 19);
        assert!(name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
        assert_eq!(name, schema_name(&id), "deterministic");
    }

    #[test]
    fn quotes_identifiers() {
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn detects_iterative_scan_support() {
        assert!(supports_iterative_scan("0.8.0"));
        assert!(!supports_iterative_scan("0.7.4"));
        assert!(supports_iterative_scan("1.0"));
    }
}
