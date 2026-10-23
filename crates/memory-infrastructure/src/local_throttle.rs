//! Single-node `FailureCounter`: counts in memory, lost on restart (which
//! also lifts every lock).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use memory_application::ports::audit::{FailureCounter, FailureState};
use memory_domain::error::{DomainError, DomainResult};

/// Above this many sources, expired entries are swept on the next failure.
const SWEEP_ABOVE: usize = 10_000;

#[derive(Default)]
struct Entry {
    failures: u32,
    window_ends: Option<Instant>,
    locked_until: Option<Instant>,
}

impl Entry {
    fn expired(&self, now: Instant) -> bool {
        self.window_ends.is_none_or(|t| t <= now) && self.locked_until.is_none_or(|t| t <= now)
    }
}

#[derive(Default)]
pub struct LocalFailureCounter {
    sources: Mutex<HashMap<String, Entry>>,
}

impl LocalFailureCounter {
    fn with<T>(&self, f: impl FnOnce(&mut HashMap<String, Entry>) -> T) -> DomainResult<T> {
        let mut map = self
            .sources
            .lock()
            .map_err(|_| DomainError::internal("failure counter lock poisoned"))?;
        Ok(f(&mut map))
    }
}

#[async_trait]
impl FailureCounter for LocalFailureCounter {
    async fn state(&self, source: &str) -> DomainResult<FailureState> {
        let now = Instant::now();
        self.with(|map| {
            let Some(e) = map.get(source) else {
                return FailureState::default();
            };
            FailureState {
                failures: if e.window_ends.is_some_and(|t| t > now) {
                    e.failures
                } else {
                    0
                },
                locked_for: e
                    .locked_until
                    .and_then(|t| t.checked_duration_since(now))
                    .filter(|d| !d.is_zero()),
            }
        })
    }

    async fn add(&self, source: &str, window: Duration) -> DomainResult<u32> {
        let now = Instant::now();
        self.with(|map| {
            if map.len() > SWEEP_ABOVE {
                map.retain(|_, e| !e.expired(now));
            }
            let e = map.entry(source.to_owned()).or_default();
            if e.window_ends.is_none_or(|t| t <= now) {
                e.failures = 0;
                e.window_ends = Some(now + window);
            }
            e.failures += 1;
            e.failures
        })
    }

    async fn lock(&self, source: &str, duration: Duration) -> DomainResult<()> {
        let until = Instant::now() + duration;
        self.with(|map| map.entry(source.to_owned()).or_default().locked_until = Some(until))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn counts_per_window_and_locks_until_the_time_is_up() {
        let c = LocalFailureCounter::default();
        let w = Duration::from_millis(50);
        assert_eq!(c.add("a", w).await.unwrap(), 1);
        assert_eq!(c.add("a", w).await.unwrap(), 2);
        assert_eq!(c.add("b", w).await.unwrap(), 1);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(c.state("a").await.unwrap().failures, 0);
        assert_eq!(c.add("a", w).await.unwrap(), 1, "a new window");

        c.lock("a", Duration::from_millis(50)).await.unwrap();
        assert!(c.state("a").await.unwrap().locked_for.is_some());
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(c.state("a").await.unwrap().locked_for, None);
    }
}
