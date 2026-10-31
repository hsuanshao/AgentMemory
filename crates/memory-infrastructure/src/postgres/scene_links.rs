//! Which atoms each scene was built from (`scene_atoms`) and why a scene may
//! be out of date (`scene_stale`); mirrors `sqlite/scene_links.rs`. Every
//! function runs inside the caller's transaction, which holds the
//! `{schema}.atoms` lock: atom writes and L2 placements see each other's
//! effects instead of racing past them.

use memory_domain::error::DomainResult;
use memory_domain::profile::{
    MemberState, NewSceneVersion, SceneAtom, SceneMember, SceneOp, SceneVersion, SceneWriteLinks,
    StaleMark, StaleReason,
};
use sqlx::postgres::PgRow;
use sqlx::PgConnection;

use super::db::Schema;
use super::sql::{args, execute, fetch_all, fetch_optional, get, Arg};
use crate::sqlite::sql::from_ms;

fn version(v: i64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// `(?,?,…)` for `n` values.
fn list(n: usize) -> String {
    vec!["?"; n].join(",")
}

async fn current_scene(
    conn: &mut PgConnection,
    s: &Schema,
    atom_id: &str,
) -> DomainResult<Option<String>> {
    fetch_optional(
        conn,
        &format!("SELECT scene_id FROM {s}.scene_atoms WHERE atom_id = ? AND state = 'current'"),
        &args![atom_id],
    )
    .await?
    .map(|r| get(&r, "scene_id"))
    .transpose()
}

/// The text a scene last saw of an atom: the one kept by an open rewrite
/// mark, which predates the rewrites since.
async fn earliest_before(
    conn: &mut PgConnection,
    s: &Schema,
    scene_id: &str,
    atom_id: &str,
) -> DomainResult<Option<String>> {
    Ok(fetch_optional(
        conn,
        &format!("SELECT before_content FROM {s}.scene_stale WHERE scene_id = ? AND atom_id = ? AND reason IN ('rewritten', 'changed_during_run') ORDER BY id LIMIT 1"),
        &args![scene_id, atom_id],
    )
    .await?
    .map(|r| get::<Option<String>>(&r, "before_content"))
    .transpose()?
    .flatten())
}

/// A stale mark about to be written.
struct NewMark<'a> {
    scene_id: &'a str,
    atom_id: &'a str,
    reason: StaleReason,
    atom_version: Option<u32>,
    before: Option<&'a str>,
}

async fn insert_mark(
    conn: &mut PgConnection,
    s: &Schema,
    mark: NewMark<'_>,
    now: i64,
) -> DomainResult<()> {
    execute(
        conn,
        &format!("INSERT INTO {s}.scene_stale (scene_id, atom_id, reason, atom_version, before_content, attempts, marked_at_ms) VALUES (?,?,?,?,?,0,?)"),
        &args![
            mark.scene_id,
            mark.atom_id,
            mark.reason.as_str(),
            mark.atom_version.map(i64::from),
            mark.before.map(str::to_owned),
            now
        ],
    )
    .await?;
    Ok(())
}

/// An atom placed in a scene was rewritten from `before` to `version`. One
/// open rewrite mark per scene and atom: a later rewrite replaces it with a
/// new mark (a new id, attempts from 0) that keeps the oldest text. A mark
/// is never changed in place, so an L2 run settling the marks it was given
/// by id cannot settle a rewrite made after it read them.
pub async fn mark_rewritten(
    conn: &mut PgConnection,
    s: &Schema,
    atom_id: &str,
    version: u32,
    before: &str,
    now: i64,
) -> DomainResult<()> {
    let Some(scene_id) = current_scene(conn, s, atom_id).await? else {
        return Ok(());
    };
    let kept: Option<String> = fetch_optional(
        conn,
        &format!("SELECT before_content FROM {s}.scene_stale WHERE scene_id = ? AND atom_id = ? AND reason = 'rewritten' ORDER BY id LIMIT 1"),
        &args![scene_id.as_str(), atom_id],
    )
    .await?
    .map(|r| get::<Option<String>>(&r, "before_content"))
    .transpose()?
    .flatten();
    execute(
        conn,
        &format!("DELETE FROM {s}.scene_stale WHERE scene_id = ? AND atom_id = ? AND reason = 'rewritten'"),
        &args![scene_id.as_str(), atom_id],
    )
    .await?;
    insert_mark(
        conn,
        s,
        NewMark {
            scene_id: &scene_id,
            atom_id,
            reason: StaleReason::Rewritten,
            atom_version: Some(version),
            before: Some(kept.as_deref().unwrap_or(before)),
        },
        now,
    )
    .await
}

