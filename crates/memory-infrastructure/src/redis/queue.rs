//! `QueueBackend` over Redis, shared by every node's dispatcher.
//!
//! Each state change is one Lua script, so it is atomic on the server: a
//! job is in exactly one of `ready`, `delayed` or `processing` (leased to
//! one consumer under a receipt). Delivery is at-least-once: a lease that
//! is neither acked nor extended expires (its node presumably died) and
//! the next `pop` on any node puts the job back in `ready`, attempt count
//! unchanged. Times come from the Redis clock (`TIME`), so clock skew
//! between nodes cannot release a retry or a lease early or late.

use std::time::Duration;

use ::redis::aio::ConnectionManager;
use ::redis::{cmd, pipe, Script};
use async_trait::async_trait;
use memory_application::ports::pipeline::{DeadLetter, PipelineJob};
use memory_domain::error::DomainResult;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{from_json, to_json, unavailable, KeySpace};
use crate::worker::{dedupe_key, Delivery, Envelope, QueueBackend};

/// The coalescing marker outlives a lost job (node crash after the pop)
/// by at most this long; afterwards an identical job can be queued again.
const DEDUPE_TTL_SECS: u64 = 3600;
/// Due delayed jobs promoted per pop; the rest follow on the next pops.
const PROMOTE_BATCH: u32 = 100;
/// Polling backoff while the queue is empty. BLPOP would stall the shared
/// multiplexed connection, so `pop` polls instead.
const POLL_MIN: Duration = Duration::from_millis(10);
const POLL_MAX: Duration = Duration::from_millis(100);
/// Newest dead letters kept; older ones are trimmed so the list is bounded.
const DEAD_LETTER_CAP: i64 = 10_000;

/// Adds the job unless its dedupe marker exists (set in the same step).
/// The marker holds the item id, so only that item's pop clears it.
const PUSH_NEW: &str = r#"
if redis.call('SET', KEYS[1], ARGV[3], 'NX', 'EX', ARGV[2]) then
  redis.call('RPUSH', KEYS[2], ARGV[1])
  return 1
end
return 0
"#;

/// Schedules the job `ARGV[2]` ms from now on the server clock.
const PUSH_LATER: &str = r#"
local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
redis.call('ZADD', KEYS[1], now + tonumber(ARGV[2]), ARGV[1])
return 1
"#;

/// In one atomic step: requeues expired leases, promotes due delayed jobs,
/// pops the head of `ready`, clears its dedupe marker (if it still names
/// this item) and leases it under the caller's receipt.
///
/// KEYS: delayed, ready, processing (receipt → lease deadline), inflight
/// (receipt → item). ARGV: dedupe prefix, batch, receipt, lease ms.
const POP: &str = r#"
local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
local batch = tonumber(ARGV[2])
local expired = redis.call('ZRANGE', KEYS[3], '-inf', now, 'BYSCORE', 'LIMIT', 0, batch)
for _, receipt in ipairs(expired) do
  local item = redis.call('HGET', KEYS[4], receipt)
  if item then
    -- Redeliveries go first: they have waited longest.
    redis.call('LPUSH', KEYS[2], item)
  end
  redis.call('HDEL', KEYS[4], receipt)
  redis.call('ZREM', KEYS[3], receipt)
end
local due = redis.call('ZRANGE', KEYS[1], '-inf', now, 'BYSCORE', 'LIMIT', 0, batch)
for _, item in ipairs(due) do
  redis.call('ZREM', KEYS[1], item)
  redis.call('RPUSH', KEYS[2], item)
end
local item = redis.call('LPOP', KEYS[2])
if not item then
  return false
end
local ok, decoded = pcall(cjson.decode, item)
if ok and type(decoded) == 'table' and type(decoded.dedupe) == 'string' then
  local marker = ARGV[1] .. decoded.dedupe
  if redis.call('GET', marker) == decoded.id then
    redis.call('DEL', marker)
  end
