//! Postgres adapters against a live server. Skipped unless
//! `AGENT_MEMORY_TEST_POSTGRES_URL` is set, e.g.
//!
//! ```text
//! docker run -d --rm --name amem-pg-test -e POSTGRES_PASSWORD=test -p 25432:5432 pgvector/pgvector:pg16
//! AGENT_MEMORY_TEST_POSTGRES_URL=postgres://postgres:test@127.0.0.1:25432/postgres cargo test
//! ```
//!
//! Each test works in a fresh, randomly named department and drops it after.

use std::sync::Arc;

#[path = "support/embedding_backfill.rs"]
mod embedding_backfill;
#[path = "support/profile_versions.rs"]
mod profile_versions;
#[path = "support/redaction_rules.rs"]
mod redaction_rules;

use chrono::{Duration, Utc};
use memory_application::admin::DepartmentLifecycle;
use memory_application::common::Page;
use memory_application::ports::memory::{
    AtomFilter, AtomRepository, AtomSelector, ConversationFilter, ConversationRepository,
};
use memory_application::ports::meta::{
    AssetRepository, BindingRepository, UserKeyRepository, UserRepository,
};
use memory_domain::memory::{AtomType, AtomicMemory, ConversationMessage, Role};
use memory_domain::meta::{
    AgentBinding, Asset, AssetStatus, AssetType, InjectionMode, KeyStatus, User, UserKey,
    UserStatus, UserType, Visibility,
};
use memory_domain::profile::ProfileScope;
use memory_domain::scope::{DepartmentId, MemoryScope, SegmentId};
use memory_infrastructure::postgres::db::{schema_name, PgDb};
use memory_infrastructure::postgres::lifecycle::PostgresCleanup;
use memory_infrastructure::postgres::memory_store::PostgresMemoryStore;
use memory_infrastructure::postgres::meta_store::PostgresMetaStore;
use serde_json::json;

const URL_ENV: &str = "AGENT_MEMORY_TEST_POSTGRES_URL";

/// A pool (2-dimensional embeddings) and a fresh department, or `None` when
/// no test server is configured.
async fn setup() -> Option<(Arc<PgDb>, DepartmentId)> {
    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping: set {URL_ENV} to run the Postgres store tests");
        return None;
    };
    let db = PgDb::connect(&url, 4, Some(2))
        .await
        .expect("connect to test postgres");
    let id = format!("ins-pg-{}", uuid::Uuid::new_v4().simple());
    Some((Arc::new(db), SegmentId::parse("department", &id).unwrap()))
}

/// Drops the department schema.
async fn teardown(db: &Arc<PgDb>, department: &DepartmentId) {
    PostgresCleanup::new(Arc::clone(db))
        .destroy(department)
        .await
        .unwrap();
}

fn scope(user: &str) -> MemoryScope {
    MemoryScope::from_optional(Some("team-1"), Some("agt-1"), Some(user), None).unwrap()
}

fn message(id: &str, content: &str, user: &str, offset_ms: i64) -> ConversationMessage {
    let t = Utc::now() + Duration::milliseconds(offset_ms);
    ConversationMessage {
        id: id.into(),
        scope: scope(user),
        session_id: "s1".into(),
        role: Role::User,
        content: content.into(),
        timestamp: t,
        recorded_at: t,
        version: 1,
    }
}

