//! DDL. Hot query fields are real columns; metadata entities additionally
//! keep their full serialized form in `doc` so the mapping stays in serde.

use memory_domain::profile::{import_legacy_files, LegacyProfileFile};
use rusqlite::{params, Connection};

use crate::profile_versions::{attrs_json, legacy_scene_id, parents_json};

const DEPARTMENT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS conversations (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    team_id TEXT NOT NULL, agent_id TEXT NOT NULL, user_id TEXT NOT NULL, task_id TEXT,
    session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL, recorded_at_ms INTEGER NOT NULL, version INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_conv_scope ON conversations(team_id, agent_id, user_id, recorded_at_ms);
CREATE INDEX IF NOT EXISTS ix_conv_session ON conversations(session_id, seq);
CREATE VIRTUAL TABLE IF NOT EXISTS conversations_fts USING fts5(tokens, id UNINDEXED);

CREATE TABLE IF NOT EXISTS atoms (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    team_id TEXT NOT NULL, agent_id TEXT NOT NULL, user_id TEXT NOT NULL, task_id TEXT,
    session_id TEXT, atom_type TEXT NOT NULL, content TEXT NOT NULL, background TEXT,
    priority INTEGER NOT NULL, source_ids TEXT NOT NULL, version INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL, embedding BLOB
);
CREATE INDEX IF NOT EXISTS ix_atoms_scope ON atoms(team_id, agent_id, user_id, updated_at_ms);
CREATE INDEX IF NOT EXISTS ix_atoms_profile ON atoms(team_id, agent_id, seq);
CREATE INDEX IF NOT EXISTS ix_atoms_missing_embedding ON atoms(seq) WHERE embedding IS NULL;
CREATE VIRTUAL TABLE IF NOT EXISTS atoms_fts USING fts5(tokens, id UNINDEXED);
-- Bookkeeping for derived indexes (e.g. the sqlite-vec atom index's dims).
CREATE TABLE IF NOT EXISTS index_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS profile_files (
    team_id TEXT NOT NULL, agent_id TEXT NOT NULL, path TEXT NOT NULL, version INTEGER NOT NULL,
    content TEXT NOT NULL, is_head INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (team_id, agent_id, path, version)
);
CREATE INDEX IF NOT EXISTS ix_profile_head ON profile_files(team_id, agent_id, is_head, path);

-- Scenes (L2) and personas (L3) are append-only versions; profile_files is
-- the storage up to 0.4, kept after its rows were imported.
CREATE TABLE IF NOT EXISTS scenes (
    id TEXT PRIMARY KEY, team_id TEXT NOT NULL, agent_id TEXT NOT NULL, created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_scenes_profile ON scenes(team_id, agent_id);
CREATE TABLE IF NOT EXISTS scene_versions (
    scene_id TEXT NOT NULL REFERENCES scenes(id) ON DELETE CASCADE, version INTEGER NOT NULL, op TEXT NOT NULL, slug TEXT NOT NULL,
    summary TEXT NOT NULL, heat INTEGER NOT NULL, body TEXT NOT NULL,
    attrs TEXT NOT NULL, parents TEXT NOT NULL, author TEXT, created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (scene_id, version)
);
CREATE INDEX IF NOT EXISTS ix_scene_versions_slug ON scene_versions(slug);
CREATE TABLE IF NOT EXISTS persona_versions (
    team_id TEXT NOT NULL, agent_id TEXT NOT NULL, version INTEGER NOT NULL, op TEXT NOT NULL,
    body TEXT NOT NULL, attrs TEXT NOT NULL, author TEXT, created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (team_id, agent_id, version)
);

CREATE TABLE IF NOT EXISTS memory_prompts (
    id TEXT PRIMARY KEY, layer TEXT NOT NULL, status TEXT NOT NULL, created_at_ms INTEGER NOT NULL, doc TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS prompt_settings (
    target_type TEXT NOT NULL, team_id TEXT NOT NULL, agent_id TEXT NOT NULL, layer TEXT NOT NULL,
    memory_prompt_id TEXT NOT NULL, updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (target_type, team_id, agent_id, layer)
);
CREATE TABLE IF NOT EXISTS prompt_logs (
    id TEXT PRIMARY KEY, memory_prompt_id TEXT, layer TEXT NOT NULL, target_type TEXT NOT NULL,
    team_id TEXT, agent_id TEXT, action TEXT NOT NULL, created_at_ms INTEGER NOT NULL, doc TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS generation_logs (
    log_id TEXT PRIMARY KEY, layer TEXT NOT NULL, status TEXT NOT NULL, started_at_ms INTEGER NOT NULL, doc TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_genlog_time ON generation_logs(started_at_ms DESC, log_id DESC);
CREATE TABLE IF NOT EXISTS generation_refs (
    layer TEXT NOT NULL, record_id TEXT NOT NULL, log_id TEXT NOT NULL, started_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_genref ON generation_refs(layer, record_id, started_at_ms DESC);

CREATE TABLE IF NOT EXISTS skills (
    skill_id TEXT NOT NULL, version INTEGER NOT NULL, is_head INTEGER NOT NULL, name TEXT NOT NULL,
    status TEXT NOT NULL, team_id TEXT NOT NULL, owner_agent_id TEXT NOT NULL, owner_user_id TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL, created_at_ms INTEGER NOT NULL, doc TEXT NOT NULL,
    PRIMARY KEY (skill_id, version)
);
CREATE INDEX IF NOT EXISTS ix_skills_head ON skills(is_head, team_id, owner_agent_id, name);
CREATE VIRTUAL TABLE IF NOT EXISTS skills_fts USING fts5(tokens, skill_id UNINDEXED);
CREATE TABLE IF NOT EXISTS skill_buffers (
    buffer_key TEXT PRIMARY KEY, messages TEXT NOT NULL, tool_call_count INTEGER NOT NULL, byte_count INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS skill_archives (archive_key TEXT PRIMARY KEY, messages TEXT NOT NULL, created_at_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS skill_tasks (task_id TEXT PRIMARY KEY, doc TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS knowledge (
    id TEXT PRIMARY KEY, team_id TEXT NOT NULL, kind TEXT NOT NULL, created_at TEXT NOT NULL, doc TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS meta_users (
    user_id TEXT PRIMARY KEY, username TEXT NOT NULL UNIQUE, user_type TEXT NOT NULL, status TEXT NOT NULL,
    created_at TEXT NOT NULL, doc TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_single_system_admin ON meta_users(user_type) WHERE user_type = 'system_admin';
CREATE TABLE IF NOT EXISTS meta_user_keys (
    key_id TEXT PRIMARY KEY, user_id TEXT NOT NULL, key_value TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL, doc TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_keys_user ON meta_user_keys(user_id, created_at);
CREATE TABLE IF NOT EXISTS meta_teams (
    team_id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at TEXT NOT NULL, doc TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta_team_members (
    team_id TEXT NOT NULL, user_id TEXT NOT NULL, status TEXT NOT NULL, joined_at TEXT NOT NULL, doc TEXT NOT NULL,
    PRIMARY KEY (team_id, user_id)
);
CREATE TABLE IF NOT EXISTS meta_agents (
    agent_id TEXT PRIMARY KEY, team_id TEXT NOT NULL, owner_user_id TEXT NOT NULL, status TEXT NOT NULL,
    name TEXT NOT NULL, created_at TEXT NOT NULL, doc TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta_tasks (
    task_id TEXT PRIMARY KEY, team_id TEXT NOT NULL, creator_user_id TEXT NOT NULL, status TEXT NOT NULL,
    created_at TEXT NOT NULL, doc TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta_task_agents (
    task_id TEXT NOT NULL, agent_id TEXT NOT NULL, created_at TEXT NOT NULL, doc TEXT NOT NULL,
    PRIMARY KEY (task_id, agent_id)
);
CREATE TABLE IF NOT EXISTS meta_participation_logs (
    id TEXT PRIMARY KEY, team_id TEXT NOT NULL, task_id TEXT NOT NULL, agent_id TEXT NOT NULL, user_id TEXT NOT NULL,
    created_at TEXT NOT NULL, doc TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta_assets (
    asset_id TEXT PRIMARY KEY, team_id TEXT NOT NULL, asset_type TEXT NOT NULL, status TEXT NOT NULL,
    owner_user_id TEXT NOT NULL, visibility TEXT NOT NULL, created_at TEXT NOT NULL, doc TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_assets_team ON meta_assets(team_id, created_at DESC);
CREATE TABLE IF NOT EXISTS meta_agent_bindings (
    agent_id TEXT NOT NULL, asset_id TEXT NOT NULL, priority INTEGER NOT NULL, doc TEXT NOT NULL,
    PRIMARY KEY (agent_id, asset_id)
);
CREATE INDEX IF NOT EXISTS ix_bindings_asset ON meta_agent_bindings(asset_id);
CREATE TABLE IF NOT EXISTS meta_asset_acl (
    id TEXT PRIMARY KEY, asset_id TEXT NOT NULL, subject_type TEXT NOT NULL, subject_id TEXT NOT NULL,
    permission TEXT NOT NULL, doc TEXT NOT NULL,
    UNIQUE (asset_id, subject_type, subject_id, permission)
);
CREATE TABLE IF NOT EXISTS meta_config_params (
    scope TEXT NOT NULL, user_id TEXT NOT NULL, module TEXT NOT NULL, param_name TEXT NOT NULL, param_value TEXT NOT NULL,
    PRIMARY KEY (scope, user_id, module, param_name)
);
INSERT OR IGNORE INTO meta_config_params VALUES
    ('global', '', 'quota', 'max_users_per_department', '500'),
    ('global', '', 'quota', 'max_teams_per_department', '100'),
    ('global', '', 'asset_type', 'skill.enabled', '1'),
    ('global', '', 'asset_type', 'llm_wiki.enabled', '1'),
    ('global', '', 'asset_type', 'code_graph.enabled', '1'),
    ('global', '', 'asset_type', 'chat_memory.enabled', '1');
"#;

const SYSTEM_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS session_states (
    department TEXT NOT NULL, scope_key TEXT NOT NULL, session_id TEXT NOT NULL,
    l1_due_ms INTEGER, last_active_ms INTEGER NOT NULL, doc TEXT NOT NULL,
    PRIMARY KEY (department, scope_key, session_id)
);
CREATE INDEX IF NOT EXISTS ix_session_due ON session_states(l1_due_ms) WHERE l1_due_ms IS NOT NULL;
CREATE TABLE IF NOT EXISTS profile_states (
    department TEXT NOT NULL, profile_key TEXT NOT NULL, l2_due_ms INTEGER, updated_ms INTEGER NOT NULL, doc TEXT NOT NULL,
    PRIMARY KEY (department, profile_key)
);
CREATE INDEX IF NOT EXISTS ix_profile_due ON profile_states(l2_due_ms) WHERE l2_due_ms IS NOT NULL;
"#;

/// Schema version of both database kinds, kept in `PRAGMA user_version`.
/// Each step below upgrades from the version before it; the DDL above
/// always describes the latest version.
///
/// 1. 0.3.0: "instance" became "department" (column, prompt target and
///    quota names).
/// 2. 0.3.0: the team role `reviewer` was removed; its members are members.
/// 3. Scenes and personas became append-only versions (`scenes`,
///    `scene_versions`, `persona_versions`); `profile_files` is imported.
/// 4. A partial index of the atoms without an embedding
///    (`ix_atoms_missing_embedding`), the embedding backfill's queue. The
///    DDL above creates it; nothing is converted.
pub const SCHEMA_VERSION: i64 = 4;

fn user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

/// Refuses a database written by a newer memory-server. Running on it would
/// ignore what the newer version stores, and rewriting `user_version` would
/// make a later upgrade replay migrations over data written since.
fn refuse_newer(conn: &Connection) -> rusqlite::Result<()> {
    let found = user_version(conn)?;
    if found <= SCHEMA_VERSION {
        return Ok(());
    }
    Err(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(format!(
            "the database is at schema version {found}, newer than this memory-server knows \
             ({SCHEMA_VERSION}): run the newer version, or restore the backup taken before the \
             upgrade (docs/UPGRADE.md, Roll back)"
        )),
    ))
}

fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT count(*) FROM pragma_table_info(?1) WHERE name = ?2",
        [table, column],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

pub fn migrate_department(conn: &Connection) -> rusqlite::Result<()> {
    refuse_newer(conn)?;
    conn.execute_batch(DEPARTMENT_SCHEMA)?;
    if user_version(conn)? < 1 {
        conn.execute_batch(
            "BEGIN;
             UPDATE prompt_settings SET target_type = 'department' WHERE target_type = 'instance';
             UPDATE prompt_logs SET target_type = 'department' WHERE target_type = 'instance';
             UPDATE OR REPLACE meta_config_params SET param_name = replace(param_name, '_per_instance', '_per_department')
             WHERE module = 'quota' AND param_name IN ('max_users_per_instance', 'max_teams_per_instance');
             COMMIT;",
        )?;
    }
    if user_version(conn)? < 2 {
        conn.execute(
            "UPDATE meta_team_members SET doc = json_set(doc, '$.role', 'member') WHERE json_extract(doc, '$.role') = 'reviewer'",
            [],
        )?;
    }
    if user_version(conn)? < 3 {
        import_profile_files(conn)?;
    }
    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
}

/// Migration 3: every stored file version becomes a scene or persona
/// version. `INSERT OR IGNORE` keeps a repeated run harmless.
fn import_profile_files(conn: &Connection) -> rusqlite::Result<()> {
    let files = {
        let mut stmt = conn.prepare(
            "SELECT team_id, agent_id, path, version, content, created_at_ms, updated_at_ms FROM profile_files",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(LegacyProfileFile {
                team_id: r.get(0)?,
                agent_id: r.get(1)?,
                path: r.get(2)?,
                version: r.get(3)?,
                content: r.get(4)?,
                created_at_ms: r.get(5)?,
                updated_at_ms: r.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let (scenes, personas) = import_legacy_files(files, legacy_scene_id);
    let tx = conn.unchecked_transaction()?;
    for s in &scenes {
        let v = &s.scene;
        tx.execute(
            "INSERT OR IGNORE INTO scenes (id, team_id, agent_id, created_at_ms) VALUES (?,?,?,?)",
            params![
                v.scene_id,
                s.team_id,
                s.agent_id,
                v.scene_created_at.timestamp_millis()
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO scene_versions (scene_id, version, op, slug, summary, heat, body, attrs, parents, author, created_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            params![
                v.scene_id,
                v.version,
                v.op.as_str(),
                v.slug,
                v.summary,
                v.heat,
                v.body,
                attrs_json(&v.attrs),
                parents_json(&v.parents),
                v.author,
                v.created_at.timestamp_millis()
            ],
        )?;
    }
    for p in &personas {
        let v = &p.persona;
        tx.execute(
            "INSERT OR IGNORE INTO persona_versions (team_id, agent_id, version, op, body, attrs, author, created_at_ms) VALUES (?,?,?,?,?,?,?,?)",
            params![
                p.team_id,
                p.agent_id,
                v.version,
                v.op.as_str(),
                v.body,
                attrs_json(&v.attrs),
                v.author,
                v.created_at.timestamp_millis()
            ],
        )?;
    }
    tx.commit()
}

pub fn migrate_system(conn: &Connection) -> rusqlite::Result<()> {
    refuse_newer(conn)?;
    // Before the DDL: its indexes name the new column.
    if user_version(conn)? < 1 {
        let tx = conn.unchecked_transaction()?;
        for table in ["session_states", "profile_states"] {
            if has_column(&tx, table, "instance")? {
                tx.execute_batch(&format!(
                    "ALTER TABLE {table} RENAME COLUMN instance TO department;
                     UPDATE {table} SET doc = json_set(json_remove(doc, '$.instance'), '$.department', json_extract(doc, '$.instance'))
                     WHERE json_type(doc, '$.instance') IS NOT NULL;"
                ))?;
            }
        }
        tx.commit()?;
    }
    conn.execute_batch(SYSTEM_SCHEMA)?;
    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_db_from_before_departments_is_renamed() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"CREATE TABLE session_states (
                instance TEXT NOT NULL, scope_key TEXT NOT NULL, session_id TEXT NOT NULL,
                l1_due_ms INTEGER, last_active_ms INTEGER NOT NULL, doc TEXT NOT NULL,
                PRIMARY KEY (instance, scope_key, session_id));
            CREATE TABLE profile_states (
                instance TEXT NOT NULL, profile_key TEXT NOT NULL, l2_due_ms INTEGER, updated_ms INTEGER NOT NULL, doc TEXT NOT NULL,
                PRIMARY KEY (instance, profile_key));
            INSERT INTO session_states VALUES ('default', 'k', 's', NULL, 1, '{"instance":"default","session_id":"s"}');
            INSERT INTO profile_states VALUES ('default', 'p', NULL, 1, '{"instance":"default"}');"#,
        )
        .unwrap();

        migrate_system(&conn).unwrap();
        migrate_system(&conn).unwrap();

        let (department, doc): (String, String) = conn
            .query_row("SELECT department, doc FROM session_states", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(department, "default");
        let doc: serde_json::Value = serde_json::from_str(&doc).unwrap();
        assert_eq!(
            doc,
            serde_json::json!({"department": "default", "session_id": "s"})
        );
        let doc: String = conn
            .query_row("SELECT doc FROM profile_states", [], |r| r.get(0))
            .unwrap();
        assert_eq!(doc, r#"{"department":"default"}"#);
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn department_db_from_0_2_is_migrated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(DEPARTMENT_SCHEMA).unwrap();
        conn.execute_batch(
            r#"INSERT INTO prompt_settings VALUES ('instance', '', '', 'l1', 'p-1', 1);
             INSERT INTO meta_config_params VALUES ('global', '', 'quota', 'max_users_per_instance', '20');
             INSERT INTO meta_team_members VALUES ('t-1', 'u-1', 'active', '', '{"team_id":"t-1","user_id":"u-1","role":"reviewer"}');"#,
        )
        .unwrap();

        migrate_department(&conn).unwrap();

        let target: String = conn
            .query_row("SELECT target_type FROM prompt_settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(target, "department");
        let users: String = conn
            .query_row(
                "SELECT param_value FROM meta_config_params WHERE param_name = 'max_users_per_department'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(users, "20", "the operator's value replaces the default");
        let old: i64 = conn
            .query_row(
                "SELECT count(*) FROM meta_config_params WHERE param_name LIKE '%instance'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old, 0);
        let role: String = conn
            .query_row(
                "SELECT json_extract(doc, '$.role') FROM meta_team_members",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(role, "member", "reviewers become members");
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn department_db_from_0_4_1_gets_the_backfill_index() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(DEPARTMENT_SCHEMA).unwrap();
        conn.execute_batch("DROP INDEX ix_atoms_missing_embedding; PRAGMA user_version = 3")
            .unwrap();
        migrate_department(&conn).unwrap();
        let indexed: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'ix_atoms_missing_embedding'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1);
        assert_eq!(user_version(&conn).unwrap(), 4);
    }

    #[test]
    fn department_db_from_0_4_imports_profile_files() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(DEPARTMENT_SCHEMA).unwrap();
        conn.execute_batch("PRAGMA user_version = 2").unwrap();
        let scene = |summary: &str, body: &str| {
            format!("-----META-START-----\ncreated: c\nupdated: u\nsummary: {summary}\nheat: 2\nowner: ops\n-----META-END-----\n\n{body}")
        };
        let rows = [
            ("scene_blocks/deploy.md", 1, scene("old", "v1"), 0, 10, 10),
            ("scene_blocks/deploy.md", 2, scene("new", "v2"), 1, 10, 20),
            ("persona.md", 1, "# Me".to_owned(), 0, 5, 5),
            (
                "persona.md",
                2,
                "# Me\n\n---\n## 🗺️ Scene Navigation (Scene Index)\nx".to_owned(),
                0,
                5,
                6,
            ),
            ("persona.md", 3, "# Me, revised".to_owned(), 1, 5, 7),
        ];
        for (path, version, content, head, created, updated) in rows {
            conn.execute(
                "INSERT INTO profile_files VALUES ('t', 'a', ?, ?, ?, ?, ?, ?)",
                params![path, version, content, head, created, updated],
            )
            .unwrap();
        }

        migrate_department(&conn).unwrap();
        migrate_department(&conn).unwrap();

        let scenes: Vec<(String, u32, String, String, String, String)> = conn
            .prepare("SELECT slug, version, op, summary, body, attrs FROM scene_versions ORDER BY version")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(scenes.len(), 2);
        assert_eq!(scenes[1].0, "deploy.md");
        assert_eq!((scenes[1].1, scenes[1].2.as_str()), (2, "import"));
        assert_eq!((scenes[1].3.as_str(), scenes[1].4.as_str()), ("new", "v2"));
        assert_eq!(
            scenes[1].5, r#"{"owner":"ops"}"#,
            "unknown META keys are kept"
        );
        let created: i64 = conn
            .query_row("SELECT created_at_ms FROM scenes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(created, 10);
        let personas: Vec<(u32, String)> = conn
            .prepare("SELECT version, body FROM persona_versions ORDER BY version")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            personas,
            vec![(1, "# Me".to_owned()), (3, "# Me, revised".to_owned())],
            "a version that only refreshed the navigation is skipped"
        );
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn a_newer_database_is_refused_and_left_alone() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 1))
            .unwrap();
        for migrate in [migrate_department, migrate_system] {
            let err = migrate(&conn).unwrap_err().to_string();
            assert!(err.contains("newer than this memory-server"), "{err}");
        }
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION + 1);
    }
}
