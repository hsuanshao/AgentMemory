//! Which atoms each scene was built from (`scene_atoms`) and why a scene may
//! be out of date (`scene_stale`). Every function runs inside the caller's
//! transaction, so a mark is written together with the atom or scene write
//! that causes it.

use memory_domain::profile::{
    MemberState, NewSceneVersion, SceneAtom, SceneMember, SceneOp, SceneVersion, SceneWriteLinks,
    StaleMark, StaleReason,
};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use super::sql::from_ms;

/// The scene atom `atom_id` currently belongs to.
fn current_scene(conn: &Connection, atom_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT scene_id FROM scene_atoms WHERE atom_id = ? AND state = 'current'",
        [atom_id],
        |r| r.get(0),
    )
    .optional()
}

/// The text a scene last saw of an atom: the one kept by an open
/// rewrite mark, which predates the rewrites since.
fn earliest_before(
    conn: &Connection,
    scene_id: &str,
    atom_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT before_content FROM scene_stale WHERE scene_id = ? AND atom_id = ? AND reason IN ('rewritten', 'changed_during_run') ORDER BY id LIMIT 1",
        params![scene_id, atom_id],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
}

/// A stale mark about to be written.
struct NewMark<'a> {
    scene_id: &'a str,
    atom_id: &'a str,
    reason: StaleReason,
    atom_version: Option<u32>,
    before: Option<&'a str>,
}

fn insert_mark(conn: &Connection, mark: NewMark<'_>, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO scene_stale (scene_id, atom_id, reason, atom_version, before_content, attempts, marked_at_ms) VALUES (?,?,?,?,?,0,?)",
        params![mark.scene_id, mark.atom_id, mark.reason.as_str(), mark.atom_version, mark.before, now],
    )?;
    Ok(())
}