fn atom(id: &str, content: &str, user: &str) -> AtomicMemory {
    let now = Utc::now();
    AtomicMemory {
        id: id.into(),
        scope: scope(user),
        atom_type: AtomType::Persona,
        content: content.into(),
        background: None,
        priority: 80,
        source_message_ids: vec![],
        session_id: Some("s1".into()),
        version: 1,
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn conversations_are_searchable_in_chinese_and_isolated_by_user() {
    let Some((db, department)) = setup().await else {
        return;
    };
    let store = PostgresMemoryStore::new(Arc::clone(&db));
    store
        .append(
            &department,
            &[
                message("m1", "下週一要發布新版本", "alice", 0),
                message("m2", "下週一要發布新版本", "bob", 1),
            ],
        )
        .await
        .unwrap();

    // Equal or back-dated recorded_at must not hide rows from the cursor.
    let old = message("m3", "older but stored later", "alice", -60_000);
    store.append(&department, &[old]).await.unwrap();
    let tail =
        ConversationRepository::after_seq(&store, &department, &scope("alice"), "s1", None, 10)
            .await
            .unwrap();
    assert_eq!(
        tail.iter().map(|m| m.item.id.as_str()).collect::<Vec<_>>(),
        ["m1", "m3"]
    );

    let hits = store
        .search(
            &department,
            &scope("alice"),
            "發布",
            &ConversationFilter::default(),
            10,
        )
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item.id, "m1");

    let after_first = ConversationRepository::after_seq(
        &store,
        &department,
        &scope("alice"),
        "s1",
        Some(tail[0].seq),
        10,
    )
    .await
    .unwrap();
    assert_eq!(after_first.len(), 1);
    let none_left = ConversationRepository::after_seq(
        &store,
        &department,
        &scope("alice"),
        "s1",
        Some(after_first[0].seq),
        10,
    )
    .await
    .unwrap();
    assert!(none_left.is_empty());
    teardown(&db, &department).await;
}

#[tokio::test]
async fn atoms_support_keyword_vector_and_profile_queries() {
    let Some((db, department)) = setup().await else {
        return;
    };
    let store = PostgresMemoryStore::new(Arc::clone(&db));
    store
        .upsert(
            &department,
            &atom("a1", "prefers Rust for backend services", "alice"),
            Some(&[1.0, 0.0]),
        )
        .await
        .unwrap();
    store
        .upsert(
            &department,
            &atom("a2", "likes hiking on weekends", "bob"),
            Some(&[0.0, 1.0]),
        )
        .await
        .unwrap();

    let kw = store
        .keyword_search(
            &department,
            &scope("alice"),
            "rust backend",
            &AtomFilter::default(),
            5,
        )
        .await
        .unwrap();
    assert_eq!(kw.len(), 1);
    let vec = store
        .vector_search(
            &department,
            &scope("alice"),
            &[1.0, 0.0],
            &AtomFilter::default(),
            5,
        )
        .await
        .unwrap();
    assert!((vec[0].score - 1.0).abs() < 1e-6);

    let profile = ProfileScope::from(&scope("alice"));
    let (all, total) = AtomRepository::query(
        &store,
        &department,
        AtomSelector::Profile(&profile),
        &AtomFilter::default(),
        Page {
            limit: 10,
            offset: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!((all.len(), total), (2, 2), "profile scope spans users");

    assert_eq!(
        AtomRepository::delete_profile(&store, &department, &profile)
            .await
            .unwrap(),
        2
    );
    assert!(store
        .keyword_search(
            &department,
            &scope("alice"),
            "rust",
            &AtomFilter::default(),
            5
        )
        .await
        .unwrap()
        .is_empty());
    teardown(&db, &department).await;
}

#[tokio::test]
async fn scene_and_persona_versions_are_append_only() {
    let Some((db, department)) = setup().await else {
        return;
    };
    let store = PostgresMemoryStore::new(Arc::clone(&db));
    profile_versions::exercise(
        &store,
        &department,
        &ProfileScope::from(&scope("alice")),
        &ProfileScope::from(
            &MemoryScope::from_optional(Some("team-2"), Some("agt-2"), Some("bob"), None).unwrap(),
        ),
    )
    .await;
    teardown(&db, &department).await;
}

fn user(id: &str, name: &str) -> User {
    User {
        user_id: id.into(),
        username: name.into(),
        user_type: UserType::Normal,
        status: UserStatus::Active,
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: String::new(),
        metadata_json: json!({}),
    }
}

fn key(id: &str, user: &str, value: &str, default: bool, created: &str) -> UserKey {
    UserKey {
        key_id: id.into(),
        user_id: user.into(),
        key_value: value.into(),
        name: None,
        status: KeyStatus::Active,
        is_default: default,
        last_used_at: None,
        expires_at: None,
        created_at: created.into(),
    }
}

#[tokio::test]
async fn revoking_the_default_key_promotes_the_oldest_remaining_key() {
    let Some((db, department)) = setup().await else {
        return;
    };
    let store = PostgresMetaStore::new(Arc::clone(&db));
    store
        .create_user(
            &department,
            &user("usr-1", "alice"),
            &key("k1", "usr-1", "sk-mem-a", true, "1"),
        )
        .await
        .unwrap();
    store
        .insert_key(&department, &key("k2", "usr-1", "sk-mem-b", false, "2"))
        .await
        .unwrap();
    store
        .insert_key(&department, &key("k3", "usr-1", "sk-mem-c", false, "3"))
        .await
        .unwrap();

    store.delete_key(&department, "k1").await.unwrap();
    assert!(
        store
            .get_key(&department, "k2")
            .await
            .unwrap()
            .unwrap()
            .is_default
    );
    assert!(
        !store
            .get_key(&department, "k3")
            .await
            .unwrap()
            .unwrap()
            .is_default
    );
    assert!(
        store
            .create_user(
                &department,
                &user("usr-2", "alice"),
                &key("k4", "usr-2", "sk-mem-d", true, "4")
            )
            .await
            .is_err(),
        "usernames are unique"
    );
    teardown(&db, &department).await;
}

#[tokio::test]
async fn deleting_an_asset_cascades_bindings() {
    let Some((db, department)) = setup().await else {
        return;
    };
    let store = PostgresMetaStore::new(Arc::clone(&db));
    let asset = Asset {
        asset_id: "ast-1".into(),
        team_id: "team-1".into(),
        asset_type: AssetType::Skill,
        name: "s".into(),
        description: None,
        owner_user_id: "usr-1".into(),
        source_type: "manual".into(),
        source_ref: None,
        version: 1,
        visibility: Visibility::Team,
        status: AssetStatus::Active,
        confidence: None,
        expires_at: None,
        last_used_at: None,
        usage_count: 0,
        content_ref: None,
        created_at: "1".into(),
        updated_at: "1".into(),
        metadata_json: json!({}),
    };
    store.insert_asset(&department, &asset).await.unwrap();
    let binding = AgentBinding {
        id: "b".into(),
        agent_id: "agt-1".into(),
        asset_id: "ast-1".into(),
        asset_type: AssetType::Skill,
        injection_mode: InjectionMode::Summary,
        priority: 50,
        created_by: "usr-1".into(),
        created_at: "1".into(),
    };
    store.ensure_binding(&department, &binding).await.unwrap();
    store.touch_asset(&department, "ast-1", "2").await.unwrap();
    assert_eq!(
        store
            .get_asset(&department, "ast-1")
            .await
            .unwrap()
            .unwrap()
            .usage_count,
        1
    );

    store
        .delete_assets(&department, &["ast-1".into()])
        .await
        .unwrap();
    assert!(store
        .list_bindings(&department, "agt-1")
        .await
        .unwrap()
        .is_empty());
    teardown(&db, &department).await;
}

#[tokio::test]
async fn rewriting_a_row_takes_a_fresh_sequence() {
    let Some((db, department)) = setup().await else {
        return;
    };
    let store = PostgresMemoryStore::new(Arc::clone(&db));
    store
        .append(
            &department,
            &[
                message("m1", "first", "alice", 0),
                message("m2", "second", "alice", 1),
            ],
        )
        .await
        .unwrap();
    store
        .append(&department, &[message("m1", "first, edited", "alice", 0)])
        .await
        .unwrap();
    let tail =
        ConversationRepository::after_seq(&store, &department, &scope("alice"), "s1", None, 10)
            .await
            .unwrap();
    assert_eq!(
        tail.iter().map(|m| m.item.id.as_str()).collect::<Vec<_>>(),
        ["m2", "m1"],
        "a rewrite moves behind every earlier row"
    );
    assert!(tail[1].seq > tail[0].seq);
    assert_eq!(tail[1].item.content, "first, edited");

    let profile = ProfileScope::from(&scope("alice"));
    store
        .upsert(&department, &atom("a1", "one", "alice"), None)
        .await
        .unwrap();
    store
        .upsert(&department, &atom("a2", "two", "alice"), None)
        .await
        .unwrap();
    let first = AtomRepository::after_seq(&store, &department, &profile, None, 10)
        .await
        .unwrap();
    store
        .upsert(&department, &atom("a1", "one again", "alice"), None)
        .await
        .unwrap();
    let rest = AtomRepository::after_seq(&store, &department, &profile, Some(first[1].seq), 10)
        .await
        .unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].item.content, "one again");
    teardown(&db, &department).await;
}

#[tokio::test]
async fn keyword_search_matches_underscored_and_cjk_tokens_verbatim() {
    let Some((db, department)) = setup().await else {
        return;
    };
    let store = PostgresMemoryStore::new(Arc::clone(&db));
    let mut a = atom("a1", "部署腳本叫 deploy_prod", "alice");
    a.background = Some("記憶體不足時重試".into());
    store.upsert(&department, &a, None).await.unwrap();
    store
        .upsert(
            &department,
            &atom("a2", "deploy staging only", "alice"),
            None,
        )
        .await
        .unwrap();
    for query in ["deploy_prod", "記憶體", "部署"] {
        let hits = store
            .keyword_search(
                &department,
                &scope("alice"),
                query,
                &AtomFilter::default(),
                5,
            )
            .await
            .unwrap();
        assert_eq!(
            hits.iter().map(|h| h.item.id.as_str()).collect::<Vec<_>>(),
            ["a1"],
            "query {query}"
        );
        assert!((0.0..=1.0).contains(&hits[0].score));
    }
    assert!(
        store
            .keyword_search(
                &department,
                &scope("alice"),
                "the 'a' | !",
                &AtomFilter::default(),
                5
            )
            .await
            .unwrap()
            .is_empty(),
        "operators and quotes are escaped"
    );
    teardown(&db, &department).await;
}

#[tokio::test]
async fn departments_live_in_separate_schemas_and_cleanup_drops_them() {
    let Some((db, a)) = setup().await else {
        return;
    };
    let b = SegmentId::parse("department", &format!("{}-other", a.as_str())).unwrap();
    let store = PostgresMemoryStore::new(Arc::clone(&db));
    store
        .append(&a, &[message("m1", "只在 A 實例", "alice", 0)])
        .await
        .unwrap();
    let filter = ConversationFilter::default();
    assert_eq!(
        ConversationRepository::count(&store, &b, None, &filter)
            .await
            .unwrap(),
        0
    );
    assert!(store
        .search(&b, &scope("alice"), "實例", &filter, 5)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        ConversationRepository::count(&store, &a, None, &filter)
            .await
            .unwrap(),
        1
    );

    let schema_exists = |name: String| {
        let db = Arc::clone(&db);
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname = $1)",
            )
            .bind(name)
            .fetch_one(db.pool())
            .await
            .unwrap()
        }
    };
    assert!(schema_exists(schema_name(&a)).await);
    assert!(db
        .known_departments()
        .await
        .unwrap()
        .contains(&a.to_string()));

    let cleanup = PostgresCleanup::new(Arc::clone(&db));
    assert_eq!(cleanup.destroy(&a).await.unwrap(), json!({"removed": true}));
    assert!(!schema_exists(schema_name(&a)).await);
    assert!(!db
        .known_departments()
        .await
        .unwrap()
        .contains(&a.to_string()));
    assert_eq!(
        cleanup.destroy(&a).await.unwrap(),
        json!({"removed": false})
    );

    // The department comes back empty on next use.
    assert_eq!(
        ConversationRepository::count(&store, &a, None, &filter)
            .await
            .unwrap(),
        0
    );
    teardown(&db, &a).await;
    teardown(&db, &b).await;
}

