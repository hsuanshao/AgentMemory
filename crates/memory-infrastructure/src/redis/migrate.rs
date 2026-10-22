//! Versioned changes to what is stored in Redis. `{p}:schema_version` holds
//! the version reached; [`migrate`] runs the missing steps at start-up while
//! holding `{p}:lock:migrate`, so one replica migrates and the others wait.
//!
//! 1. 0.3.0: "instance" became "department": `{p}:pipe:inst:*` indexes are
//!    renamed and the `instance` field of every state document, queued job
//!    and dead letter is renamed.

use std::time::Duration;

use ::redis::aio::ConnectionManager;
use ::redis::{cmd, pipe, AsyncCommands};
use memory_domain::error::{DomainError, DomainResult};
use serde_json::Value;
use tracing::info;

use super::{unavailable, KeySpace};

/// The latest version.
pub const SCHEMA_VERSION: i64 = 1;

const LOCK_TTL: Duration = Duration::from_secs(60);
/// How long a replica waits for another one to finish migrating.
const WAIT: Duration = Duration::from_secs(120);

pub async fn migrate(conn: &ConnectionManager, prefix: &KeySpace) -> DomainResult<()> {
    let mut c = conn.clone();
    let version_key = prefix.key("schema_version");
    let lock_key = prefix.key("lock:migrate");
    let token = uuid::Uuid::new_v4().to_string();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let found = version(&mut c, &version_key).await?;
        if found > SCHEMA_VERSION {
            // Written by a newer memory-server, whose state this one would
            // misread.
            return Err(DomainError::unavailable(format!(
                "the redis state is at schema version {found}, newer than this memory-server \
                 knows ({SCHEMA_VERSION}): run the newer version, or restore the backup taken \
                 before the upgrade (docs/UPGRADE.md, Roll back)"
            )));
        }
        if found == SCHEMA_VERSION {
            return Ok(());
        }
        let locked: Option<String> = cmd("SET")
            .arg(&lock_key)
            .arg(&token)
            .arg("NX")
            .arg("PX")
            .arg(LOCK_TTL.as_millis() as u64)
            .query_async(&mut c)
            .await
            .map_err(unavailable)?;
        if locked.is_some() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(DomainError::unavailable(format!(
                "another replica has held {lock_key} for too long; if none is migrating, delete the key and restart"
            )));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let result = async {
        // Checked again under the lock: another replica may have finished.
        let from = version(&mut c, &version_key).await?;
        if from < 1 {
            rename_instance(&mut c, prefix).await?;
        }
        if from < SCHEMA_VERSION {
            let _: () = c
                .set(&version_key, SCHEMA_VERSION)
                .await
                .map_err(unavailable)?;
            info!(
                from,
                to = SCHEMA_VERSION,
                "migrated redis coordination data"
            );
        }
        Ok(())
    }
    .await;
    // Only our own lock: it may have expired and been taken by now.
    let _: i64 = ::redis::Script::new(
        "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) end return 0",
    )
    .key(&lock_key)
    .arg(&token)
    .invoke_async(&mut c)
    .await
    .map_err(unavailable)?;
    result
}

async fn version(c: &mut ConnectionManager, key: &str) -> DomainResult<i64> {
    let v: Option<i64> = c.get(key).await.map_err(unavailable)?;
    Ok(v.unwrap_or(0))
}

async fn scan(c: &mut ConnectionManager, pattern: &str) -> DomainResult<Vec<String>> {
    let mut keys = Vec::new();
    let mut cursor: u64 = 0;
    loop {
        let (next, batch): (u64, Vec<String>) = cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(pattern)
            .arg("COUNT")
            .arg(1000)
            .query_async(c)
            .await
            .map_err(unavailable)?;
        keys.extend(batch);
        if next == 0 {
            return Ok(keys);
        }
        cursor = next;
    }
}

/// `prefix` with glob characters escaped, for `SCAN MATCH`.
fn glob_escape(s: &str) -> String {
    s.chars()
        .flat_map(|ch| match ch {
            '*' | '?' | '[' | ']' | '\\' => vec!['\\', ch],
            _ => vec![ch],
        })
        .collect()
}

