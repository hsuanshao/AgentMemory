//! SQLite → Postgres migration against a live server. Skipped unless
//! `AGENT_MEMORY_TEST_POSTGRES_URL` is set (see `postgres_stores.rs`).
//!
//! Data is written through the SQLite adapters and read back through the
//! Postgres ones, so the test checks behaviour (search, vectors, versions,
//! keys), not just row counts.

use std::sync::Arc;

use chrono::Utc;
use memory_application::admin::DepartmentLifecycle;
use memory_application::common::Page;
use memory_application::ports::memory::{
    AtomFilter, AtomRepository, ConversationFilter, ConversationRepository, ProfileVersionStore,
};
use memory_application::ports::meta::{UserKeyRepository, UserRepository};
use memory_application::ports::skill::{SkillFilter, SkillRepository};
use memory_domain::memory::{AtomType, AtomicMemory, ConversationMessage, Role};
use memory_domain::meta::{KeyStatus, User, UserKey, UserStatus, UserType};
use memory_domain::profile::{NewPersonaVersion, PersonaOp, ProfileScope};
use memory_domain::scope::{DepartmentId, MemoryScope, SegmentId};
use memory_domain::skill::{SkillStatus, SkillVersion};
use memory_infrastructure::postgres::db::PgDb;
use memory_infrastructure::postgres::lifecycle::PostgresCleanup;
use memory_infrastructure::postgres::memory_store::PostgresMemoryStore;
use memory_infrastructure::postgres::meta_store::PostgresMetaStore;
use memory_infrastructure::postgres::skill_store::PostgresSkillStore;
use memory_infrastructure::sqlite::db::DepartmentPool;
use memory_infrastructure::sqlite::memory_store::SqliteMemoryStore;
use memory_infrastructure::sqlite::meta_store::SqliteMetaStore;
use memory_infrastructure::sqlite::skill_store::SqliteSkillStore;
use memory_infrastructure::sqlite_to_postgres::{migrate, Outcome};
use serde_json::json;

const URL_ENV: &str = "AGENT_MEMORY_TEST_POSTGRES_URL";

fn scope() -> MemoryScope {
    MemoryScope::from_optional(Some("team-1"), Some("agt-1"), Some("usr-1"), None).unwrap()
}

fn message(id: &str, content: &str) -> ConversationMessage {
    let now = Utc::now();
    ConversationMessage {
        id: id.into(),
        scope: scope(),
        session_id: "s1".into(),
        role: Role::User,
        content: content.into(),
        timestamp: now,
        recorded_at: now,
        version: 1,
    }
}

fn atom(id: &str, content: &str, background: Option<&str>) -> AtomicMemory {
    let now = Utc::now();
    AtomicMemory {
        id: id.into(),
        scope: scope(),
        atom_type: AtomType::Persona,
        content: content.into(),
        background: background.map(Into::into),
        priority: 80,
        source_message_ids: vec!["m1".into()],
        session_id: Some("s1".into()),
        version: 1,
        created_at: now,
        updated_at: now,
    }
}

fn skill(version: u32, is_head: bool, description: &str) -> SkillVersion {
    SkillVersion {
        skill_id: "skl-1".into(),
        version,
        is_head,
        name: "release-checklist".into(),
        description: description.into(),
        content: "Run the integration tests, then tag.".into(),
        content_hash: format!("h{version}"),
        manifest: vec![],
        status: SkillStatus::Active,
        owner_user_id: "usr-1".into(),
        owner_agent_id: "agt-1".into(),
        team_id: "team-1".into(),
        task_id: None,
        metadata: json!({}),
        created_at_ms: 1_000 + i64::from(version),
        updated_at_ms: 1_000 + i64::from(version),
    }
}

