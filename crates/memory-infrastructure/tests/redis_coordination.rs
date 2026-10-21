//! Redis coordination adapters against a real server. Skipped unless
//! `AGENT_MEMORY_TEST_REDIS_URL` is set, e.g.
//!
//! ```sh
//! docker run -d --rm --name amem-redis-test -p 26379:6379 redis:8.4.0
//! AGENT_MEMORY_TEST_REDIS_URL=redis://127.0.0.1:26379 cargo test --workspace
//! ```
//!
//! Every test uses its own random key prefix, so tests run in parallel and
//! leftovers of earlier runs never interfere.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use memory_application::ports::coordination::KeyLock;
use memory_application::ports::pipeline::{
    DeadLetter, DeadLetterFilter, PipelineJob, PipelineStateRepository, ProfileState, SessionState,
};
use memory_domain::profile::ProfileScope;
use memory_domain::scope::{DepartmentId, MemoryScope, SegmentId};
use memory_infrastructure::redis::{
    connect, KeySpace, RedisKeyLock, RedisPipelineState, RedisQueue,
};
use memory_infrastructure::worker::{Envelope, QueueBackend};
use redis::aio::ConnectionManager;

const ENV: &str = "AGENT_MEMORY_TEST_REDIS_URL";
/// Long enough that no test job is redelivered unless a test wants it.
const LEASE: Duration = Duration::from_secs(60);

/// A fresh namespace on the test server, or `None` (test skipped).
async fn redis() -> Option<(String, KeySpace)> {
    let Ok(url) = std::env::var(ENV) else {
        eprintln!("skipping: set {ENV} to run Redis integration tests");
        return None;
    };
    let keys = KeySpace::new(&format!("amem-test-{}", uuid::Uuid::new_v4()));
    Some((url, keys))
}

async fn conn(url: &str) -> ConnectionManager {
    connect(url).await.expect("test redis must be reachable")
}

fn id(raw: &str) -> DepartmentId {
    SegmentId::parse("department", raw).unwrap()
}

fn scope(team: &str, agent: &str, user: &str) -> MemoryScope {
    MemoryScope::from_optional(Some(team), Some(agent), Some(user), None).unwrap()
}

fn profile(team: &str, agent: &str) -> ProfileScope {
    ProfileScope::from(&scope(team, agent, "u"))
}

fn session(
    department: &str,
    scope: MemoryScope,
    sid: &str,
    due: Option<DateTime<Utc>>,
    active: DateTime<Utc>,
) -> SessionState {
    SessionState {
        department: id(department),
        scope,
        session_id: sid.into(),
        conversation_count: 3,
        warmup: 1,
        l1_cursor: Some(7),
        l1_due_at: due,
        last_l1_at: None,
        last_scene_name: Some("scene".into()),
        last_active_at: active,
    }
}

fn ms(t: DateTime<Utc>) -> DateTime<Utc> {
    // JSON round-trips keep full precision; truncate anyway so equality
    // does not depend on the serializer.
    DateTime::from_timestamp_millis(t.timestamp_millis()).unwrap()
}

