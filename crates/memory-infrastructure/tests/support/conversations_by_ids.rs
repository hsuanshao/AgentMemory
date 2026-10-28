//! `ConversationRepository::get_by_ids`, run against each backend: the
//! sources a recall shows under its atoms.

use chrono::{TimeZone, Utc};
use memory_application::ports::memory::ConversationRepository;
use memory_domain::memory::{ConversationMessage, Role};
use memory_domain::scope::{DepartmentId, MemoryScope};

fn scope(user: &str) -> MemoryScope {
    MemoryScope::from_optional(Some("team-src"), Some("agt-src"), Some(user), None).unwrap()
}

fn message(id: &str, user: &str, minute: u32) -> ConversationMessage {
    let at = Utc.with_ymd_and_hms(2026, 10, 28, 12, minute, 0).unwrap();
    ConversationMessage {
        id: id.into(),
        scope: scope(user),
        session_id: "s-src".into(),
        role: Role::User,
        content: format!("message {id}"),
        timestamp: at,
        recorded_at: at,
        version: 1,
    }
}

pub async fn exercise(store: &dyn ConversationRepository, department: &DepartmentId) {
    store
        .append(
            department,
            &[
                message("src-2", "alice", 2),
                message("src-1", "alice", 1),
                message("src-bob", "bob", 0),
            ],
        )
        .await
        .unwrap();
    let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    // Oldest first, whatever order the ids come in; unknown ids are skipped.
    let found = store
        .get_by_ids(
            department,
            &scope("alice"),
            &ids(&["src-2", "gone", "src-1"]),
        )
        .await
        .unwrap();
    assert_eq!(
        found.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["src-1", "src-2"]
    );
    assert_eq!(found[0].content, "message src-1");

    // Another user's message is not returned, even by its id.
    let found = store
        .get_by_ids(department, &scope("alice"), &ids(&["src-bob"]))
        .await
        .unwrap();
    assert!(found.is_empty());

    assert!(store
        .get_by_ids(department, &scope("alice"), &[])
        .await
        .unwrap()
        .is_empty());
}
