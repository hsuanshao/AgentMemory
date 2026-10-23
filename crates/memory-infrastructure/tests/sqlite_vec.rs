//! sqlite-vec atom index: KNN, isolation, sync and fallback.

use std::path::Path;
use std::sync::Arc;

use chrono::{Duration, Utc};
use memory_application::common::TimeRange;
use memory_application::ports::memory::{AtomFilter, AtomRepository};
use memory_domain::memory::{AtomType, AtomicMemory};
use memory_domain::profile::ProfileScope;
use memory_domain::scope::{DepartmentId, MemoryScope, SegmentId};
use memory_infrastructure::sqlite::db::DepartmentPool;
use memory_infrastructure::sqlite::memory_store::SqliteMemoryStore;
use memory_infrastructure::sqlite::vec_index;
use rusqlite::{Connection, OptionalExtension};

struct Fixture {
    _dir: tempfile::TempDir,
    pool: Arc<DepartmentPool>,
    department: DepartmentId,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(DepartmentPool::new(dir.path().to_path_buf(), 4));
        let department = SegmentId::parse("department", "ins-vec").unwrap();
        pool.create(&department).unwrap();
        Self {
            pool,
            _dir: dir,
            department,
        }
    }

    fn store(&self, dims: Option<usize>) -> SqliteMemoryStore {
        SqliteMemoryStore::new(Arc::clone(&self.pool), dims)
    }

    /// A side connection to inspect the index directly.
    fn inspect(&self) -> Connection {
        open_db(&self.pool.department_dir(&self.department).join("memory.db"))
    }
}

fn open_db(path: &Path) -> Connection {
    vec_index::register();
    Connection::open(path).unwrap()
}

fn indexed_rows(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM atoms_vec", [], |r| r.get(0))
        .unwrap()
}

