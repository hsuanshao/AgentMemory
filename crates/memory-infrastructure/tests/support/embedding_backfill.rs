//! The embedding backfill contract of `AtomRepository`, run against each
//! backend with 2-dimensional embeddings.

use chrono::Duration;
use memory_application::ports::memory::{AtomFilter, AtomRepository};
use memory_domain::memory::AtomicMemory;
use memory_domain::profile::ProfileScope;
use memory_domain::scope::DepartmentId;

/// `with` and `without` are atoms of the same scope; `without` is stored
/// without an embedding, as when the embedding call times out.
pub async fn exercise(
    store: &dyn AtomRepository,
    department: &DepartmentId,
    with: AtomicMemory,
    without: AtomicMemory,
) {
    store
        .upsert(department, &with, Some(&[0.0, 1.0]))
        .await
        .unwrap();
    store.upsert(department, &without, None).await.unwrap();
    let profile = ProfileScope::from(&without.scope);
    let seqs = |atoms: Vec<memory_application::ports::memory::Sequenced<AtomicMemory>>| {
        atoms
            .into_iter()
            .map(|a| (a.item.id, a.seq))
            .collect::<Vec<_>>()
    };
    let before = seqs(
        store
            .after_seq(department, &profile, None, 10)
            .await
            .unwrap(),
    );

    let missing = store
        .missing_embeddings(department, None, 10)
        .await
        .unwrap();
    assert_eq!(
        missing
            .iter()
            .map(|a| a.item.id.as_str())
            .collect::<Vec<_>>(),
        [without.id.as_str()]
    );
    // From a cursor on.
    assert!(store
        .missing_embeddings(department, Some(missing[0].seq), 10)
        .await
        .unwrap()
        .is_empty());
    assert!(store.departments().await.unwrap().contains(department));

    // The atom changed since it was read: leave it to the next write.
    let stale = without.updated_at - Duration::milliseconds(5);
    assert!(!store
        .set_embedding(department, &without.id, stale, &[1.0, 0.0])
        .await
        .unwrap());

    assert!(store
        .set_embedding(department, &without.id, without.updated_at, &[1.0, 0.0])
        .await
        .unwrap());
    // Only once: it has one now.
    assert!(!store
        .set_embedding(department, &without.id, without.updated_at, &[0.0, 1.0])
        .await
        .unwrap());
    assert!(store
        .missing_embeddings(department, None, 10)
        .await
        .unwrap()
        .is_empty());

    // Found by vector now, and not rewritten: same seqs, so L2 does not see
    // the atom as new.
    let hits = store
        .vector_search(
            department,
            &without.scope,
            &[1.0, 0.0],
            &AtomFilter::default(),
            1,
        )
        .await
        .unwrap();
    assert_eq!(hits[0].item.id, without.id);
    assert!((hits[0].score - 1.0).abs() < 1e-6);
    assert_eq!(
        seqs(
            store
                .after_seq(department, &profile, None, 10)
                .await
                .unwrap()
        ),
        before
    );
}