end
redis.call('ZADD', KEYS[3], now + tonumber(ARGV[4]), ARGV[3])
redis.call('HSET', KEYS[4], ARGV[3], item)
return item
"#;

/// Pushes the lease deadline out, only while the lease still exists (it
/// may have expired and been redelivered already).
const EXTEND: &str = r#"
if not redis.call('ZSCORE', KEYS[1], ARGV[1]) then
  return 0
end
local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
redis.call('ZADD', KEYS[1], 'XX', now + tonumber(ARGV[2]), ARGV[1])
return 1
"#;

/// What is stored per queued job. `id` keeps identical envelopes distinct
/// in the delayed set and is the value of the dedupe marker it set, so a
/// redelivered item never clears a newer job's marker; `dedupe` names the
/// marker a first attempt clears.
#[derive(Serialize, Deserialize)]
struct Item {
    id: String,
    dedupe: Option<String>,
    envelope: Envelope,
}

impl Item {
    fn new(envelope: Envelope) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            dedupe: (envelope.attempt == 0).then(|| dedupe_key(&envelope.job)),
            envelope,
        }
    }
}

pub struct RedisQueue {
    conn: ConnectionManager,
    keys: KeySpace,
    lease: Duration,
    push_new: Script,
    push_later: Script,
    pop: Script,
    extend: Script,
}

impl RedisQueue {
    /// Stored dead letters with their exact stored text (for `LREM`), oldest
    /// first. Letters written before errors were recorded parse too.
    async fn raw_dead_letters(&self) -> DomainResult<Vec<(String, DeadLetter)>> {
        let raw: Vec<String> = cmd("LRANGE")
            .arg(self.dead())
            .arg(0)
            .arg(-1)
            .query_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)?;
        Ok(raw
            .into_iter()
            .filter_map(|r| match from_json::<DeadLetter>(&r) {
                Ok(letter) => Some((r, letter)),
                Err(err) => {
                    warn!(error = %err, "skipping an unreadable dead letter");
                    None
                }
            })
            .collect())
    }

    /// `lease`: how long a popped job may go without `ack`/`extend` before
    /// it is redelivered.
    pub fn new(conn: ConnectionManager, prefix: &KeySpace, lease: Duration) -> Self {
        Self {
            conn,
            keys: prefix.child("queue"),
            lease: lease.max(Duration::from_millis(30)),
            push_new: Script::new(PUSH_NEW),
            push_later: Script::new(PUSH_LATER),
            pop: Script::new(POP),
            extend: Script::new(EXTEND),
        }
    }

    fn lease_ms(&self) -> u64 {
        u64::try_from(self.lease.as_millis()).unwrap_or(u64::MAX)
    }

    fn ready(&self) -> String {
        self.keys.key("ready")
    }

    fn delayed(&self) -> String {
        self.keys.key("delayed")
    }

    fn dead(&self) -> String {
        self.keys.key("dead")
    }

    fn processing(&self) -> String {
        self.keys.key("processing")
    }

    fn inflight(&self) -> String {
        self.keys.key("inflight")
    }

    fn dedupe_prefix(&self) -> String {
        self.keys.key("dedupe:")
    }

    async fn pop_once(&self) -> DomainResult<Option<Delivery>> {
        let receipt = uuid::Uuid::new_v4().to_string();
        let item: Option<String> = self
            .pop
            .key(self.delayed())
            .key(self.ready())
            .key(self.processing())
            .key(self.inflight())
            .arg(self.dedupe_prefix())
            .arg(PROMOTE_BATCH)
            .arg(&receipt)
            .arg(self.lease_ms())
            .invoke_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)?;
        let Some(raw) = item else {
            return Ok(None);
        };
        Ok(Some(Delivery {
            envelope: from_json::<Item>(&raw)?.envelope,
            receipt,
        }))
    }
}

