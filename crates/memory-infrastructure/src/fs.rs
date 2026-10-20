//! Local filesystem adapters rooted at each department directory.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use memory_application::ports::memory::SessionObjectStore;
use memory_application::ports::skill::SkillResourceStore;
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::scope::DepartmentId;

/// Files under `<data>/departments/<id>/`; independent of the storage backend.
pub struct LocalFileStore {
    data_dir: PathBuf,
}

impl LocalFileStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    fn department_dir(&self, department: &DepartmentId) -> PathBuf {
        self.data_dir.join("departments").join(department.as_str())
    }

    /// Resolves `key` under the department directory, rejecting anything that
    /// could escape it.
    fn resolve(&self, department: &DepartmentId, key: &str) -> DomainResult<PathBuf> {
        let relative = Path::new(key);
        let safe = !key.is_empty()
            && !key.contains('\0')
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_)));
        if !safe {
            return Err(DomainError::invalid(format!("invalid storage key: {key}")));
        }
        Ok(self.department_dir(department).join(relative))
    }

    fn skill_dir(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
    ) -> DomainResult<PathBuf> {
        self.resolve(department, &format!("skills/{skill_id}/v{version}/files"))
    }
}

fn io_err(e: std::io::Error) -> DomainError {
    DomainError::unavailable(format!("storage I/O failed: {e}"))
}

async fn write_file(path: &Path, bytes: &[u8]) -> DomainResult<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(io_err)?;
    }
    // Write-then-rename keeps readers from seeing partial files.
    let tmp = path.with_extension("tmp-write");
    tokio::fs::write(&tmp, bytes).await.map_err(io_err)?;
    tokio::fs::rename(&tmp, path).await.map_err(io_err)
}

async fn remove_tree(path: &Path) -> DomainResult<()> {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(e)),
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[async_trait]
impl SessionObjectStore for LocalFileStore {
    async fn put(&self, department: &DepartmentId, key: &str, bytes: &[u8]) -> DomainResult<()> {
        write_file(&self.resolve(department, key)?, bytes).await
    }
}

#[async_trait]
impl SkillResourceStore for LocalFileStore {
    async fn copy_version(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        from: u32,
        to: u32,
    ) -> DomainResult<()> {
        let (src, dst) = (
            self.skill_dir(department, skill_id, from)?,
            self.skill_dir(department, skill_id, to)?,
        );
        if !src.exists() {
            return Ok(());
        }
        tokio::task::spawn_blocking(move || copy_tree(&src, &dst))
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?
            .map_err(io_err)
    }

    async fn write(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
        path: &str,
        bytes: &[u8],
    ) -> DomainResult<()> {
        self.resolve(department, path)?;
        write_file(
            &self.skill_dir(department, skill_id, version)?.join(path),
            bytes,
        )
        .await
    }

    async fn remove(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
        path: &str,
    ) -> DomainResult<()> {
        self.resolve(department, path)?;
        match tokio::fs::remove_file(self.skill_dir(department, skill_id, version)?.join(path))
            .await
        {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_err(e)),
            _ => Ok(()),
        }
    }

    async fn read(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
        path: &str,
    ) -> DomainResult<Option<Vec<u8>>> {
        self.resolve(department, path)?;
        match tokio::fs::read(self.skill_dir(department, skill_id, version)?.join(path)).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(e)),
        }
    }

    async fn delete_version(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
    ) -> DomainResult<()> {
        remove_tree(&self.resolve(department, &format!("skills/{skill_id}/v{version}"))?).await
    }

    async fn delete_skill(&self, department: &DepartmentId, skill_id: &str) -> DomainResult<()> {
        remove_tree(&self.resolve(department, &format!("skills/{skill_id}"))?).await
    }
}

/// Moves `<data>/instances` (before 0.3.0) to `<data>/departments`. Runs at
/// start-up before anything opens a department; a no-op once moved.
pub fn migrate_data_dir(data_dir: &Path) -> DomainResult<()> {
    let old = data_dir.join("instances");
    if !old.is_dir() {
        return Ok(());
    }
    let new = data_dir.join("departments");
    if new.exists() {
        return Err(DomainError::unavailable(format!(
            "both {} and {} exist; move the departments from the first into the second, then delete it",
            old.display(),
            new.display()
        )));
    }
    std::fs::rename(&old, &new).map_err(|e| {
        DomainError::unavailable(format!(
            "rename {} to {}: {e}",
            old.display(),
            new.display()
        ))
    })?;
    tracing::info!(from = %old.display(), to = %new.display(), "moved the department directories");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_moves_once_and_refuses_to_merge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("instances/default")).unwrap();
        std::fs::write(dir.path().join("instances/default/memory.db"), b"x").unwrap();

        migrate_data_dir(dir.path()).unwrap();
        assert!(dir.path().join("departments/default/memory.db").is_file());
        assert!(!dir.path().join("instances").exists());
        migrate_data_dir(dir.path()).unwrap();

        std::fs::create_dir(dir.path().join("instances")).unwrap();
        assert!(migrate_data_dir(dir.path()).is_err());
    }
}