fn stored_dims(conn: &Connection) -> Option<i64> {
    conn.query_row(
        "SELECT value FROM index_meta WHERE key = 'atoms_vec_dims'",
        [],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
}

fn scope_of(team: &str, user: &str) -> MemoryScope {
    MemoryScope::from_optional(Some(team), Some("agt-1"), Some(user), None).unwrap()
}

fn atom(id: &str, scope: MemoryScope) -> AtomicMemory {
    let now = Utc::now();
    AtomicMemory {
        id: id.into(),
        scope,
        atom_type: AtomType::Persona,
        content: format!("content of {id}"),
        background: None,
        priority: 80,
        source_message_ids: vec![],
        session_id: Some("s1".into()),
        version: 1,
        created_at: now,
        updated_at: now,
    }
}

async fn put(store: &SqliteMemoryStore, f: &Fixture, a: &AtomicMemory, e: Option<&[f32]>) {
    store.upsert(&f.department, a, e).await.unwrap();
}

async fn search(
    store: &SqliteMemoryStore,
    f: &Fixture,
    scope: &MemoryScope,
    q: &[f32],
    filter: &AtomFilter,
    limit: usize,
) -> Vec<(String, f64)> {
    store
        .vector_search(&f.department, scope, q, filter, limit)
        .await
        .unwrap()
        .into_iter()
        .map(|s| (s.item.id, s.score))
        .collect()
}

fn ids(hits: &[(String, f64)]) -> Vec<&str> {
    hits.iter().map(|(id, _)| id.as_str()).collect()
}

fn require_extension() {
    assert!(
        vec_index::available(),
        "sqlite-vec must load in the test build"
    );
}

#[tokio::test]
async fn knn_orders_by_cosine_like_brute_force() {
    require_extension();
    let f = Fixture::new();
    let store = f.store(Some(3));
    assert_eq!(store.vector_index(), "sqlite-vec");
    let alice = scope_of("team-1", "alice");
    let vectors: [(&str, [f32; 3]); 4] = [
        ("a-far", [-1.0, 0.0, 0.0]),
        ("a-mid", [0.8, 0.6, 0.0]),
        ("a-best", [2.0, 0.0, 0.0]),
        ("a-ortho", [0.0, 1.0, 0.0]),
    ];
    for (id, v) in &vectors {
        put(&store, &f, &atom(id, alice.clone()), Some(v)).await;
    }
    assert_eq!(indexed_rows(&f.inspect()), 4);

    let q = [1.0, 0.0, 0.0];
    let hits = search(&store, &f, &alice, &q, &AtomFilter::default(), 3).await;
    assert_eq!(ids(&hits), ["a-best", "a-mid", "a-ortho"]);
    let expected = [1.0, 0.8, 0.0];
    for ((_, got), want) in hits.iter().zip(expected) {
        assert!((got - want).abs() < 1e-5, "score {got} vs {want}");
    }

    let brute = f.store(None);
    let bf = search(&brute, &f, &alice, &q, &AtomFilter::default(), 3).await;
    assert_eq!(ids(&bf), ids(&hits));
}

#[tokio::test]
async fn other_scopes_are_never_returned_even_when_nearer() {
    require_extension();
    let f = Fixture::new();
    let store = f.store(Some(2));
    let alice = scope_of("team-1", "alice");
    // Many nearer neighbours in other users / teams / agents: an
    // over-fetch-then-filter strategy would lose alice's rows here.
    for i in 0..40 {
        let v = [1.0, 0.001 * i as f32];
        put(
            &store,
            &f,
            &atom(&format!("bob-{i}"), scope_of("team-1", "bob")),
            Some(&v),
        )
        .await;
        put(
            &store,
            &f,
            &atom(&format!("t2-{i}"), scope_of("team-2", "alice")),
            Some(&v),
        )
        .await;
        let other_agent =
            MemoryScope::from_optional(Some("team-1"), Some("agt-2"), Some("alice"), None).unwrap();
        put(
            &store,
            &f,
            &atom(&format!("ag2-{i}"), other_agent),
            Some(&v),
        )
        .await;
    }
    put(
        &store,
        &f,
        &atom("alice-1", alice.clone()),
        Some(&[0.0, 1.0]),
    )
    .await;
    put(
        &store,
        &f,
        &atom("alice-2", alice.clone()),
        Some(&[0.5, 1.0]),
    )
    .await;

    let hits = search(&store, &f, &alice, &[1.0, 0.0], &AtomFilter::default(), 5).await;
    assert_eq!(ids(&hits), ["alice-2", "alice-1"]);
}

#[tokio::test]
async fn atom_type_and_time_filters_apply_in_the_index() {
    require_extension();
    let f = Fixture::new();
    let store = f.store(Some(2));
    let alice = scope_of("team-1", "alice");
    let now = Utc::now();
    let mut old = atom("old-instr", alice.clone());
    old.atom_type = AtomType::Instruction;
    old.updated_at = now - Duration::days(10);
    let mut new_instr = atom("new-instr", alice.clone());
    new_instr.atom_type = AtomType::Instruction;
    let persona = atom("persona", alice.clone());
    put(&store, &f, &old, Some(&[1.0, 0.0])).await;
    put(&store, &f, &new_instr, Some(&[0.6, 0.8])).await;
    put(&store, &f, &persona, Some(&[1.0, 0.1])).await;

    let typed = AtomFilter {
        atom_type: Some(AtomType::Instruction),
        session_id: None,
        time: TimeRange::default(),
    };
    let hits = search(&store, &f, &alice, &[1.0, 0.0], &typed, 5).await;
    assert_eq!(ids(&hits), ["old-instr", "new-instr"]);

    let recent = AtomFilter {
        atom_type: Some(AtomType::Instruction),
        session_id: None,
        time: TimeRange {
            start: Some(now - Duration::days(1)),
            end: Some(now + Duration::days(1)),
        },
    };
    let hits = search(&store, &f, &alice, &[1.0, 0.0], &recent, 5).await;
    assert_eq!(ids(&hits), ["new-instr"]);

    let until = AtomFilter {
        atom_type: None,
        session_id: None,
        time: TimeRange {
            start: None,
            end: Some(now - Duration::days(1)),
        },
    };
    let hits = search(&store, &f, &alice, &[1.0, 0.0], &until, 5).await;
    assert_eq!(ids(&hits), ["old-instr"]);
}

#[tokio::test]
async fn index_follows_replace_delete_and_delete_profile() {
    require_extension();
    let f = Fixture::new();
    let store = f.store(Some(2));
    let alice = scope_of("team-1", "alice");
    let bob = scope_of("team-1", "bob");
    put(&store, &f, &atom("a1", alice.clone()), Some(&[1.0, 0.0])).await;
    put(&store, &f, &atom("a2", alice.clone()), Some(&[0.0, 1.0])).await;
    put(&store, &f, &atom("b1", bob.clone()), Some(&[1.0, 0.0])).await;
    let conn = f.inspect();
    assert_eq!(indexed_rows(&conn), 3);

    // Replace moves the vector (new seq) without leaving the old one behind.
    put(&store, &f, &atom("a1", alice.clone()), Some(&[-1.0, 0.0])).await;
    assert_eq!(indexed_rows(&conn), 3);
    let hits = search(&store, &f, &alice, &[1.0, 0.0], &AtomFilter::default(), 5).await;
    assert_eq!(ids(&hits), ["a2", "a1"]);
    assert!((hits[1].1 + 1.0).abs() < 1e-5);

    // Replacing without an embedding removes it from the index.
    put(&store, &f, &atom("a1", alice.clone()), None).await;
    assert_eq!(indexed_rows(&conn), 2);
    let hits = search(&store, &f, &alice, &[1.0, 0.0], &AtomFilter::default(), 5).await;
    assert_eq!(ids(&hits), ["a2"]);

    assert_eq!(
        store
            .delete(&f.department, &alice, &["a2".into()])
            .await
            .unwrap(),
        1
    );
    assert_eq!(indexed_rows(&conn), 1);
    assert!(
        search(&store, &f, &alice, &[1.0, 0.0], &AtomFilter::default(), 5)
            .await
            .is_empty()
    );

    let profile = ProfileScope::from(&bob);
    AtomRepository::delete_profile(&store, &f.department, &profile)
        .await
        .unwrap();
    assert_eq!(indexed_rows(&conn), 0);
}

#[tokio::test]
async fn dimension_change_rebuilds_from_blobs() {
    require_extension();
    let f = Fixture::new();
    let alice = scope_of("team-1", "alice");
    let two = f.store(Some(2));
    put(&two, &f, &atom("a1", alice.clone()), Some(&[1.0, 0.0])).await;
    put(&two, &f, &atom("a2", alice.clone()), Some(&[0.0, 1.0])).await;
    let conn = f.inspect();
    assert_eq!(stored_dims(&conn), Some(2));

    // A 3-dim configuration rebuilds; the 2-dim BLOBs cannot be indexed.
    let three = f.store(Some(3));
    put(
        &three,
        &f,
        &atom("a3", alice.clone()),
        Some(&[0.0, 0.0, 1.0]),
    )
    .await;
    assert_eq!(stored_dims(&conn), Some(3));
    assert_eq!(indexed_rows(&conn), 1);
    let hits = search(
        &three,
        &f,
        &alice,
        &[0.0, 0.0, 1.0],
        &AtomFilter::default(),
        5,
    )
    .await;
    assert_eq!(ids(&hits), ["a3"]);

    // Back to 2 dims: rebuilt again from the BLOBs, on a read.
    let hits = search(&two, &f, &alice, &[1.0, 0.0], &AtomFilter::default(), 5).await;
    assert_eq!(ids(&hits), ["a1", "a2"]);
    assert_eq!(stored_dims(&conn), Some(2));
    assert_eq!(indexed_rows(&conn), 2);

    // A recorded dimension that disagrees forces a rebuild too.
    conn.execute(
        "UPDATE index_meta SET value = 99 WHERE key = 'atoms_vec_dims'",
        [],
    )
    .unwrap();
    let hits = search(&two, &f, &alice, &[0.0, 1.0], &AtomFilter::default(), 5).await;
    assert_eq!(ids(&hits), ["a2", "a1"]);
    assert_eq!(stored_dims(&conn), Some(2));
}

#[tokio::test]
async fn without_dims_search_is_brute_force_and_index_is_invalidated() {
    let f = Fixture::new();
    let alice = scope_of("team-1", "alice");
    let brute = f.store(None);
    assert_eq!(brute.vector_index(), "brute-force");
    put(&brute, &f, &atom("a1", alice.clone()), Some(&[1.0, 0.0])).await;
    put(&brute, &f, &atom("a2", alice.clone()), Some(&[0.0, 1.0])).await;
    let hits = search(&brute, &f, &alice, &[1.0, 0.1], &AtomFilter::default(), 5).await;
    assert_eq!(ids(&hits), ["a1", "a2"]);

    require_extension();
    let indexed = f.store(Some(2));
    put(&indexed, &f, &atom("a3", alice.clone()), Some(&[0.7, 0.7])).await;
    let conn = f.inspect();
    assert_eq!(stored_dims(&conn), Some(2));
    assert_eq!(indexed_rows(&conn), 3);

    // A write that bypasses the index marks it stale, and the next indexed
    // use rebuilds it so the write is not missed.
    put(&brute, &f, &atom("a4", alice.clone()), Some(&[1.0, 0.0])).await;
    assert_eq!(stored_dims(&conn), None);
    let hits = search(&indexed, &f, &alice, &[1.0, 0.0], &AtomFilter::default(), 2).await;
    assert_eq!(hits.len(), 2);
    assert!(ids(&hits).contains(&"a4") && ids(&hits).contains(&"a1"));
    assert_eq!(indexed_rows(&conn), 4);
}

#[tokio::test]
async fn query_of_another_dimension_falls_back_to_brute_force() {
    require_extension();
    let f = Fixture::new();
    let alice = scope_of("team-1", "alice");
    let store = f.store(Some(2));
    put(&store, &f, &atom("a1", alice.clone()), Some(&[1.0, 0.0])).await;
    let hits = search(
        &store,
        &f,
        &alice,
        &[1.0, 0.0, 0.0],
        &AtomFilter::default(),
        5,
    )
    .await;
    assert_eq!(ids(&hits), ["a1"]);
    assert_eq!(hits[0].1, 0.0);
}
