//! Versioned migrations of data written by an older version, against live
//! servers. Skipped unless `AGENT_MEMORY_TEST_POSTGRES_URL` /
//! `AGENT_MEMORY_TEST_REDIS_URL` are set (see `postgres_stores.rs` and
//! `redis_coordination.rs`).
//!
//! Each test writes with the current adapters, rewrites what was written
//! into the old layout, then checks the migration brings it back.

use std::time::Duration;

use chrono::Utc;
use memory_application::ports::pipeline::{
    DeadLetter, PipelineJob, PipelineStateRepository, SessionState,
};
use memory_domain::scope::{DepartmentId, MemoryScope, SegmentId};
use memory_infrastructure::postgres::db::{quote_ident, schema_name, PgDb};
use memory_infrastructure::redis::{connect, migrate, KeySpace, RedisPipelineState, RedisQueue};
use memory_infrastructure::worker::QueueBackend;
use redis::AsyncCommands;
use serde_json::Value;
use sqlx::{Connection, Executor, PgConnection};

fn department(raw: &str) -> DepartmentId {
    SegmentId::parse("department", raw).unwrap()
}

fn scope() -> MemoryScope {
    MemoryScope::from_optional(Some("team-1"), Some("agt-1"), Some("u-1"), None).unwrap()
}

/// The inverse of migration 1 on a JSON document.
fn to_instance(doc: &str) -> String {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(map) => {
                if let Some(id) = map.remove("department") {
                    map.insert("instance".into(), id);
                }
                map.values_mut().for_each(walk);
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut v: Value = serde_json::from_str(doc).unwrap();
    walk(&mut v);
    v.to_string()
}

/// `url` with its database replaced by `name`.
fn with_database(url: &str, name: &str) -> String {
    let (base, query) = url
        .split_once('?')
        .map_or((url, None), |(b, q)| (b, Some(q)));
    let (server, _) = base.rsplit_once('/').unwrap();
    match query {
        Some(q) => format!("{server}/{name}?{q}"),
        None => format!("{server}/{name}"),
    }
}

#[tokio::test]
async fn postgres_data_from_0_2_is_migrated() {
    let Ok(url) = std::env::var("AGENT_MEMORY_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: set AGENT_MEMORY_TEST_POSTGRES_URL to run the Postgres migration test"
        );
        return;
    };
    // The registry is shared by the whole database, so this test gets its own.
    let name = format!("amem_migrate_{}", uuid::Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&url).await.unwrap();
    admin
        .execute(format!("CREATE DATABASE {name}").as_str())
        .await
        .unwrap();
    let db_url = with_database(&url, &name);

    let dept = department("legacy");
    let s = quote_ident(&schema_name(&dept));
    {
        let db = PgDb::connect(&db_url, 2, None).await.unwrap();
        db.schema(&dept).await.unwrap();
        let mut c = db.conn().await.unwrap();
        c.execute(
            format!(
                r#"INSERT INTO {s}.prompt_settings VALUES ('instance', '', '', 'l1', 'p-1', 1);
                 UPDATE {s}.meta_config_params SET param_name = 'max_users_per_instance', param_value = '20'
                 WHERE param_name = 'max_users_per_department';
                 INSERT INTO {s}.meta_config_params VALUES ('global', '', 'quota', 'max_users_per_department', '500');
                 INSERT INTO {s}.meta_team_members VALUES ('t-1', 'u-1', 'active', '', '{{"team_id":"t-1","user_id":"u-1","role":"reviewer"}}');
                 ALTER TABLE agent_memory.departments RENAME COLUMN department_id TO instance_id;
                 ALTER TABLE agent_memory.departments RENAME TO instances;
                 ALTER INDEX agent_memory.departments_pkey RENAME TO instances_pkey;
                 DROP TABLE agent_memory.schema_migrations;"#
            )
            .as_str(),
        )
        .await
        .unwrap();
        db.pool().close().await;
    }

    for _ in 0..2 {
        let db = PgDb::connect(&db_url, 2, None).await.unwrap();
        let mut c = db.conn().await.unwrap();
        let (id, target, users, role, version): (String, String, String, String, i64) =
            sqlx::query_as(&format!(
                "SELECT (SELECT department_id FROM agent_memory.departments),
                        (SELECT target_type FROM {s}.prompt_settings),
                        (SELECT param_value FROM {s}.meta_config_params WHERE param_name = 'max_users_per_department'),
                        (SELECT doc->>'role' FROM {s}.meta_team_members),
                        (SELECT max(version) FROM agent_memory.schema_migrations)"
            ))
            .fetch_one(&mut *c)
            .await
            .unwrap();
        assert_eq!(
            (
                id.as_str(),
                target.as_str(),
                users.as_str(),
                role.as_str(),
                version
            ),
            ("legacy", "department", "20", "member", 2)
        );
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_class WHERE relname IN ('instances', 'instances_pkey')",
        )
        .fetch_one(&mut *c)
        .await
        .unwrap();
        assert_eq!(left, 0);
        drop(c);
        db.pool().close().await;
    }

    admin
        .execute(format!("DROP DATABASE {name} WITH (FORCE)").as_str())
        .await
        .unwrap();
}

