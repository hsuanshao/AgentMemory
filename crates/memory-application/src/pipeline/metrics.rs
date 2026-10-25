//! Pipeline worker metrics shared between the worker and the status API.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;

#[derive(Debug, Default)]
pub struct PipelineMetrics {
    enqueued: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    retried: AtomicU64,
    dead_lettered: AtomicU64,
    redriven: AtomicU64,
    expired: AtomicU64,
    last: Mutex<LastEvent>,
}

#[derive(Debug, Default, Clone, Serialize)]
struct LastEvent {
    last_task_at: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub tasks_enqueued: u64,
    pub tasks_completed: u64,
    pub tasks_failed: u64,
    pub tasks_retried: u64,
    pub tasks_dead_lettered: u64,
    /// Dead letters the sweeper requeued on its own.
    pub dead_letters_redriven: u64,
    /// Dead letters the sweeper removed (too old or redriven too often).
    pub dead_letters_expired: u64,
    pub last_task_at: Option<String>,
    /// The error of the last job that was dead-lettered. A failed attempt
    /// that is retried only counts in `tasks_retried`.
    pub last_error: Option<String>,
}

impl PipelineMetrics {
    pub fn enqueued(&self) {
        self.enqueued.fetch_add(1, Ordering::Relaxed);
    }

    pub fn completed(&self, at: String) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        self.last.lock().expect("metrics lock").last_task_at = Some(at);
    }

    pub fn failed(&self, error: String, will_retry: bool) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        let counter = if will_retry {
            &self.retried
        } else {
            &self.dead_lettered
        };
        counter.fetch_add(1, Ordering::Relaxed);
        if !will_retry {
            self.last.lock().expect("metrics lock").last_error = Some(error);
        }
    }

    pub fn swept(&self, redriven: usize, expired: usize) {
        self.redriven.fetch_add(redriven as u64, Ordering::Relaxed);
        self.expired.fetch_add(expired as u64, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let last = self.last.lock().expect("metrics lock").clone();
        MetricsSnapshot {
            tasks_enqueued: self.enqueued.load(Ordering::Relaxed),
            tasks_completed: self.completed.load(Ordering::Relaxed),
            tasks_failed: self.failed.load(Ordering::Relaxed),
            tasks_retried: self.retried.load(Ordering::Relaxed),
            tasks_dead_lettered: self.dead_lettered.load(Ordering::Relaxed),
            dead_letters_redriven: self.redriven.load(Ordering::Relaxed),
            dead_letters_expired: self.expired.load(Ordering::Relaxed),
            last_task_at: last.last_task_at,
            last_error: last.last_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retried_attempt_leaves_last_error_alone() {
        let m = PipelineMetrics::default();
        m.failed("skill job failed: timeout".into(), true);
        m.completed("2026-10-25T09:43:50Z".into());
        let s = m.snapshot();
        assert_eq!((s.tasks_failed, s.tasks_retried), (1, 1));
        assert_eq!(s.last_error, None);
    }

    #[test]
    fn a_dead_lettered_job_sets_last_error() {
        let m = PipelineMetrics::default();
        m.failed("l1 job failed: first".into(), false);
        m.failed("l1 job failed: retried".into(), true);
        let s = m.snapshot();
        assert_eq!(s.tasks_dead_lettered, 1);
        assert_eq!(s.last_error.as_deref(), Some("l1 job failed: first"));
    }
}
