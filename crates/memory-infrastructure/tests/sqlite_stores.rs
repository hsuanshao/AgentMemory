use std::sync::Arc;

#[path = "support/embedding_backfill.rs"]
mod embedding_backfill;
#[path = "support/profile_versions.rs"]
mod profile_versions;

use chrono::{Duration, Utc};
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
use memory_infrastructure::sqlite::db::DepartmentPool;
use memory_infrastructure::sqlite::memory_store::SqliteMemoryStore;
use memory_infrastructure::sqlite::meta_store::SqliteMetaStore;
use serde_json::json;

fn setup() -> (tempfile::TempDir, Arc<DepartmentPool>, DepartmentId) {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(DepartmentPool::new(dir.path().to_path_buf(), 4));
    (
        dir,
        pool,
        SegmentId::parse("department", "ins-test").unwrap(),
    )
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
    let (_dir, pool, department) = setup();
    let store = SqliteMemoryStore::new(pool, None);
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
}

#[tokio::test]
async fn atoms_support_keyword_vector_and_profile_queries() {
    let (_dir, pool, department) = setup();
    let store = SqliteMemoryStore::new(pool, None);
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
}

#[tokio::test]
async fn scene_and_persona_versions_are_append_only() {
    let (_dir, pool, department) = setup();
    let store = SqliteMemoryStore::new(pool, None);
    profile_versions::exercise(
        &store,
        &department,
        &ProfileScope::from(&scope("alice")),
        &ProfileScope::from(
            &MemoryScope::from_optional(Some("team-2"), Some("agt-2"), Some("bob"), None).unwrap(),
        ),
    )
    .await;
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
    let (_dir, pool, department) = setup();
    let store = SqliteMetaStore::new(pool);
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
}

#[tokio::test]
async fn deleting_an_asset_cascades_bindings() {
    let (_dir, pool, department) = setup();
    let store = SqliteMetaStore::new(pool);
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
}

#[tokio::test]
async fn atoms_can_be_queried_by_session() {
    let (_dir, pool, department) = setup();
    let store = SqliteMemoryStore::new(pool, None);
    let mut other = atom("a2", "from another session", "u1");
    other.session_id = Some("s2".into());
    for a in [atom("a1", "from this session", "u1"), other] {
        store.upsert(&department, &a, None).await.unwrap();
    }
    let filter = AtomFilter {
        session_id: Some("s1".into()),
        ..Default::default()
    };
    let (found, total) = AtomRepository::query(
        &store,
        &department,
        AtomSelector::Scope(&scope("u1")),
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
}

#[tokio::test]
async fn atoms_without_an_embedding_get_one_later() {
    // Brute-force cosine over the BLOBs, and the vec0 index when it loads.
    let mut dims = vec![None];
    if memory_infrastructure::sqlite::vec_index::available() {
        dims.push(Some(2));
    }
    for dims in dims {
        let (_dir, pool, department) = setup();
        let store = SqliteMemoryStore::new(pool, dims);
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
    }
}

/// An embedding service that is down until `up` is set.
struct FlakyEmbedder {
    up: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl memory_application::ports::system::EmbeddingProvider for FlakyEmbedder {
    fn dimensions(&self) -> Option<usize> {
        Some(2)
    }

    async fn embed(&self, texts: &[String]) -> memory_domain::error::DomainResult<Vec<Vec<f32>>> {
        if !self.up.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(memory_domain::error::DomainError::unavailable("timed out"));
        }
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

#[tokio::test]
async fn backfill_waits_out_a_failing_embedding_service() {
    use memory_application::memory::backfill::{BackfillReport, EmbeddingBackfill};

    let (_dir, pool, department) = setup();
    let store = Arc::new(SqliteMemoryStore::new(pool, None));
    for i in 0..3 {
        store
            .upsert(&department, &atom(&format!("a{i}"), "text", "alice"), None)
            .await
            .unwrap();
    }
    let embedder = Arc::new(FlakyEmbedder { up: false.into() });
    let backfill = EmbeddingBackfill::new(store.clone(), embedder.clone());

    assert_eq!(
        backfill.all(10).await.unwrap(),
        BackfillReport {
            missing: 3,
            embedded: 0,
            skipped: 3
        }
    );
    embedder
        .up
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        backfill.department(&department, 2).await.unwrap(),
        BackfillReport {
            missing: 2,
            embedded: 2,
            skipped: 0
        }
    );
    assert_eq!(backfill.all(10).await.unwrap().embedded, 1);
    assert_eq!(backfill.all(10).await.unwrap(), BackfillReport::default());
}