#[tokio::test]
async fn redis_instance_keys_and_documents_are_renamed() {
    let Ok(url) = std::env::var("AGENT_MEMORY_TEST_REDIS_URL") else {
        eprintln!("skipping: set AGENT_MEMORY_TEST_REDIS_URL to run the Redis migration test");
        return;
    };
    let conn = connect(&url).await.unwrap();
    let keys = KeySpace::new(&format!("amem-test-{}", uuid::Uuid::new_v4()));
    let dept = department("legacy");
    let now = Utc::now();

    let state = RedisPipelineState::new(conn.clone(), &keys);
    let queue = RedisQueue::new(conn.clone(), &keys, Duration::from_secs(60));
    state
        .put_session(&SessionState {
            department: dept.clone(),
            scope: scope(),
            session_id: "s-1".into(),
            conversation_count: 1,
            warmup: 0,
            l1_cursor: None,
            l1_due_at: None,
            last_l1_at: None,
            last_scene_name: None,
            last_active_at: now,
        })
        .await
        .unwrap();
    let job = PipelineJob::L1 {
        department: dept.clone(),
        scope: scope(),
        session_id: "s-1".into(),
        range: None,
    };
    queue.push_new(job.clone()).await.unwrap();
    queue
        .dead_letter(DeadLetter {
            job: job.clone(),
            attempt: 3,
            error: Some("boom".into()),
            dead_at_ms: 1,
        })
        .await
        .unwrap();

    // Back to the 0.2 layout.
    let mut c = conn.clone();
    let _: () = c
        .rename(
            keys.key("pipe:department:legacy:sessions"),
            keys.key("pipe:inst:legacy:sessions"),
        )
        .await
        .unwrap();
    let ids: Vec<String> = c
        .zrange(keys.key("pipe:inst:legacy:sessions"), 0, -1)
        .await
        .unwrap();
    let session_key = keys.key(&format!("pipe:session:{}", ids[0]));
    let doc: String = c.get(&session_key).await.unwrap();
    let _: () = c.set(&session_key, to_instance(&doc)).await.unwrap();
    for list in [keys.key("queue:ready"), keys.key("queue:dead")] {
        let items: Vec<String> = c.lrange(&list, 0, -1).await.unwrap();
        let _: () = c.del(&list).await.unwrap();
        for item in items {
            let _: () = c.rpush(&list, to_instance(&item)).await.unwrap();
        }
    }
    assert!(
        queue.dead_letters().await.unwrap().is_empty(),
        "old letters are unreadable"
    );

    migrate::migrate(&conn, &keys).await.unwrap();
    migrate::migrate(&conn, &keys).await.unwrap();

    let sessions = state.list_sessions(&dept, 10).await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].department, dept);
    let delivery = queue
        .pop(Duration::from_millis(100))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivery.envelope.job, job);
    queue.ack(&delivery.receipt).await.unwrap();
    let letters = queue.dead_letters().await.unwrap();
    assert_eq!(letters.len(), 1);
    assert_eq!(letters[0].job, job);
    let version: i64 = c.get(keys.key("schema_version")).await.unwrap();
    assert_eq!(version, migrate::SCHEMA_VERSION);
    let old: Vec<String> = c
        .keys(format!("{}*", keys.key("pipe:inst:")))
        .await
        .unwrap();
    assert!(old.is_empty());

    let all: Vec<String> = c.keys(format!("{}*", keys.key(""))).await.unwrap();
    let _: () = c.del(all).await.unwrap();
}
