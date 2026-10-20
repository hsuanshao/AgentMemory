//! Redis coordination: pipeline scheduling state, the job queue and
//! distributed locks, shared by every replica pointing at the same Redis.
//!
//! Every key lives under one configurable prefix:
//!
//! | key                                   | type   | holds                          |
//! |---------------------------------------|--------|--------------------------------|
//! | `{p}:pipe:session:{id}`               | string | `SessionState` JSON            |
//! | `{p}:pipe:profile:{id}`               | string | `ProfileState` JSON            |
//! | `{p}:pipe:due:sessions` / `:profiles` | zset   | id → due ms (only when due)    |
//! | `{p}:pipe:department:{id}:sessions`   | zset   | id → last active ms            |
//! | `{p}:pipe:department:{id}:profiles`   | zset   | id → updated ms                |
//! | `{p}:queue:ready`                     | list   | jobs ready to run              |
//! | `{p}:queue:delayed`                   | zset   | job → not-before ms            |
//! | `{p}:queue:processing`                | zset   | receipt → lease deadline ms    |
//! | `{p}:queue:inflight`                  | hash   | receipt → leased job           |
//! | `{p}:queue:dedupe:{dedupe_key}`       | string | item id of a waiting first try |
//! | `{p}:queue:dead`                      | list   | newest 10,000 dead letters     |
//! | `{p}:lock:state:{key}` / `:job:{key}` | string | lock owner token (with PX TTL) |
//! | `{p}:lock:migrate`                    | string | held while migrating           |
//! | `{p}:schema_version`                  | string | version reached, see `migrate` |
//!
//! Scripts touch keys they derive from arguments, so Redis Cluster is not
//! supported; use a single primary (with replicas/Sentinel for failover).

mod lock;
pub mod migrate;
mod queue;
mod state;

use std::time::Duration;

use ::redis::aio::{ConnectionManager, ConnectionManagerConfig};
use ::redis::{Client, RedisError};
use memory_domain::error::{DomainError, DomainResult};

pub use lock::RedisKeyLock;
pub use queue::RedisQueue;
pub use state::RedisPipelineState;

/// Bounds start-up so a missing Redis fails the boot instead of hanging it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// No command we send blocks server-side, so a slow reply means trouble.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Opens a self-reconnecting connection and checks the server answers.
pub async fn connect(url: &str) -> DomainResult<ConnectionManager> {
    let client =
        Client::open(url).map_err(|e| DomainError::invalid(format!("invalid redis url: {e}")))?;
    let config = ConnectionManagerConfig::new()
        .set_connection_timeout(Some(CONNECT_TIMEOUT))
        .set_response_timeout(Some(RESPONSE_TIMEOUT))
        .set_number_of_retries(2);
    let mut conn = tokio::time::timeout(
        CONNECT_TIMEOUT * 3,
        ConnectionManager::new_with_config(client, config),
    )
    .await
    .map_err(|_| DomainError::unavailable("redis connection timed out"))?
    .map_err(unavailable)?;
    ::redis::cmd("PING")
        .query_async::<String>(&mut conn)
        .await
        .map_err(unavailable)?;
    Ok(conn)
}

/// The root namespace (`key_prefix`) all keys of this deployment share.
#[derive(Debug, Clone)]
pub struct KeySpace(String);

impl KeySpace {
    pub fn new(prefix: &str) -> Self {
        Self(prefix.trim_end_matches(':').to_owned())
    }

    /// `{prefix}:{suffix}`.
    pub fn key(&self, suffix: &str) -> String {
        format!("{}:{suffix}", self.0)
    }

    /// A child namespace, e.g. `lock:job`.
    pub fn child(&self, name: &str) -> Self {
        Self(self.key(name))
    }
}

fn unavailable(e: RedisError) -> DomainError {
    DomainError::unavailable(format!("redis: {e}"))
}

fn to_json<T: serde::Serialize>(value: &T) -> DomainResult<String> {
    serde_json::to_string(value)
        .map_err(|e| DomainError::internal(format!("failed to encode redis document: {e}")))
}

fn from_json<T: serde::de::DeserializeOwned>(doc: &str) -> DomainResult<T> {
    serde_json::from_str(doc)
        .map_err(|e| DomainError::internal(format!("corrupt redis document: {e}")))
}