#[tokio::test]
async fn state_round_trips_and_due_indexes_follow_updates() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let store = RedisPipelineState::new(conn(&url).await, &keys);
    let now = ms(Utc::now());
    let past = now - chrono::Duration::seconds(5);
    let future = now + chrono::Duration::hours(1);

    let due = session("ins", scope("t", "a", "u1"), "s1", Some(past), past);
    let later = session("ins", scope("t", "a", "u2"), "s2", Some(future), now);
    let idle = session("ins", scope("t", "a", "u3"), "s/3|x", None, future);
    for s in [&due, &later, &idle] {
        store.put_session(s).await.unwrap();
    }
    let got = store
        .get_session(&due.department, &due.scope, "s1")
        .await
        .unwrap();
    assert_eq!(got.as_ref(), Some(&due));
    assert_eq!(
        store
            .get_session(&idle.department, &idle.scope, "s/3|x")
            .await
            .unwrap()
            .as_ref(),
        Some(&idle),
        "session ids may contain separators"
    );
    assert_eq!(store.due_sessions(now).await.unwrap(), vec![due.clone()]);

    // Clearing the deadline must remove it from the due index.
    let mut cleared = due.clone();
    cleared.l1_due_at = None;
    store.put_session(&cleared).await.unwrap();
    assert!(store.due_sessions(now).await.unwrap().is_empty());
    assert_eq!(
        store.due_sessions(future).await.unwrap(),
        vec![later.clone()]
    );

    // Newest activity first, limited.
    let listed = store.list_sessions(&id("ins"), 2).await.unwrap();
    let names: Vec<_> = listed.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(names, ["s/3|x", "s2"]);
    assert!(store.list_sessions(&id("ins"), 0).await.unwrap().is_empty());

    let mut p = ProfileState::new(id("ins"), profile("t", "a"));
    p.l2_pending = true;
    p.l2_due_at = Some(past);
    p.l2_cursor = Some(9);
    store.put_profile(&p).await.unwrap();
    let other = ProfileState::new(id("ins"), profile("t", "b"));
    store.put_profile(&other).await.unwrap();
    assert_eq!(
        store.get_profile(&p.department, &p.profile).await.unwrap(),
        Some(p.clone())
    );
    assert_eq!(store.due_profiles(now).await.unwrap(), vec![p.clone()]);
    p.l2_due_at = None;
    store.put_profile(&p).await.unwrap();
    assert!(store.due_profiles(now).await.unwrap().is_empty());
    assert_eq!(store.list_profiles(&id("ins"), 10).await.unwrap().len(), 2);
}

#[tokio::test]
async fn delete_profile_and_department_remove_documents_and_indexes() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let store = RedisPipelineState::new(conn(&url).await, &keys);
    let now = ms(Utc::now());
    let past = now - chrono::Duration::seconds(1);

    let mine = session("ins", scope("t", "a", "u1"), "s1", Some(past), now);
    let sibling = session("ins", scope("t", "ab", "u1"), "s2", Some(past), now);
    let elsewhere = session("ins2", scope("t", "a", "u1"), "s3", Some(past), now);
    for s in [&mine, &sibling, &elsewhere] {
        store.put_session(s).await.unwrap();
    }
    let mut p = ProfileState::new(id("ins"), profile("t", "a"));
    p.l2_due_at = Some(past);
    store.put_profile(&p).await.unwrap();

    store
        .delete_profile(&id("ins"), &profile("t", "a"))
        .await
        .unwrap();
    assert_eq!(
        store.get_profile(&p.department, &p.profile).await.unwrap(),
        None
    );
    assert_eq!(
        store
            .get_session(&mine.department, &mine.scope, "s1")
            .await
            .unwrap(),
        None
    );
    assert!(store.due_profiles(now).await.unwrap().is_empty());
    let mut due: Vec<_> = store
        .due_sessions(now)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.session_id)
        .collect();
    due.sort();
    assert_eq!(
        due,
        ["s2", "s3"],
        "agent `ab` and department `ins2` survive"
    );

    store
        .put_profile(&ProfileState::new(id("ins"), profile("t", "ab")))
        .await
        .unwrap();
    store.delete_department(&id("ins")).await.unwrap();
    assert!(store
        .list_sessions(&id("ins"), 10)
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .list_profiles(&id("ins"), 10)
        .await
        .unwrap()
        .is_empty());
    let due = store.due_sessions(now).await.unwrap();
    assert_eq!(due, vec![elsewhere.clone()]);
    assert_eq!(store.list_sessions(&id("ins2"), 10).await.unwrap().len(), 1);
}

fn letter(job: PipelineJob, attempt: u32) -> DeadLetter {
    DeadLetter {
        job,
        attempt,
        error: Some("LLM_UNAVAILABLE: quota reached".into()),
        dead_at_ms: 1,
        redrives: 0,
        first_dead_at_ms: None,
        model: None,
    }
}