/// Atoms about to be deleted, with their text. A deletion replaces the
/// atom's open rewrite marks, keeping the oldest text the scene saw.
pub async fn mark_deleted(
    conn: &mut PgConnection,
    s: &Schema,
    atoms: &[(String, String)],
    now: i64,
) -> DomainResult<()> {
    for (atom_id, content) in atoms {
        let Some(scene_id) = current_scene(conn, s, atom_id).await? else {
            continue;
        };
        let before = earliest_before(conn, s, &scene_id, atom_id)
            .await?
            .unwrap_or_else(|| content.clone());
        execute(
            conn,
            &format!("DELETE FROM {s}.scene_stale WHERE scene_id = ? AND atom_id = ? AND reason IN ('rewritten', 'changed_during_run')"),
            &args![scene_id.as_str(), atom_id.as_str()],
        )
        .await?;
        insert_mark(
            conn,
            s,
            NewMark {
                scene_id: &scene_id,
                atom_id,
                reason: StaleReason::Deleted,
                atom_version: None,
                before: Some(&before),
            },
            now,
        )
        .await?;
    }
    Ok(())
}

/// What a scene write means for the atoms; see
/// `ProfileVersionStore::append_scenes_linked`. `written` pairs each new
/// version with the one stored for it.
pub async fn apply_links(
    conn: &mut PgConnection,
    s: &Schema,
    team: &str,
    agent: &str,
    written: &[(NewSceneVersion, SceneVersion)],
    links: &SceneWriteLinks,
    now: i64,
) -> DomainResult<()> {
    for (new, stored) in written {
        let scene_id = stored.scene_id.as_str();
        for parent in &new.parents {
            if parent.scene_id == scene_id {
                continue;
            }
            let pair = args![scene_id, parent.scene_id.as_str()];
            // A row the merged scene kept for an atom moved out of it gives
            // way to the atom coming back.
            execute(
                conn,
                &format!("DELETE FROM {s}.scene_atoms WHERE scene_id = ? AND atom_id IN (SELECT atom_id FROM {s}.scene_atoms WHERE scene_id = ? AND state = 'current')"),
                &pair,
            )
            .await?;
            execute(
                conn,
                &format!("UPDATE {s}.scene_atoms SET scene_id = ? WHERE scene_id = ? AND state = 'current'"),
                &pair,
            )
            .await?;
            execute(
                conn,
                &format!("UPDATE {s}.scene_stale SET scene_id = ? WHERE scene_id = ?"),
                &pair,
            )
            .await?;
        }
        match new.op {
            // A removed scene says nothing any more: nothing left to bring
            // up to date, so its marks go with it.
            SceneOp::Retire if !new.attrs.contains_key("merged_into") => {
                execute(
                    conn,
                    &format!("UPDATE {s}.scene_atoms SET state = 'moved_out' WHERE scene_id = ? AND state = 'current'"),
                    &args![scene_id],
                )
                .await?;
                execute(
                    conn,
                    &format!("DELETE FROM {s}.scene_stale WHERE scene_id = ?"),
                    &args![scene_id],
                )
                .await?;
            }
            SceneOp::Manual => {
                execute(
                    conn,
                    &format!("DELETE FROM {s}.scene_stale WHERE scene_id = ?"),
                    &args![scene_id],
                )
                .await?;
            }
            _ => {}
        }
    }
    if let Some((_, target)) = written.first() {
        for member in &links.members {
            place(conn, s, target, member, now).await?;
        }
    }
    if !links.resolved.is_empty() {
        let mut params: Vec<Arg> = vec![team.into(), agent.into()];
        params.extend(links.resolved.iter().map(|id| Arg::from(*id)));
        execute(
            conn,
            &format!(
                "DELETE FROM {s}.scene_stale WHERE scene_id IN (SELECT id FROM {s}.scenes WHERE team_id = ? AND agent_id = ?) AND id IN ({})",
                list(links.resolved.len())
            ),
            &params,
        )
        .await?;
    }
    Ok(())
}

