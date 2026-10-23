//! A SQLite connection usable from async code, and a per-department pool.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use memory_domain::error::{DomainError, DomainResult};
use memory_domain::scope::DepartmentId;
use rusqlite::Connection;

use super::{schema, vec_index};

/// Blocking SQLite work runs on the blocking thread pool, one statement
/// batch at a time per database.
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(
        path: &Path,
        migrate: fn(&Connection) -> rusqlite::Result<()>,
    ) -> DomainResult<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| DomainError::unavailable(format!("create {}: {e}", dir.display())))?;
        }
        // Before the first open, so every connection gets `vec0` if it loads.
        vec_index::register();
        let conn = Connection::open(path).map_err(db_err)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;",
        )
        .map_err(db_err)?;
        migrate(&conn).map_err(db_err)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn call<T, F>(&self, f: F) -> DomainResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| DomainError::internal("database lock poisoned"))?;
            f(&mut guard).map_err(db_err)
        })
        .await
        .map_err(|e| DomainError::internal(format!("database task failed: {e}")))?
    }

    /// Like [`Db::call`] for closures that already speak `DomainResult`.
    pub async fn call_domain<T, F>(&self, f: F) -> DomainResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> DomainResult<T> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| DomainError::internal("database lock poisoned"))?;
            f(&mut guard)
        })
        .await
        .map_err(|e| DomainError::internal(format!("database task failed: {e}")))?
    }
}

pub fn db_err(e: rusqlite::Error) -> DomainError {
    match &e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            DomainError::conflict("duplicate_entry", e.to_string())
        }
        _ => DomainError::unavailable(format!("storage error: {e}")),
    }
}

struct Entry {
    db: Arc<Db>,
    last_used: Instant,
}

/// Lazily opens `<data_dir>/departments/<id>/memory.db`, keeping at most
/// `capacity` connections (least recently used are closed first).
pub struct DepartmentPool {
    data_dir: PathBuf,
    capacity: usize,
    entries: Mutex<HashMap<DepartmentId, Entry>>,
}

/// Where `retire` moves a department's directory before deleting it.
const REMOVED_DIR: &str = "departments-removed";

impl DepartmentPool {
    pub fn new(data_dir: PathBuf, capacity: usize) -> Self {
        // Left behind when the process stopped between moving a retired
        // department aside and deleting it.
        let _ = std::fs::remove_dir_all(data_dir.join(REMOVED_DIR));
        Self {
            data_dir,
            capacity: capacity.max(1),
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn department_dir(&self, department: &DepartmentId) -> PathBuf {
        self.data_dir.join("departments").join(department.as_str())
    }

    /// The department's database; `unknown_department` when it has none
    /// (only [`DepartmentPool::create`] makes one).
    pub fn get(&self, department: &DepartmentId) -> DomainResult<Arc<Db>> {
        self.open(department, false)
    }

    /// Opens the department's database, creating it if needed.
    pub fn create(&self, department: &DepartmentId) -> DomainResult<Arc<Db>> {
        self.open(department, true)
    }

    fn open(&self, department: &DepartmentId, create: bool) -> DomainResult<Arc<Db>> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| DomainError::internal("pool lock poisoned"))?;
        if let Some(entry) = entries.get_mut(department) {
            entry.last_used = Instant::now();
            return Ok(Arc::clone(&entry.db));
        }
        let path = self.department_dir(department).join("memory.db");
        if !create && !path.is_file() {
            return Err(DomainError::unknown_department(department));
        }
        if entries.len() >= self.capacity {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone())
            {
                entries.remove(&oldest);
            }
        }
        let db = Arc::new(Db::open(&path, schema::migrate_department)?);
        entries.insert(
            department.clone(),
            Entry {
                db: Arc::clone(&db),
                last_used: Instant::now(),
            },
        );
        Ok(db)
    }

    /// Whether the department has a database (open or on disk); never
    /// creates one.
    pub fn exists(&self, department: &DepartmentId) -> bool {
        let open = self
            .entries
            .lock()
            .is_ok_and(|entries| entries.contains_key(department));
        open || self.department_dir(department).join("memory.db").is_file()
    }

    /// Takes the department out of service in one step under the pool lock:
    /// its open database is dropped and its directory moved aside, so no
    /// `get` in between can reopen the file or build a new one. Returns
    /// where the directory went, for the caller to delete (that can take a
    /// while, so it happens outside the lock). In-flight users keep their
    /// handle to the old file; nothing they write is visible afterwards.
    pub fn retire(&self, department: &DepartmentId) -> DomainResult<Option<PathBuf>> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| DomainError::internal("pool lock poisoned"))?;
        entries.remove(department);
        let dir = self.department_dir(department);
        if !dir.exists() {
            return Ok(None);
        }
        let removed = self.data_dir.join(REMOVED_DIR);
        std::fs::create_dir_all(&removed)
            .map_err(|e| DomainError::unavailable(format!("create {}: {e}", removed.display())))?;
        let to = removed.join(format!("{}-{}", department, uuid::Uuid::new_v4().simple()));
        std::fs::rename(&dir, &to)
            .map_err(|e| DomainError::unavailable(format!("move {} aside: {e}", dir.display())))?;
        Ok(Some(to))
    }

    /// Closes the department database (in-flight users keep their handle).
    pub fn evict(&self, department: &DepartmentId) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(department);
        }
    }

    /// Departments that currently have a database on disk (a directory
    /// holding only files is not one).
    pub fn known_departments(&self) -> Vec<String> {
        std::fs::read_dir(self.data_dir.join("departments"))
            .map(|dir| {
                dir.filter_map(Result::ok)
                    .filter(|e| e.path().join("memory.db").is_file())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn fts_available(&self) -> bool {
        Connection::open_in_memory()
            .and_then(|c| c.execute_batch("CREATE VIRTUAL TABLE t USING fts5(x);"))
            .is_ok()
    }
}