/// Migration 1.
async fn rename_instance(c: &mut ConnectionManager, prefix: &KeySpace) -> DomainResult<()> {
    let pipe_keys = prefix.child("pipe");
    let old_index = pipe_keys.key("inst:");
    let new_index = pipe_keys.key("department:");
    for old in scan(c, &format!("{}*", glob_escape(&old_index))).await? {
        let new = format!("{new_index}{}", &old[old_index.len()..]);
        // Both exist only if a new replica wrote before this ran: merge.
        let mut p = pipe();
        p.atomic()
            .cmd("ZUNIONSTORE")
            .arg(&new)
            .arg(2)
            .arg(&new)
            .arg(&old)
            .arg("AGGREGATE")
            .arg("MAX")
            .ignore()
            .cmd("DEL")
            .arg(&old)
            .ignore();
        let _: () = p.query_async(c).await.map_err(unavailable)?;
    }

    for kind in ["session", "profile"] {
        let pattern = format!("{}*", glob_escape(&pipe_keys.key(&format!("{kind}:"))));
        for key in scan(c, &pattern).await? {
            let doc: Option<String> = c.get(&key).await.map_err(unavailable)?;
            if let Some(doc) = doc.as_deref().and_then(renamed) {
                let _: () = cmd("SET")
                    .arg(&key)
                    .arg(doc)
                    .arg("KEEPTTL")
                    .query_async(c)
                    .await
                    .map_err(unavailable)?;
            }
        }
    }

    let queue = prefix.child("queue");
    for list in [queue.key("ready"), queue.key("dead")] {
        let items: Vec<String> = c.lrange(&list, 0, -1).await.map_err(unavailable)?;
        let rewritten: Vec<Option<String>> = items.iter().map(|i| renamed(i)).collect();
        if rewritten.iter().all(Option::is_none) {
            continue;
        }
        let items: Vec<String> = items
            .into_iter()
            .zip(rewritten)
            .map(|(old, new)| new.unwrap_or(old))
            .collect();
        let mut p = pipe();
        p.atomic().del(&list).ignore().rpush(&list, items).ignore();
        let _: () = p.query_async(c).await.map_err(unavailable)?;
    }

    let delayed = queue.key("delayed");
    let entries: Vec<(String, f64)> = c
        .zrange_withscores(&delayed, 0, -1)
        .await
        .map_err(unavailable)?;
    for (item, score) in entries {
        if let Some(new) = renamed(&item) {
            let mut p = pipe();
            p.atomic()
                .zrem(&delayed, &item)
                .ignore()
                .zadd(&delayed, new, score)
                .ignore();
            let _: () = p.query_async(c).await.map_err(unavailable)?;
        }
    }

    let inflight = queue.key("inflight");
    let leased: Vec<(String, String)> = c.hgetall(&inflight).await.map_err(unavailable)?;
    for (receipt, item) in leased {
        if let Some(new) = renamed(&item) {
            let _: () = c.hset(&inflight, receipt, new).await.map_err(unavailable)?;
        }
    }
    Ok(())
}

/// The document with every `instance` field renamed to `department`, or
/// `None` when it has none (or is not JSON).
fn renamed(doc: &str) -> Option<String> {
    fn walk(v: &mut Value) -> bool {
        match v {
            Value::Object(map) => {
                let mut changed = false;
                if let Some(id) = map.remove("instance") {
                    map.insert("department".to_owned(), id);
                    changed = true;
                }
                for child in map.values_mut() {
                    changed |= walk(child);
                }
                changed
            }
            Value::Array(items) => items.iter_mut().fold(false, |acc, i| walk(i) | acc),
            _ => false,
        }
    }
    let mut value: Value = serde_json::from_str(doc).ok()?;
    walk(&mut value).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_the_field_wherever_it_is() {
        let job = r#"{"id":"x","dedupe":null,"envelope":{"job":{"L1":{"instance":"default","session_id":"s"}},"attempt":0}}"#;
        let new: Value = serde_json::from_str(&renamed(job).unwrap()).unwrap();
        assert_eq!(new["envelope"]["job"]["L1"]["department"], "default");
        assert!(new["envelope"]["job"]["L1"].get("instance").is_none());

        assert_eq!(
            renamed(r#"{"instance":"d","x":1}"#).unwrap(),
            r#"{"department":"d","x":1}"#
        );
        assert_eq!(renamed(r#"{"department":"d"}"#), None);
        assert_eq!(renamed("not json"), None);
    }

    #[test]
    fn glob_characters_are_escaped() {
        assert_eq!(glob_escape("am[1]*"), r"am\[1\]\*");
    }
}