/// Writes one of everything through the SQLite adapters.
async fn seed(pool: &Arc<DepartmentPool>, department: &DepartmentId) {
    let memory = SqliteMemoryStore::new(Arc::clone(pool), Some(2));
    memory
        .append(
            department,
            &[
                message("m1", "We release v2 next Monday"),
                message("m2", "我們下週一發布第二版"),
            ],
        )
        .await
        .unwrap();
    memory
        .upsert(
            department,
            &atom("a1", "Prefers Rust for services", Some("said in standup")),
            Some(&[1.0, 0.0]),
        )
        .await
        .unwrap();
    memory
        .upsert(
            department,
            &atom("a2", "Drinks oolong tea", None),
            Some(&[0.0, 1.0, 0.0]), // wrong dimension: kept, but without vector
        )
        .await
        .unwrap();
    let profile = ProfileScope::from(&scope());
    for body in ["v1", "v2"] {
        memory
            .append_persona(
                department,
                &profile,
                NewPersonaVersion {
                    op: PersonaOp::Generate,
                    body: body.into(),
                    attrs: Default::default(),
                    author: None,
                },
            )
            .await
            .unwrap();
    }

    let meta = SqliteMetaStore::new(Arc::clone(pool));
    let user = User {
        user_id: "usr-1".into(),
        username: "alice".into(),
        user_type: UserType::SystemAdmin,
        status: UserStatus::Active,
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: String::new(),
        metadata_json: json!({}),
    };
    let key = UserKey {
        key_id: "key-1".into(),
        user_id: "usr-1".into(),
        key_value: format!("sk-mem-{}", "x".repeat(32)),
        name: None,
        status: KeyStatus::Active,
        is_default: true,
        last_used_at: None,
        expires_at: None,
        created_at: "2026-01-01T00:00:00.000Z".into(),
    };
    meta.create_user(department, &user, &key).await.unwrap();

    let skills = SqliteSkillStore::new(Arc::clone(pool));
    skills
        .append_version(department, &skill(1, true, "old steps"))
        .await
        .unwrap();
    skills
        .append_version(department, &skill(2, true, "how to ship a release"))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn sqlite_data_survives_the_move_to_postgres() {
    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping: set {URL_ENV} to run the migration test");
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let id = format!("ins-mig-{}", uuid::Uuid::new_v4().simple());
    let department: DepartmentId = SegmentId::parse("department", &id).unwrap();
    let pool = Arc::new(DepartmentPool::new(data.path().to_owned(), 4));
    seed(&pool, &department).await;
    drop(pool);

    let pg = Arc::new(PgDb::connect(&url, 4, Some(2)).await.unwrap());

    // A dry run reports the plan and writes nothing.
    let dry = migrate(data.path(), &pg, true).await.unwrap();
    let ours = dry.departments.iter().find(|r| r.department == id).unwrap();
    assert_eq!(ours.outcome, Outcome::Copied);
    assert_eq!((ours.embeddings_copied, ours.embeddings_dropped), (1, 1));
    assert!(ours
        .tables
        .iter()
        .all(|t| t.target == 0 || t.table == "meta_config_params"));

    let done = migrate(data.path(), &pg, false).await.unwrap();
    let ours = done
        .departments
        .iter()
        .find(|r| r.department == id)
        .unwrap();
    assert_eq!(ours.outcome, Outcome::Copied);
    let rows = |table: &str| {
        ours.tables
            .iter()
            .find(|t| t.table == table)
            .unwrap()
            .target
    };
    assert_eq!(
        (
            rows("conversations"),
            rows("atoms"),
            rows("persona_versions"),
            rows("skills")
        ),
        (2, 2, 2, 2)
    );
    assert_eq!((ours.embeddings_copied, ours.embeddings_dropped), (1, 1));

    let memory = PostgresMemoryStore::new(Arc::clone(&pg));
    let filter = ConversationFilter::default();
    let (messages, total) = ConversationRepository::query(
        &memory,
        &department,
        &scope(),
        &filter,
        Page {
            limit: 10,
            offset: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!((messages.len(), total), (2, 2));
    let hits = ConversationRepository::search(&memory, &department, &scope(), "發布", &filter, 5)
        .await
        .unwrap();
    assert_eq!(
        hits[0].item.id, "m2",
        "CJK keyword search works on copied rows"
    );

    let atoms = AtomFilter::default();
    let by_background = memory
        .keyword_search(&department, &scope(), "standup", &atoms, 5)
        .await
        .unwrap();
    assert_eq!(by_background[0].item.id, "a1", "background is indexed too");
    let by_vector = memory
        .vector_search(&department, &scope(), &[1.0, 0.0], &atoms, 5)
        .await
        .unwrap();
    assert_eq!(
        by_vector
            .iter()
            .map(|s| s.item.id.as_str())
            .collect::<Vec<_>>(),
        ["a1"],
        "only the embedding with the right dimension moved"
    );

    let profile = ProfileScope::from(&scope());
    let head = memory
        .persona(&department, &profile, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((head.body.as_str(), head.version), ("v2", 2));

    let meta = PostgresMetaStore::new(Arc::clone(&pg));
    let key = meta
        .find_key_by_value(&department, &format!("sk-mem-{}", "x".repeat(32)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(key.user_id, "usr-1");
    let admin = meta.get_user(&department, "usr-1").await.unwrap().unwrap();
    assert_eq!(admin.user_type, UserType::SystemAdmin);

    let skills = PostgresSkillStore::new(Arc::clone(&pg));
    let found = skills
        .search_heads(
            &department,
            &SkillFilter {
                statuses: vec![SkillStatus::Active],
                ..SkillFilter::default()
            },
            "ship release",
            5,
        )
        .await
        .unwrap();
    assert_eq!(found.len(), 1, "only the head version is searchable");
    assert_eq!(found[0].0.version, 2);

    // Sequences moved past the copied `seq`s: new writes do not collide,
    // and cursors (which compare `seq`) see them as newer.
    memory
        .append(&department, &[message("m3", "after the move")])
        .await
        .unwrap();
    let newer =
        ConversationRepository::after_seq(&memory, &department, &scope(), "s1", Some(2), 10)
            .await
            .unwrap();
    assert_eq!(
        newer.iter().map(|s| s.item.id.as_str()).collect::<Vec<_>>(),
        ["m3"]
    );

    // A second run finds this department complete... until it differs.
    let again = migrate(data.path(), &pg, false).await;
    let refused = again.is_err();
    assert!(
        refused,
        "the target now has a row the source lacks, so the copy must refuse"
    );
    memory
        .delete_by_ids(&department, &scope(), &["m3".into()])
        .await
        .unwrap();
    let again = migrate(data.path(), &pg, false).await.unwrap();
    let ours = again
        .departments
        .iter()
        .find(|r| r.department == id)
        .unwrap();
    assert_eq!(ours.outcome, Outcome::AlreadyMigrated);

    PostgresCleanup::new(Arc::clone(&pg))
        .destroy(&department)
        .await
        .unwrap();
}
