//! Credential failures: throttling per source and the audit log.
//!
//! The gateway asks [`CredentialGuard::gate`] before a request and reports
//! every refused credential to [`CredentialGuard::rejected`]. A source that
//! keeps failing is answered more slowly, then refused for a while. Nothing
//! here fails a request: when the counter or the log is unreachable the
//! request goes on and the problem is logged.

use std::sync::Arc;
use std::time::Duration;

use memory_domain::error::DomainResult;
use tracing::warn;

use crate::common::Page;
use crate::ports::audit::{AuditEvent, AuditFilter, AuditKind, AuditLog, FailureCounter};
use crate::ports::system::{Clock, IdGenerator};

/// How failures are answered. The defaults: from the 5th failure in 15
/// minutes each refusal waits 250 ms, doubling up to 5 s; the 20th locks the
/// source out for 15 minutes.
#[derive(Debug, Clone, Copy)]
pub struct ThrottlePolicy {
    pub window: Duration,
    pub delay_after: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub lock_after: u32,
    pub lock_for: Duration,
}

impl Default for ThrottlePolicy {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(15 * 60),
            delay_after: 5,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(5),
            lock_after: 20,
            lock_for: Duration::from_secs(15 * 60),
        }
    }
}

impl ThrottlePolicy {
    /// How long the refusal of the `failures`-th failure waits.
    pub fn delay(&self, failures: u32) -> Duration {
        if failures < self.delay_after {
            return Duration::ZERO;
        }
        let doublings = (failures - self.delay_after).min(16);
        self.base_delay
            .saturating_mul(1 << doublings)
            .min(self.max_delay)
    }
}

/// Whether a request from a source may go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Open,
    Locked { retry_after: Duration },
}

/// What the gateway knows about a refused request.
#[derive(Debug, Clone, Default)]
pub struct Attempt {
    pub source: String,
    pub forwarded_for: Option<String>,
    pub user_agent: Option<String>,
    pub department: Option<String>,
    pub path: String,
    pub credential: Option<String>,
    pub request_id: String,
}

pub struct CredentialGuard {
    counter: Arc<dyn FailureCounter>,
    log: Arc<dyn AuditLog>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    policy: ThrottlePolicy,
    retention: Duration,
}

impl CredentialGuard {
    pub fn new(
        counter: Arc<dyn FailureCounter>,
        log: Arc<dyn AuditLog>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        policy: ThrottlePolicy,
        retention: Duration,
    ) -> Self {
        Self {
            counter,
            log,
            clock,
            ids,
            policy,
            retention,
        }
    }

    pub async fn gate(&self, source: &str) -> Gate {
        match self.counter.state(source).await {
            Ok(state) => match state.locked_for {
                Some(left) if !left.is_zero() => Gate::Locked { retry_after: left },
                _ => Gate::Open,
            },
            Err(e) => {
                warn!(error = %e, "credential throttle unavailable; not throttling");
                Gate::Open
            }
        }
    }

    /// Counts the failure, records it, locks the source at the limit, and
    /// returns how long the refusal should wait.
    pub async fn rejected(&self, attempt: Attempt) -> Duration {
        let failures = match self.counter.add(&attempt.source, self.policy.window).await {
            Ok(n) => n,
            Err(e) => {
                warn!(error = %e, "credential throttle unavailable; not counting");
                0
            }
        };
        self.record(AuditKind::CredentialRejected, &attempt, None)
            .await;
        if failures >= self.policy.lock_after {
            if let Err(e) = self
                .counter
                .lock(&attempt.source, self.policy.lock_for)
                .await
            {
                warn!(error = %e, "credential throttle unavailable; not locking");
            }
            let detail = format!(
                "{failures} failures; locked for {} minutes",
                self.policy.lock_for.as_secs() / 60
            );
            self.record(AuditKind::SourceLocked, &attempt, Some(detail))
                .await;
            return Duration::ZERO;
        }
        self.policy.delay(failures)
    }

    /// Records an event; a failure to write is logged, not raised.
    pub async fn record(&self, kind: AuditKind, attempt: &Attempt, detail: Option<String>) {
        let event = AuditEvent {
            event_id: self.ids.next("aud-"),
            at_ms: self.clock.now().timestamp_millis(),
            kind,
            department: attempt.department.clone(),
            source: attempt.source.clone(),
            forwarded_for: attempt.forwarded_for.clone(),
            user_agent: attempt.user_agent.clone(),
            path: attempt.path.clone(),
            credential: attempt.credential.clone(),
            request_id: attempt.request_id.clone(),
            detail,
        };
        if let Err(e) = self.log.record(&event).await {
            warn!(error = %e, kind = kind.as_str(), "audit log unavailable; event not recorded");
        }
    }

    pub async fn list(&self, filter: &AuditFilter, page: Page) -> DomainResult<Vec<AuditEvent>> {
        self.log.list(filter, page).await
    }