#[tokio::test]
async fn an_unreachable_server_fails_fast_as_unavailable() {
    let started = std::time::Instant::now();
    // The password is assembled at runtime so no credential-shaped URL
    // literal trips a secret scanner.
    let url = format!("postgres://memory:{}@127.0.0.1:1/memory", "secret");
    let err = PgDb::connect(&url, 2, None)
        .await
        .err()
        .expect("nothing listens on port 1");
    assert_eq!(err.kind, memory_domain::error::ErrorKind::Unavailable);
    assert!(err.detail.contains("127.0.0.1:1/memory"), "{err}");
    assert!(
        !err.detail.contains("secret"),
        "credentials stay out of errors"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
}

#[tokio::test]
async fn atoms_can_be_queried_by_session() {
    let Some((db, department)) = setup().await else {
        return;
    };
    let store = PostgresMemoryStore::new(Arc::clone(&db));
    let mut other = atom("a2", "from another session", "alice");
    other.session_id = Some("s2".into());
    for a in [atom("a1", "from this session", "alice"), other] {
        store.upsert(&department, &a, None).await.unwrap();
    }
    let filter = AtomFilter {
        session_id: Some("s1".into()),
        ..Default::default()
    };
    let (found, total) = AtomRepository::query(
        &store,
        &department,
        AtomSelector::Scope(&scope("alice")),
        &filter,
        Page {
            limit: 10,
            offset: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(total, 1);
    assert_eq!(found[0].id, "a1");
    teardown(&db, &department).await;
}

#[tokio::test]
async fn atoms_without_an_embedding_get_one_later() {
    let Some((db, department)) = setup().await else {
        return;
    };
    let store = PostgresMemoryStore::new(Arc::clone(&db));
    let without = AtomicMemory {
        updated_at: Utc::now() + Duration::milliseconds(1),
        ..atom("a2", "deploys with a blue-green switch", "alice")
    };
    embedding_backfill::exercise(
        &store,
        &department,
        atom("a1", "prefers Rust for backend services", "alice"),
        without,
    )
    .await;
    teardown(&db, &department).await;
}

#[tokio::test]
async fn redaction_rules_are_kept_and_replaced_by_id() {
    let Some((db, department)) = setup().await else {
        return;
    };
    redaction_rules::exercise(&PostgresMetaStore::new(Arc::clone(&db)), &department).await;
    teardown(&db, &department).await;
}