#[async_trait]
impl QueueBackend for RedisQueue {
    async fn push_new(&self, job: PipelineJob) -> DomainResult<()> {
        let marker = format!("{}{}", self.dedupe_prefix(), dedupe_key(&job));
        let item = Item::new(Envelope::new(job));
        let added: i64 = self
            .push_new
            .key(marker)
            .key(self.ready())
            .arg(to_json(&item)?)
            .arg(DEDUPE_TTL_SECS)
            .arg(&item.id)
            .invoke_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)?;
        if added == 0 {
            debug!("job already queued; coalescing");
        }
        Ok(())
    }

    async fn push_later(&self, envelope: Envelope, delay: Duration) -> DomainResult<()> {
        let delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
        self.push_later
            .key(self.delayed())
            .arg(to_json(&Item::new(envelope))?)
            .arg(delay_ms)
            .invoke_async::<i64>(&mut self.conn.clone())
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn pop(&self, wait: Duration) -> DomainResult<Option<Delivery>> {
        let deadline = tokio::time::Instant::now() + wait;
        let mut backoff = POLL_MIN;
        loop {
            if let Some(delivery) = self.pop_once().await? {
                return Ok(Some(delivery));
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            tokio::time::sleep(backoff.min(left)).await;
            backoff = (backoff * 2).min(POLL_MAX);
        }
    }

    async fn ack(&self, receipt: &str) -> DomainResult<()> {
        pipe()
            .atomic()
            .cmd("ZREM")
            .arg(self.processing())
            .arg(receipt)
            .ignore()
            .cmd("HDEL")
            .arg(self.inflight())
            .arg(receipt)
            .ignore()
            .exec_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)
    }

    async fn extend(&self, receipt: &str) -> DomainResult<()> {
        let extended: i64 = self
            .extend
            .key(self.processing())
            .arg(receipt)
            .arg(self.lease_ms())
            .invoke_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)?;
        if extended == 0 {
            warn!("job lease already expired; it may run twice");
        }
        Ok(())
    }

    /// Well inside the lease, so one missed renewal is survivable.
    fn extend_interval(&self) -> Option<Duration> {
        Some(self.lease / 3)
    }

    async fn dead_letter(&self, letter: DeadLetter) -> DomainResult<()> {
        pipe()
            .atomic()
            .cmd("RPUSH")
            .arg(self.dead())
            .arg(to_json(&letter)?)
            .ignore()
            .cmd("LTRIM")
            .arg(self.dead())
            .arg(-DEAD_LETTER_CAP)
            .arg(-1)
            .ignore()
            .exec_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)
    }

    async fn dead_letters(&self) -> DomainResult<Vec<DeadLetter>> {
        Ok(self
            .raw_dead_letters()
            .await?
            .into_iter()
            .map(|(_, letter)| letter)
            .collect())
    }

    async fn take_dead_letters(
        &self,
        select: &(dyn for<'a> Fn(&'a DeadLetter) -> bool + Sync),
        limit: usize,
    ) -> DomainResult<Vec<DeadLetter>> {
        let mut taken = Vec::new();
        for (raw, letter) in self.raw_dead_letters().await? {
            if taken.len() == limit {
                break;
            }
            if !select(&letter) {
                continue;
            }
            // LREM decides the race: only the node that removes it requeues it.
            let removed: i64 = cmd("LREM")
                .arg(self.dead())
                .arg(1)
                .arg(&raw)
                .query_async(&mut self.conn.clone())
                .await
                .map_err(unavailable)?;
            if removed == 1 {
                taken.push(letter);
            }
        }
        Ok(taken)
    }

    async fn counts(&self) -> DomainResult<(usize, usize)> {
        let (ready, delayed, dead): (usize, usize, usize) = pipe()
            .cmd("LLEN")
            .arg(self.ready())
            .cmd("ZCARD")
            .arg(self.delayed())
            .cmd("LLEN")
            .arg(self.dead())
            .query_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)?;
        Ok((ready + delayed, dead))
    }
}
