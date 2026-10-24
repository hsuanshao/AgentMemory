//! Distributed `FailureCounter`: one counter and one lock key per source,
//! both expiring by themselves, so every replica sees the same counts.

use std::time::Duration;

use ::redis::aio::ConnectionManager;
use ::redis::Script;
use async_trait::async_trait;
use memory_application::ports::audit::{FailureCounter, FailureState};
use memory_domain::error::DomainResult;

use super::{unavailable, KeySpace};

/// Counts a failure; the first one of a window sets its expiry.
const ADD: &str = r#"
local n = redis.call('INCR', KEYS[1])
if n == 1 then redis.call('PEXPIRE', KEYS[1], ARGV[1]) end
return n
"#;

pub struct RedisFailureCounter {
    conn: ConnectionManager,
    failures: KeySpace,
    locks: KeySpace,
    add: Script,
}

impl RedisFailureCounter {
    pub fn new(conn: ConnectionManager, keys: &KeySpace) -> Self {
        Self {
            conn,
            failures: keys.child("auth:failures"),
            locks: keys.child("auth:locked"),
            add: Script::new(ADD),
        }
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX).max(1)
}

#[async_trait]
impl FailureCounter for RedisFailureCounter {
    async fn state(&self, source: &str) -> DomainResult<FailureState> {
        let mut conn = self.conn.clone();
        let (failures, ttl): (Option<u32>, i64) = ::redis::pipe()
            .get(self.failures.key(source))
            .pttl(self.locks.key(source))
            .query_async(&mut conn)
            .await
            .map_err(unavailable)?;
        Ok(FailureState {
            failures: failures.unwrap_or(0),
            locked_for: (ttl > 0).then(|| Duration::from_millis(ttl as u64)),
        })
    }

    async fn add(&self, source: &str, window: Duration) -> DomainResult<u32> {
        let mut conn = self.conn.clone();
        self.add
            .key(self.failures.key(source))
            .arg(millis(window))
            .invoke_async(&mut conn)
            .await
            .map_err(unavailable)
    }

    async fn lock(&self, source: &str, duration: Duration) -> DomainResult<()> {
        let mut conn = self.conn.clone();
        ::redis::cmd("SET")
            .arg(self.locks.key(source))
            .arg(1)
            .arg("PX")
            .arg(millis(duration))
            .query_async::<()>(&mut conn)
            .await
            .map_err(unavailable)
    }
}