/// An atom placed in a scene was rewritten from `before` to `version`. One
/// open rewrite mark per scene and atom: a later rewrite replaces it with a
/// new mark (a new id, attempts from 0) that keeps the oldest text. A mark
/// is never changed in place, so an L2 run settling the marks it was given
/// by id cannot settle a rewrite made after it read them.
pub fn mark_rewritten(
    conn: &Connection,
    atom_id: &str,
    version: u32,
    before: &str,
    now: i64,
) -> rusqlite::Result<()> {
    let Some(scene_id) = current_scene(conn, atom_id)? else {
        return Ok(());
    };
    let kept: Option<String> = conn
        .query_row(
            "SELECT before_content FROM scene_stale WHERE scene_id = ? AND atom_id = ? AND reason = 'rewritten' ORDER BY id LIMIT 1",
            params![scene_id, atom_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    conn.execute(
        "DELETE FROM scene_stale WHERE scene_id = ? AND atom_id = ? AND reason = 'rewritten'",
        params![scene_id, atom_id],
    )?;
    insert_mark(
        conn,
        NewMark {
            scene_id: &scene_id,
            atom_id,
            reason: StaleReason::Rewritten,
            atom_version: Some(version),
            before: Some(kept.as_deref().unwrap_or(before)),
        },
        now,
    )
}

/// Atoms about to be deleted, with their text. A deletion replaces the
/// atom's open rewrite marks, keeping the oldest text the scene saw.
pub fn mark_deleted(
    conn: &Connection,
    atoms: &[(String, String)],
    now: i64,
) -> rusqlite::Result<()> {
    for (atom_id, content) in atoms {
        let Some(scene_id) = current_scene(conn, atom_id)? else {
            continue;
        };
        let before = earliest_before(conn, &scene_id, atom_id)?.unwrap_or_else(|| content.clone());
        conn.execute(
            "DELETE FROM scene_stale WHERE scene_id = ? AND atom_id = ? AND reason IN ('rewritten', 'changed_during_run')",
            params![scene_id, atom_id],
        )?;
        insert_mark(
            conn,
            NewMark {
                scene_id: &scene_id,
                atom_id,
                reason: StaleReason::Deleted,
                atom_version: None,
                before: Some(&before),
            },
            now,
        )?;
    }
    Ok(())
}

/// What a scene write means for the atoms; see
/// `ProfileVersionStore::append_scenes_linked`. `written` pairs each new
/// version with the one stored for it.
pub fn apply_links(
    conn: &Connection,
    team: &str,
    agent: &str,
    written: &[(NewSceneVersion, SceneVersion)],
    links: &SceneWriteLinks,
    now: i64,
) -> rusqlite::Result<()> {
    for (new, stored) in written {
        let scene_id = &stored.scene_id;
        for parent in &new.parents {
            if &parent.scene_id == scene_id {
                continue;
            }
            // A row the merged scene kept for an atom moved out of it gives
            // way to the atom coming back.
            conn.execute(
                "DELETE FROM scene_atoms WHERE scene_id = ?1 AND atom_id IN (SELECT atom_id FROM scene_atoms WHERE scene_id = ?2 AND state = 'current')",
                params![scene_id, parent.scene_id],
            )?;
            conn.execute(
                "UPDATE scene_atoms SET scene_id = ?1 WHERE scene_id = ?2 AND state = 'current'",
                params![scene_id, parent.scene_id],
            )?;
            conn.execute(
                "UPDATE scene_stale SET scene_id = ?1 WHERE scene_id = ?2",
                params![scene_id, parent.scene_id],
            )?;
        }
        match new.op {
            // A removed scene says nothing any more: nothing left to bring
            // up to date, so its marks go with it.
            SceneOp::Retire if !new.attrs.contains_key("merged_into") => {
                conn.execute(
                    "UPDATE scene_atoms SET state = 'moved_out' WHERE scene_id = ? AND state = 'current'",
                    [scene_id],
                )?;
                conn.execute("DELETE FROM scene_stale WHERE scene_id = ?", [scene_id])?;
            }
            SceneOp::Manual => {
                conn.execute("DELETE FROM scene_stale WHERE scene_id = ?", [scene_id])?;
            }
            _ => {}
        }
    }
    if let Some((_, target)) = written.first() {
        for member in &links.members {
            place(conn, target, member, now)?;
        }
    }
    if !links.resolved.is_empty() {
        let marks = vec!["?"; links.resolved.len()].join(",");
        let mut args: Vec<Value> =
            vec![Value::from(team.to_owned()), Value::from(agent.to_owned())];
        args.extend(links.resolved.iter().map(|id| Value::from(*id)));
        conn.execute(
            &format!("DELETE FROM scene_stale WHERE scene_id IN (SELECT id FROM scenes WHERE team_id = ? AND agent_id = ?) AND id IN ({marks})"),
            params_from_iter(args),
        )?;
    }
    Ok(())
}

/// Makes `member` a current atom of `target`.
fn place(
    conn: &Connection,
    target: &SceneVersion,
    member: &SceneMember,
    now: i64,
) -> rusqlite::Result<()> {
    let atom_id = member.atom_id.as_str();
    if let Some(other) = current_scene(conn, atom_id)?.filter(|s| s != &target.scene_id) {
        let before =
            earliest_before(conn, &other, atom_id)?.unwrap_or_else(|| member.content.clone());
        conn.execute(
            "UPDATE scene_atoms SET state = 'moved_out' WHERE scene_id = ? AND atom_id = ?",
            params![other, atom_id],
        )?;
        conn.execute(
            "DELETE FROM scene_stale WHERE scene_id = ? AND atom_id = ? AND reason IN ('rewritten', 'changed_during_run')",
            params![other, atom_id],
        )?;
        insert_mark(
            conn,
            NewMark {
                scene_id: &other,
                atom_id,
                reason: StaleReason::MovedOut,
                atom_version: Some(member.atom_version),
                before: Some(&before),
            },
            now,
        )?;
    }
    conn.execute(
        "INSERT INTO scene_atoms (scene_id, atom_id, atom_version, user_id, state, scene_version, assigned_at_ms) VALUES (?,?,?,?,'current',?,?)
         ON CONFLICT (scene_id, atom_id) DO UPDATE SET atom_version = excluded.atom_version, user_id = excluded.user_id,
             state = 'current', scene_version = excluded.scene_version, assigned_at_ms = excluded.assigned_at_ms",
        params![target.scene_id, atom_id, member.atom_version, member.user_id, target.version, now],
    )?;
    // Placed at the version L2 read: rewrites up to it are dealt with.
    conn.execute(
        "DELETE FROM scene_stale WHERE atom_id = ? AND reason IN ('rewritten', 'changed_during_run') AND (atom_version IS NULL OR atom_version <= ?)",
        params![atom_id, member.atom_version],
    )?;
    let stored: Option<u32> = conn
        .query_row("SELECT version FROM atoms WHERE id = ?", [atom_id], |r| {
            r.get(0)
        })
        .optional()?;
    match stored {
        None => insert_mark(
            conn,
            NewMark {
                scene_id: &target.scene_id,
                atom_id,
                reason: StaleReason::Deleted,
                atom_version: None,
                before: Some(&member.content),
            },
            now,
        )?,
        Some(v) if v > member.atom_version => insert_mark(
            conn,
            NewMark {
                scene_id: &target.scene_id,
                atom_id,
                reason: StaleReason::ChangedDuringRun,
                atom_version: Some(v),
                before: Some(&member.content),
            },
            now,
        )?,
        Some(_) => {}
    }
    Ok(())
}

const ATOM_COLUMNS: &str = "sa.scene_id, sa.atom_id, sa.atom_version, sa.user_id, sa.state, sa.scene_version, sa.assigned_at_ms";

fn atom_from_row(r: &Row<'_>) -> rusqlite::Result<SceneAtom> {
    let state: String = r.get("state")?;
    Ok(SceneAtom {
        scene_id: r.get("scene_id")?,
        atom_id: r.get("atom_id")?,
        atom_version: r.get("atom_version")?,
        user_id: r.get("user_id")?,
        state: MemberState::parse(&state).map_err(|e| invalid(e.detail))?,
        scene_version: r.get("scene_version")?,
        assigned_at: from_ms(r.get("assigned_at_ms")?),
    })
}

pub fn scene_atoms(
    conn: &Connection,
    team: &str,
    agent: &str,
    scene_id: &str,
) -> rusqlite::Result<Vec<SceneAtom>> {
    conn.prepare(&format!(
        "SELECT {ATOM_COLUMNS} FROM scene_atoms sa JOIN scenes s ON s.id = sa.scene_id
         WHERE s.team_id = ? AND s.agent_id = ? AND sa.scene_id = ? ORDER BY sa.assigned_at_ms, sa.atom_id"
    ))?
    .query_map(params![team, agent, scene_id], atom_from_row)?
    .collect()
}

const MARK_COLUMNS: &str = "st.id, st.scene_id, st.atom_id, st.reason, st.atom_version, st.before_content, st.attempts, st.marked_at_ms";

fn mark_from_row(r: &Row<'_>) -> rusqlite::Result<StaleMark> {
    let reason: String = r.get("reason")?;
    Ok(StaleMark {
        id: r.get("id")?,
        scene_id: r.get("scene_id")?,
        atom_id: r.get("atom_id")?,
        reason: StaleReason::parse(&reason).map_err(|e| invalid(e.detail))?,
        atom_version: r.get("atom_version")?,
        before_content: r.get("before_content")?,
        attempts: r.get("attempts")?,
        marked_at: from_ms(r.get("marked_at_ms")?),
    })
}

pub fn stale_marks(
    conn: &Connection,
    team: &str,
    agent: &str,
    scene_ids: Option<&[String]>,
) -> rusqlite::Result<Vec<StaleMark>> {
    let mut args: Vec<Value> = vec![Value::from(team.to_owned()), Value::from(agent.to_owned())];
    let only = match scene_ids {
        Some([]) => return Ok(Vec::new()),
        Some(ids) => {
            args.extend(ids.iter().map(|id| Value::from(id.clone())));
            format!("AND st.scene_id IN ({})", vec!["?"; ids.len()].join(","))
        }
        None => String::new(),
    };
    conn.prepare(&format!(
        "SELECT {MARK_COLUMNS} FROM scene_stale st JOIN scenes s ON s.id = st.scene_id
         WHERE s.team_id = ? AND s.agent_id = ? {only} ORDER BY st.id"
    ))?
    .query_map(params_from_iter(args), mark_from_row)?
    .collect()
}

pub fn note_attempt(
    conn: &Connection,
    team: &str,
    agent: &str,
    ids: &[i64],
) -> rusqlite::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut args: Vec<Value> = vec![Value::from(team.to_owned()), Value::from(agent.to_owned())];
    args.extend(ids.iter().map(|id| Value::from(*id)));
    conn.execute(
        &format!(
            "UPDATE scene_stale SET attempts = attempts + 1 WHERE scene_id IN (SELECT id FROM scenes WHERE team_id = ? AND agent_id = ?) AND id IN ({})",
            vec!["?"; ids.len()].join(",")
        ),
        params_from_iter(args),
    )?;
    Ok(())
}

fn invalid(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, message.into())
}
