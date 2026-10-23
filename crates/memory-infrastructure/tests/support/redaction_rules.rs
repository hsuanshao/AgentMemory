//! The `RedactionRuleRepository` contract, run against each backend.

use memory_application::ports::meta::{RedactionRule, RedactionRuleRepository};
use memory_domain::scope::DepartmentId;

fn rule(id: &str, created_at: &str) -> RedactionRule {
    RedactionRule {
        rule_id: id.into(),
        kind: format!("kind-{id}"),
        pattern: r"\bEMP-\d{6}\b".into(),
        description: "badge numbers".into(),
        enabled: true,
        created_by: "usr-a".into(),
        created_at: created_at.into(),
        updated_by: "usr-a".into(),
        updated_at: created_at.into(),
    }
}

pub async fn exercise(store: &dyn RedactionRuleRepository, department: &DepartmentId) {
    assert!(store
        .list_redaction_rules(department)
        .await
        .unwrap()
        .is_empty());
    let (newer, older) = (
        rule("rdr-2", "2026-10-23T02:00:00.000Z"),
        rule("rdr-1", "2026-10-23T01:00:00.000Z"),
    );
    store.put_redaction_rule(department, &newer).await.unwrap();
    store.put_redaction_rule(department, &older).await.unwrap();

    // Oldest first.
    let listed = store.list_redaction_rules(department).await.unwrap();
    assert_eq!(listed, vec![older.clone(), newer.clone()]);

    // A put of the same id replaces it: disabling keeps the record.
    let disabled = RedactionRule {
        enabled: false,
        updated_by: "usr-b".into(),
        updated_at: "2026-10-23T03:00:00.000Z".into(),
        ..older.clone()
    };
    store
        .put_redaction_rule(department, &disabled)
        .await
        .unwrap();
    let listed = store.list_redaction_rules(department).await.unwrap();
    assert_eq!(listed, vec![disabled, newer]);
}