/// Makes `member` a current atom of `target`.
async fn place(
    conn: &mut PgConnection,
    s: &Schema,
    target: &SceneVersion,
    member: &SceneMember,
    now: i64,
) -> DomainResult<()> {
    let atom_id = member.atom_id.as_str();
    if let Some(other) = current_scene(conn, s, atom_id)
        .await?
        .filter(|o| o != &target.scene_id)
    {
        let before = earliest_before(conn, s, &other, atom_id)
            .await?
            .unwrap_or_else(|| member.content.clone());
        execute(
            conn,
            &format!(
                "UPDATE {s}.scene_atoms SET state = 'moved_out' WHERE scene_id = ? AND atom_id = ?"
            ),
            &args![other.as_str(), atom_id],
        )
        .await?;
        execute(
            conn,
            &format!("DELETE FROM {s}.scene_stale WHERE scene_id = ? AND atom_id = ? AND reason IN ('rewritten', 'changed_during_run')"),
            &args![other.as_str(), atom_id],
        )
        .await?;
        insert_mark(
            conn,
            s,
            NewMark {
                scene_id: &other,
                atom_id,
                reason: StaleReason::MovedOut,
                atom_version: Some(member.atom_version),
                before: Some(&before),
            },
            now,
        )
        .await?;
    }
    execute(
        conn,
        &format!(
            "INSERT INTO {s}.scene_atoms (scene_id, atom_id, atom_version, user_id, state, scene_version, assigned_at_ms) VALUES (?,?,?,?,'current',?,?)
             ON CONFLICT (scene_id, atom_id) DO UPDATE SET atom_version = EXCLUDED.atom_version, user_id = EXCLUDED.user_id,
                 state = 'current', scene_version = EXCLUDED.scene_version, assigned_at_ms = EXCLUDED.assigned_at_ms"
        ),
        &args![
            target.scene_id.as_str(),
            atom_id,
            member.atom_version,
            member.user_id.as_str(),
            target.version,
            now
        ],
    )
    .await?;
    // Placed at the version L2 read: rewrites up to it are dealt with.
    execute(
        conn,
        &format!("DELETE FROM {s}.scene_stale WHERE atom_id = ? AND reason IN ('rewritten', 'changed_during_run') AND (atom_version IS NULL OR atom_version <= ?)"),
        &args![atom_id, member.atom_version],
    )
    .await?;
    let stored = fetch_optional(
        conn,
        &format!("SELECT version FROM {s}.atoms WHERE id = ?"),
        &args![atom_id],
    )
    .await?
    .map(|r| get::<i64>(&r, "version").map(version))
    .transpose()?;
    match stored {
        None => {
            insert_mark(
                conn,
                s,
                NewMark {
                    scene_id: &target.scene_id,
                    atom_id,
                    reason: StaleReason::Deleted,
                    atom_version: None,
                    before: Some(&member.content),
                },
                now,
            )
            .await?
        }
        Some(v) if v > member.atom_version => {
            insert_mark(
                conn,
                s,
                NewMark {
                    scene_id: &target.scene_id,
                    atom_id,
                    reason: StaleReason::ChangedDuringRun,
                    atom_version: Some(v),
                    before: Some(&member.content),
                },
                now,
            )
            .await?
        }
        Some(_) => {}
    }
    Ok(())
}

