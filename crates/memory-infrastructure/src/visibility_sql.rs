//! SQL for [`TeamView`] over rows that keep their team and user in columns
//! or JSON fields. Shared by the SQLite and Postgres stores (both bind `?`).

use memory_domain::meta::visibility::TeamView;

/// A clause, true for the rows `view` sees, and its parameters. `team` and
/// `user` are SQL expressions; `user` is NULL for shared (per-team) rows.
pub fn clause(team: &str, user: &str, view: &TeamView) -> (String, Vec<String>) {
    let marks = |n: usize| vec!["?"; n].join(", ");
    let mut parts = Vec::new();
    let mut params = Vec::new();
    if !view.overseen.is_empty() {
        parts.push(format!("{team} IN ({})", marks(view.overseen.len())));
        params.extend(view.overseen.iter().cloned());
    }
    if !view.member_of.is_empty() {
        parts.push(format!(
            "({team} IN ({}) AND ({user} IS NULL OR {user} = ?))",
            marks(view.member_of.len())
        ));
        params.extend(view.member_of.iter().cloned());
        params.push(view.user_id.clone());
    }
    if parts.is_empty() {
        return ("1 = 0".into(), params);
    }
    (format!("({})", parts.join(" OR ")), params)
}