    /// Drops events past the retention period.
    pub async fn prune(&self) -> DomainResult<u64> {
        let cutoff = self.clock.now().timestamp_millis()
            - i64::try_from(self.retention.as_millis()).unwrap_or(i64::MAX);
        self.log.prune(cutoff).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::audit::FailureState;
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Counter(Mutex<HashMap<String, FailureState>>);

    #[async_trait]
    impl FailureCounter for Counter {
        async fn state(&self, source: &str) -> DomainResult<FailureState> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(source)
                .copied()
                .unwrap_or_default())
        }
        async fn add(&self, source: &str, _: Duration) -> DomainResult<u32> {
            let mut map = self.0.lock().unwrap();
            let s = map.entry(source.into()).or_default();
            s.failures += 1;
            Ok(s.failures)
        }
        async fn lock(&self, source: &str, d: Duration) -> DomainResult<()> {
            self.0
                .lock()
                .unwrap()
                .entry(source.into())
                .or_default()
                .locked_for = Some(d);
            Ok(())
        }
    }

    #[derive(Default)]
    struct Log(Mutex<Vec<AuditEvent>>);

    #[async_trait]
    impl AuditLog for Log {
        async fn record(&self, e: &AuditEvent) -> DomainResult<()> {
            self.0.lock().unwrap().push(e.clone());
            Ok(())
        }
        async fn list(&self, _: &AuditFilter, _: Page) -> DomainResult<Vec<AuditEvent>> {
            Ok(self.0.lock().unwrap().clone())
        }
        async fn prune(&self, before: i64) -> DomainResult<u64> {
            let mut v = self.0.lock().unwrap();
            let n = v.len();
            v.retain(|e| e.at_ms >= before);
            Ok((n - v.len()) as u64)
        }
    }

    struct Fixed;
    impl Clock for Fixed {
        fn now(&self) -> DateTime<Utc> {
            DateTime::from_timestamp_millis(1_000_000_000_000).unwrap()
        }
    }
    struct Ids;
    impl IdGenerator for Ids {
        fn next(&self, prefix: &str) -> String {
            format!("{prefix}x")
        }
        fn hex(&self, n: usize) -> String {
            "0".repeat(n)
        }
        fn secret(&self, _: usize) -> String {
            "s".into()
        }
    }

    fn guard() -> (CredentialGuard, Arc<Log>) {
        let log = Arc::new(Log::default());
        let g = CredentialGuard::new(
            Arc::new(Counter::default()),
            log.clone(),
            Arc::new(Fixed),
            Arc::new(Ids),
            ThrottlePolicy::default(),
            Duration::from_secs(90 * 24 * 3600),
        );
        (g, log)
    }

    #[test]
    fn delays_start_at_the_fifth_failure_and_double_up_to_five_seconds() {
        let p = ThrottlePolicy::default();
        let ms = |n| p.delay(n).as_millis();
        assert_eq!([ms(1), ms(4)], [0, 0]);
        assert_eq!(
            [ms(5), ms(6), ms(7), ms(8), ms(9)],
            [250, 500, 1000, 2000, 4000]
        );
        assert_eq!([ms(10), ms(19), ms(1000)], [5000, 5000, 5000]);
    }

    #[tokio::test]
    async fn the_twentieth_failure_locks_the_source_and_is_recorded() {
        let (g, log) = guard();
        let attempt = |source: &str| Attempt {
            source: source.into(),
            path: "/v3/recall".into(),
            ..Default::default()
        };
        for _ in 0..19 {
            g.rejected(attempt("10.0.0.1")).await;
        }
        assert_eq!(g.gate("10.0.0.1").await, Gate::Open);
        g.rejected(attempt("10.0.0.1")).await;
        assert!(matches!(g.gate("10.0.0.1").await, Gate::Locked { .. }));
        assert_eq!(g.gate("10.0.0.2").await, Gate::Open, "per source");
        let events = log.0.lock().unwrap();
        assert_eq!(events.len(), 21);
        assert_eq!(events.last().unwrap().kind, AuditKind::SourceLocked);
    }

    #[tokio::test]
    async fn prune_drops_events_past_retention() {
        let (g, log) = guard();
        let old = AuditEvent {
            event_id: "a".into(),
            at_ms: 1_000_000_000_000 - 91 * 24 * 3600 * 1000,
            kind: AuditKind::CredentialRejected,
            department: None,
            source: "s".into(),
            forwarded_for: None,
            user_agent: None,
            path: "/".into(),
            credential: None,
            request_id: "r".into(),
            detail: None,
        };
        log.record(&old).await.unwrap();
        g.record(AuditKind::KeyIssued, &Attempt::default(), None)
            .await;
        assert_eq!(g.prune().await.unwrap(), 1);
        assert_eq!(log.0.lock().unwrap().len(), 1);
    }
}