const ATOM_COLUMNS: &str = "sa.scene_id, sa.atom_id, sa.atom_version, sa.user_id, sa.state, sa.scene_version, sa.assigned_at_ms";

fn atom_from_row(r: &PgRow) -> DomainResult<SceneAtom> {
    let state: String = get(r, "state")?;
    Ok(SceneAtom {
        scene_id: get(r, "scene_id")?,
        atom_id: get(r, "atom_id")?,
        atom_version: version(get(r, "atom_version")?),
        user_id: get(r, "user_id")?,
        state: MemberState::parse(&state)?,
        scene_version: version(get(r, "scene_version")?),
        assigned_at: from_ms(get(r, "assigned_at_ms")?),
    })
}

pub async fn scene_atoms(
    conn: &mut PgConnection,
    s: &Schema,
    team: &str,
    agent: &str,
    scene_id: &str,
) -> DomainResult<Vec<SceneAtom>> {
    fetch_all(
        conn,
        &format!(
            "SELECT {ATOM_COLUMNS} FROM {s}.scene_atoms sa JOIN {s}.scenes s ON s.id = sa.scene_id
             WHERE s.team_id = ? AND s.agent_id = ? AND sa.scene_id = ? ORDER BY sa.assigned_at_ms, sa.atom_id"
        ),
        &args![team, agent, scene_id],
    )
    .await?
    .iter()
    .map(atom_from_row)
    .collect()
}

const MARK_COLUMNS: &str = "st.id, st.scene_id, st.atom_id, st.reason, st.atom_version, st.before_content, st.attempts, st.marked_at_ms";

fn mark_from_row(r: &PgRow) -> DomainResult<StaleMark> {
    let reason: String = get(r, "reason")?;
    Ok(StaleMark {
        id: get(r, "id")?,
        scene_id: get(r, "scene_id")?,
        atom_id: get(r, "atom_id")?,
        reason: StaleReason::parse(&reason)?,
        atom_version: get::<Option<i64>>(r, "atom_version")?.map(version),
        before_content: get(r, "before_content")?,
        attempts: version(get(r, "attempts")?),
        marked_at: from_ms(get(r, "marked_at_ms")?),
    })
}

pub async fn stale_marks(
    conn: &mut PgConnection,
    s: &Schema,
    team: &str,
    agent: &str,
    scene_ids: Option<&[String]>,
) -> DomainResult<Vec<StaleMark>> {
    let mut params: Vec<Arg> = vec![team.into(), agent.into()];
    let only = match scene_ids {
        Some([]) => return Ok(Vec::new()),
        Some(ids) => {
            params.push(Arg::Texts(ids.to_vec()));
            "AND st.scene_id = ANY(?)"
        }
        None => "",
    };
    fetch_all(
        conn,
        &format!(
            "SELECT {MARK_COLUMNS} FROM {s}.scene_stale st JOIN {s}.scenes s ON s.id = st.scene_id
             WHERE s.team_id = ? AND s.agent_id = ? {only} ORDER BY st.id"
        ),
        &params,
    )
    .await?
    .iter()
    .map(mark_from_row)
    .collect()
}

pub async fn note_attempt(
    conn: &mut PgConnection,
    s: &Schema,
    team: &str,
    agent: &str,
    ids: &[i64],
) -> DomainResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut params: Vec<Arg> = vec![team.into(), agent.into()];
    params.extend(ids.iter().map(|id| Arg::from(*id)));
    execute(
        conn,
        &format!(
            "UPDATE {s}.scene_stale SET attempts = attempts + 1 WHERE scene_id IN (SELECT id FROM {s}.scenes WHERE team_id = ? AND agent_id = ?) AND id IN ({})",
            list(ids.len())
        ),
        &params,
    )
    .await?;
    Ok(())
}