fn job(task: &str) -> PipelineJob {
    PipelineJob::SkillExtract {
        department: id("ins"),
        task_id: task.into(),
    }
}

#[tokio::test]
async fn queue_coalesces_waiting_duplicates_until_popped() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let q = RedisQueue::new(conn(&url).await, &keys, LEASE);
    q.push_new(job("t")).await.unwrap();
    q.push_new(job("t")).await.unwrap();
    q.push_new(job("other")).await.unwrap();
    assert_eq!(q.counts().await.unwrap(), (2, 0));

    let first = q.pop(Duration::from_millis(200)).await.unwrap().unwrap();
    assert_eq!(
        first.envelope,
        Envelope {
            attempt: 0,
            ..Envelope::new(job("t"))
        }
    );
    q.push_new(job("t")).await.unwrap();
    assert_eq!(
        q.counts().await.unwrap(),
        (2, 0),
        "a popped job no longer coalesces"
    );

    let mut rest = Vec::new();
    while let Some(d) = q.pop(Duration::from_millis(50)).await.unwrap() {
        rest.push(d.envelope.job);
    }
    assert_eq!(rest, vec![job("other"), job("t")], "FIFO order");
}

#[tokio::test]
async fn queue_delays_jobs_and_records_dead_letters() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let q = RedisQueue::new(conn(&url).await, &keys, LEASE);
    let retry = Envelope {
        attempt: 2,
        ..Envelope::new(job("t"))
    };
    q.push_later(retry.clone(), Duration::from_millis(400))
        .await
        .unwrap();
    // Identical envelopes stay distinct in the delayed set.
    q.push_later(retry.clone(), Duration::from_millis(400))
        .await
        .unwrap();
    assert_eq!(
        q.counts().await.unwrap(),
        (2, 0),
        "delayed jobs count as pending"
    );
    assert_eq!(q.pop(Duration::from_millis(100)).await.unwrap(), None);
    for _ in 0..2 {
        let d = q.pop(Duration::from_secs(2)).await.unwrap().unwrap();
        assert_eq!(d.envelope, retry);
        q.ack(&d.receipt).await.unwrap();
    }

    q.dead_letter(letter(retry.job, retry.attempt))
        .await
        .unwrap();
    assert_eq!(q.counts().await.unwrap(), (0, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dead_letters_are_listed_and_taken_once() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let q = Arc::new(RedisQueue::new(conn(&url).await, &keys, LEASE));
    for i in 0..20 {
        q.dead_letter(letter(job(&format!("t{i}")), 3))
            .await
            .unwrap();
    }
    // A letter in the old shape, written before errors were recorded.
    let mut c = conn(&url).await;
    let old = serde_json::json!({"job": job("old"), "attempt": 3}).to_string();
    let _: i64 = redis::cmd("RPUSH")
        .arg(keys.child("queue").key("dead"))
        .arg(&old)
        .query_async(&mut c)
        .await
        .unwrap();
    let all = q.dead_letters().await.unwrap();
    assert_eq!(all.len(), 21);
    assert_eq!(
        all[0].error.as_deref(),
        Some("LLM_UNAVAILABLE: quota reached")
    );
    assert_eq!(all[20].error, None, "old letters parse without an error");

    let skills = DeadLetterFilter {
        department: Some(id("ins")),
        kind: Some("skill".into()),
    };
    let none = DeadLetterFilter {
        department: Some(id("elsewhere")),
        kind: None,
    };
    assert!(q
        .take_dead_letters(&|l| none.matches(l), 50)
        .await
        .unwrap()
        .is_empty());

    // Two nodes redriving at once never take the same letter.
    let (a, b) = tokio::join!(
        {
            let q = Arc::clone(&q);
            let f = skills.clone();
            async move { q.take_dead_letters(&|l| f.matches(l), 15).await.unwrap() }
        },
        {
            let q = Arc::clone(&q);
            let f = skills.clone();
            async move { q.take_dead_letters(&|l| f.matches(l), 15).await.unwrap() }
        },
    );
    let mut seen = HashSet::new();
    for l in a.iter().chain(&b) {
        assert!(seen.insert(l.job.serial_key()), "taken twice: {:?}", l.job);
    }
    assert_eq!(seen.len(), 21, "every letter taken exactly once");
    assert_eq!(q.counts().await.unwrap(), (0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_consumers_never_receive_the_same_job() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let producer = RedisQueue::new(conn(&url).await, &keys, LEASE);
    const N: usize = 60;
    for i in 0..N / 2 {
        producer.push_new(job(&format!("ready-{i}"))).await.unwrap();
        // These become due together and race through the promotion script.
        producer
            .push_later(
                Envelope {
                    attempt: 1,
                    ..Envelope::new(job(&format!("delayed-{i}")))
                },
                Duration::from_millis(150),
            )
            .await
            .unwrap();
    }
    let mut consumers = Vec::new();
    for _ in 0..3 {
        let q = Arc::new(RedisQueue::new(conn(&url).await, &keys, LEASE));
        consumers.push(tokio::spawn(async move {
            let mut got = Vec::new();
            while let Some(d) = q.pop(Duration::from_millis(500)).await.unwrap() {
                got.push(d.envelope.job.serial_key());
                q.ack(&d.receipt).await.unwrap();
            }
            got
        }));
    }
    let mut all = Vec::new();
    for c in consumers {
        all.extend(c.await.unwrap());
    }
    let unique: HashSet<_> = all.iter().collect();
    assert_eq!(all.len(), N, "every job delivered once: {all:?}");
    assert_eq!(unique.len(), N, "no job delivered twice");
    assert_eq!(producer.counts().await.unwrap(), (0, 0));
}

#[tokio::test]
async fn unacked_jobs_are_redelivered_after_their_lease() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let lease = Duration::from_millis(300);
    let crashed = RedisQueue::new(conn(&url).await, &keys, lease);
    let survivor = RedisQueue::new(conn(&url).await, &keys, lease);
    let retry = Envelope {
        attempt: 2,
        ..Envelope::new(job("t"))
    };
    crashed
        .push_later(retry.clone(), Duration::ZERO)
        .await
        .unwrap();
    let lost = crashed.pop(Duration::from_secs(1)).await.unwrap().unwrap();
    // The first node "crashes": no ack, no extend.
    assert_eq!(
        survivor.pop(Duration::from_millis(100)).await.unwrap(),
        None
    );
    let again = survivor.pop(Duration::from_secs(2)).await.unwrap().unwrap();
    assert_eq!(again.envelope, retry, "redelivered with its attempt count");
    assert_ne!(again.receipt, lost.receipt);
    survivor.ack(&again.receipt).await.unwrap();
    // A late ack of the stale receipt is harmless.
    crashed.ack(&lost.receipt).await.unwrap();
    assert_eq!(
        survivor.pop(Duration::from_millis(600)).await.unwrap(),
        None
    );
    assert_eq!(survivor.counts().await.unwrap(), (0, 0));
}

#[tokio::test]
async fn acked_jobs_never_come_back() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let q = RedisQueue::new(conn(&url).await, &keys, Duration::from_millis(200));
    q.push_new(job("t")).await.unwrap();
    let d = q.pop(Duration::from_secs(1)).await.unwrap().unwrap();
    q.ack(&d.receipt).await.unwrap();
    assert_eq!(q.pop(Duration::from_millis(700)).await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extended_leases_are_not_redelivered() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let lease = Duration::from_millis(300);
    let runner = RedisQueue::new(conn(&url).await, &keys, lease);
    let other = RedisQueue::new(conn(&url).await, &keys, lease);
    assert_eq!(runner.extend_interval(), Some(lease / 3));
    runner.push_new(job("long")).await.unwrap();
    let d = runner.pop(Duration::from_secs(1)).await.unwrap().unwrap();
    for _ in 0..10 {
        tokio::time::sleep(lease / 3).await;
        runner.extend(&d.receipt).await.unwrap();
        assert_eq!(
            other.pop(Duration::ZERO).await.unwrap(),
            None,
            "a running job must not be redelivered"
        );
    }
    // Once the runner stops extending, the job comes back.
    let again = other.pop(Duration::from_secs(2)).await.unwrap().unwrap();
    assert_eq!(again.envelope, d.envelope);
}

#[tokio::test]
async fn redelivery_does_not_break_coalescing_of_newer_jobs() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let q = RedisQueue::new(conn(&url).await, &keys, Duration::from_millis(200));
    q.push_new(job("t")).await.unwrap();
    let _lost = q.pop(Duration::from_secs(1)).await.unwrap().unwrap();
    q.push_new(job("t")).await.unwrap(); // a fresh, waiting job
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Pops the redelivered job first; the newer marker must survive it.
    let redelivered = q.pop(Duration::from_secs(1)).await.unwrap().unwrap();
    q.ack(&redelivered.receipt).await.unwrap();
    q.push_new(job("t")).await.unwrap();
    assert_eq!(q.counts().await.unwrap(), (1, 0), "still coalesced");
}

async fn eventually_locks(lock: &RedisKeyLock, key: &str) -> bool {
    for _ in 0..100 {
        if lock.try_lock(key).await.unwrap().is_some() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

#[tokio::test]
async fn locks_are_exclusive_across_departments_and_released_on_drop() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let ttl = Duration::from_secs(10);
    let a = RedisKeyLock::new(conn(&url).await, keys.child("lock:job"), ttl);
    let b = RedisKeyLock::new(conn(&url).await, keys.child("lock:job"), ttl);
    let other_space = RedisKeyLock::new(conn(&url).await, keys.child("lock:state"), ttl);

    let guard = a.try_lock("k").await.unwrap().expect("free key");
    assert!(b.try_lock("k").await.unwrap().is_none());
    assert!(a.try_lock("k").await.unwrap().is_none(), "not reentrant");
    assert!(
        b.try_lock("k2").await.unwrap().is_some(),
        "other keys are free"
    );
    assert!(
        other_space.try_lock("k").await.unwrap().is_some(),
        "lock spaces are separate"
    );

    // `lock` waits for the holder instead of failing.
    let b = Arc::new(b);
    let waiter = {
        let b = Arc::clone(&b);
        tokio::spawn(async move { b.lock("k").await.map(|_| ()) })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!waiter.is_finished());
    drop(guard);
    tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("waiter acquires after release")
        .unwrap()
        .unwrap();
    // The waiter's guard was dropped at the end of its task.
    assert!(eventually_locks(&a, "k").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renewal_keeps_a_lock_past_its_ttl() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let ttl = Duration::from_millis(300);
    let a = RedisKeyLock::new(conn(&url).await, keys.child("lock:job"), ttl);
    let b = RedisKeyLock::new(conn(&url).await, keys.child("lock:job"), ttl);

    let guard = a.try_lock("long-job").await.unwrap().expect("free key");
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            b.try_lock("long-job").await.unwrap().is_none(),
            "a held lock must not expire while its guard lives"
        );
    }
    drop(guard);
    assert!(eventually_locks(&b, "long-job").await);
}

/// Runs without a server: start-up must fail fast with a clear error.
#[tokio::test]
async fn connect_fails_fast_when_redis_is_unreachable() {
    let started = std::time::Instant::now();
    let err = connect("redis://127.0.0.1:1/0")
        .await
        .expect_err("nothing listens on port 1");
    assert!(err.to_string().contains("redis"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test]
async fn dead_letters_are_capped_to_the_newest() {
    let Some((url, keys)) = redis().await else {
        return;
    };
    let q = RedisQueue::new(conn(&url).await, &keys, LEASE);
    for attempt in 0..10_005 {
        q.dead_letter(letter(job("t"), attempt)).await.unwrap();
    }
    assert_eq!(q.counts().await.unwrap(), (0, 10_000));
}
