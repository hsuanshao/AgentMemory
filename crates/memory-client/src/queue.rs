//! Offline queue: captures that could not be delivered wait here as one
//! JSON file per request and are replayed, oldest first, before the next
//! capture or on `amem flush`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::{Api, ApiError};
use crate::config::write_private;

/// Oldest jobs are dropped beyond this, so a long outage cannot fill the disk.
pub const MAX_JOBS: usize = 2_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    /// Core v3 path, e.g. `conversation/add`.
    pub path: String,
    pub body: Value,
    pub created_at_ms: u128,
}

pub struct Queue {
    dir: PathBuf,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FlushReport {
    pub sent: usize,
    pub failed: usize,
    pub remaining: usize,
    /// Why flushing stopped early (the server is still unreachable).
    pub blocked: Option<String>,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

impl Queue {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("queue"),
        }
    }

    fn failed_dir(&self) -> PathBuf {
        self.dir.join("failed")
    }

    /// Pending job files, oldest first (names start with a zero-padded time).
    pub fn pending(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        files
    }

    pub fn push(&self, path: &str, body: &Value) -> Result<(), String> {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let job = Job {
            path: path.to_owned(),
            body: body.clone(),
            created_at_ms: now_ms(),
        };
        let name = format!(
            "{:020}-{}-{:04}.json",
            job.created_at_ms,
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let bytes = serde_json::to_vec(&job).map_err(|e| e.to_string())?;
        write_private(&self.dir.join(name), &bytes)?;
        let pending = self.pending();
        for old in pending.iter().take(pending.len().saturating_sub(MAX_JOBS)) {
            let _ = std::fs::remove_file(old);
        }
        Ok(())
    }

    /// Replays pending jobs in order. Stops at the first retryable failure
    /// so order is kept; moves rejected jobs to `queue/failed/`. Only one
    /// process flushes at a time, or two could send the same job.
    pub fn flush(&self, api: &Api) -> FlushReport {
        let mut report = FlushReport::default();
        let Some(_lock) = FlushLock::acquire(&self.dir) else {
            report.remaining = self.pending().len();
            report.blocked = Some("another amem process is flushing the queue".into());
            return report;
        };
        let pending = self.pending();
        for (i, file) in pending.iter().enumerate() {
            let job: Option<Job> = std::fs::read(file)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            let Some(job) = job else {
                self.park(file);
                report.failed += 1;
                continue;
            };
            match api.post(&job.path, &job.body) {
                Ok(_) => {
                    let _ = std::fs::remove_file(file);
                    report.sent += 1;
                }
                Err(ApiError::Rejected(_)) => {
                    self.park(file);
                    report.failed += 1;
                }
                Err(ApiError::Retryable(why)) => {
                    report.remaining = pending.len() - i;
                    report.blocked = Some(why);
                    return report;
                }
            }
        }
        report
    }

    fn park(&self, file: &Path) {
        let failed = self.failed_dir();
        let _ = std::fs::create_dir_all(&failed);
        if let Some(name) = file.file_name() {
            let _ = std::fs::rename(file, failed.join(name));
        }
    }
}

/// Exclusive-create lock file; a lock older than `STALE` is from a crashed
/// process and is taken over.
struct FlushLock(PathBuf);

impl FlushLock {
    const STALE: std::time::Duration = std::time::Duration::from_secs(120);

    fn acquire(dir: &Path) -> Option<Self> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(".flush.lock");
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Some(Self(path)),
                Err(_) => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Self::STALE);
                    if !stale {
                        return None;
                    }
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        None
    }
}

impl Drop for FlushLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_order_and_caps_size() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::new(dir.path());
        for i in 0..5 {
            q.push("conversation/add", &json!({"i": i})).unwrap();
        }
        let first: Job = serde_json::from_slice(&std::fs::read(&q.pending()[0]).unwrap()).unwrap();
        assert_eq!(first.body["i"], 0);
        assert_eq!(q.pending().len(), 5);
    }

    #[test]
    fn one_flusher_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let held = FlushLock::acquire(dir.path()).unwrap();
        assert!(FlushLock::acquire(dir.path()).is_none());
        drop(held);
        assert!(FlushLock::acquire(dir.path()).is_some());
    }
}
