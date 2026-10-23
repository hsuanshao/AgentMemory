//! DDL for the registry and for one department schema. Mirrors
//! `sqlite/schema.rs` table for table; differences are deliberate:
//!
//! * full-text tokens live in a `tsvector` column of the row itself (GIN
//!   indexed) instead of a separate FTS5 table;
//! * keys and every column used for ordering are `COLLATE "C"`, so ordering
//!   is bytewise like SQLite's default `BINARY` collation regardless of the
//!   database locale;
//! * integers are `BIGINT` throughout (one Rust type, `i64`, decodes all);
//! * documents are `JSONB`, so `touch_*` can patch them in place.

/// Fixed schema holding the department → schema registry.
pub const REGISTRY_SCHEMA: &str = "agent_memory";

/// Schema changes after the first release, oldest first. The DDL in this
/// file always describes the latest version; `db::migrate` upgrades older
/// databases step by step and records each step.
pub const MIGRATIONS: &[(i64, &str)] = &[
    (1, "instance renamed to department"),
    (2, "team role reviewer removed"),
    (3, "scenes and personas stored as append-only versions"),
    // Created with the embedding column (`db::migrate_embedding`), since
    // the column exists only when embeddings are on.
    (4, "index of atoms without an embedding"),
    (5, "administrators' redaction rules"),
];

/// The latest schema version.
pub const SCHEMA_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].0;

const MIGRATIONS_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS $R;
CREATE TABLE IF NOT EXISTS $R.schema_migrations (
    version BIGINT PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#;

pub fn migrations_ddl(registry: &str) -> String {
    MIGRATIONS_DDL.replace("$R", registry)
}

/// Creates the registry. `$R` is the quoted registry schema.
const REGISTRY_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS $R;
CREATE TABLE IF NOT EXISTS $R.departments (
    department_id TEXT COLLATE "C" PRIMARY KEY,
    schema_name TEXT COLLATE "C" NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#;

/// One department. `$S` is the quoted department schema, `$K` a bytewise text.
const DEPARTMENT_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS $S;

CREATE TABLE IF NOT EXISTS $S.conversations (
    seq BIGSERIAL PRIMARY KEY,
    id $K NOT NULL UNIQUE,
    team_id $K NOT NULL, agent_id $K NOT NULL, user_id $K NOT NULL, task_id $K,
    session_id $K NOT NULL, role $K NOT NULL, content TEXT NOT NULL,
    timestamp_ms BIGINT NOT NULL, recorded_at_ms BIGINT NOT NULL, version BIGINT NOT NULL,
    tokens TSVECTOR NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_conv_scope ON $S.conversations(team_id, agent_id, user_id, recorded_at_ms);
CREATE INDEX IF NOT EXISTS ix_conv_session ON $S.conversations(session_id, seq);
CREATE INDEX IF NOT EXISTS ix_conv_fts ON $S.conversations USING GIN (tokens);

CREATE TABLE IF NOT EXISTS $S.atoms (
    seq BIGSERIAL PRIMARY KEY,
    id $K NOT NULL UNIQUE,
    team_id $K NOT NULL, agent_id $K NOT NULL, user_id $K NOT NULL, task_id $K,
    session_id $K, atom_type $K NOT NULL, content TEXT NOT NULL, background TEXT,
    priority BIGINT NOT NULL, source_ids TEXT NOT NULL, version BIGINT NOT NULL,
    created_at_ms BIGINT NOT NULL, updated_at_ms BIGINT NOT NULL,
    tokens TSVECTOR NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_atoms_scope ON $S.atoms(team_id, agent_id, user_id, updated_at_ms);
CREATE INDEX IF NOT EXISTS ix_atoms_profile ON $S.atoms(team_id, agent_id, seq);
CREATE INDEX IF NOT EXISTS ix_atoms_fts ON $S.atoms USING GIN (tokens);

CREATE TABLE IF NOT EXISTS $S.profile_files (
    team_id $K NOT NULL, agent_id $K NOT NULL, path $K NOT NULL, version BIGINT NOT NULL,
    content TEXT NOT NULL, is_head BOOLEAN NOT NULL,
    created_at_ms BIGINT NOT NULL, updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (team_id, agent_id, path, version)
);
CREATE INDEX IF NOT EXISTS ix_profile_head ON $S.profile_files(team_id, agent_id, is_head, path);

CREATE TABLE IF NOT EXISTS $S.scenes (
    id $K PRIMARY KEY, team_id $K NOT NULL, agent_id $K NOT NULL, created_at_ms BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_scenes_profile ON $S.scenes(team_id, agent_id);
CREATE TABLE IF NOT EXISTS $S.scene_versions (
    scene_id $K NOT NULL REFERENCES $S.scenes(id) ON DELETE CASCADE, version BIGINT NOT NULL, op $K NOT NULL, slug $K NOT NULL,
    summary TEXT NOT NULL, heat BIGINT NOT NULL, body TEXT NOT NULL,
    attrs JSONB NOT NULL, parents JSONB NOT NULL, author $K, created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (scene_id, version)
);
CREATE INDEX IF NOT EXISTS ix_scene_versions_slug ON $S.scene_versions(slug);
CREATE TABLE IF NOT EXISTS $S.persona_versions (
    team_id $K NOT NULL, agent_id $K NOT NULL, version BIGINT NOT NULL, op $K NOT NULL,
    body TEXT NOT NULL, attrs JSONB NOT NULL, author $K, created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (team_id, agent_id, version)
);

CREATE TABLE IF NOT EXISTS $S.memory_prompts (
    id $K PRIMARY KEY, layer $K NOT NULL, status $K NOT NULL, created_at_ms BIGINT NOT NULL, doc JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS $S.prompt_settings (
    target_type $K NOT NULL, team_id $K NOT NULL, agent_id $K NOT NULL, layer $K NOT NULL,
    memory_prompt_id $K NOT NULL, updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (target_type, team_id, agent_id, layer)
);
CREATE TABLE IF NOT EXISTS $S.prompt_logs (
    id $K PRIMARY KEY, memory_prompt_id $K, layer $K NOT NULL, target_type $K NOT NULL,
    team_id $K, agent_id $K, action $K NOT NULL, created_at_ms BIGINT NOT NULL, doc JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS $S.generation_logs (
    log_id $K PRIMARY KEY, layer $K NOT NULL, status $K NOT NULL, started_at_ms BIGINT NOT NULL, doc JSONB NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_genlog_time ON $S.generation_logs(started_at_ms DESC, log_id DESC);
CREATE TABLE IF NOT EXISTS $S.generation_refs (
    seq BIGSERIAL PRIMARY KEY,
    layer $K NOT NULL, record_id $K NOT NULL, log_id $K NOT NULL, started_at_ms BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_genref ON $S.generation_refs(layer, record_id, started_at_ms DESC);

CREATE TABLE IF NOT EXISTS $S.skills (
    skill_id $K NOT NULL, version BIGINT NOT NULL, is_head BOOLEAN NOT NULL, name $K NOT NULL,
    status $K NOT NULL, team_id $K NOT NULL, owner_agent_id $K NOT NULL, owner_user_id $K NOT NULL,
    updated_at_ms BIGINT NOT NULL, created_at_ms BIGINT NOT NULL, doc JSONB NOT NULL,
    tokens TSVECTOR,
    PRIMARY KEY (skill_id, version)
);
CREATE INDEX IF NOT EXISTS ix_skills_head ON $S.skills(is_head, team_id, owner_agent_id, name);
CREATE INDEX IF NOT EXISTS ix_skills_fts ON $S.skills USING GIN (tokens);
CREATE TABLE IF NOT EXISTS $S.skill_buffers (
    buffer_key $K PRIMARY KEY, messages JSONB NOT NULL, tool_call_count BIGINT NOT NULL, byte_count BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS $S.skill_archives (archive_key $K PRIMARY KEY, messages JSONB NOT NULL, created_at_ms BIGINT NOT NULL);
CREATE TABLE IF NOT EXISTS $S.skill_tasks (task_id $K PRIMARY KEY, doc JSONB NOT NULL);

CREATE TABLE IF NOT EXISTS $S.knowledge (
    id $K PRIMARY KEY, team_id $K NOT NULL, kind $K NOT NULL, created_at $K NOT NULL, doc JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS $S.meta_users (
    user_id $K PRIMARY KEY, username $K NOT NULL UNIQUE, user_type $K NOT NULL, status $K NOT NULL,
    created_at $K NOT NULL, doc JSONB NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_single_system_admin ON $S.meta_users(user_type) WHERE user_type = 'system_admin';
CREATE TABLE IF NOT EXISTS $S.meta_user_keys (
    key_id $K PRIMARY KEY, user_id $K NOT NULL, key_value $K NOT NULL UNIQUE, created_at $K NOT NULL, doc JSONB NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_keys_user ON $S.meta_user_keys(user_id, created_at);
CREATE TABLE IF NOT EXISTS $S.meta_teams (
    team_id $K PRIMARY KEY, name $K NOT NULL, created_at $K NOT NULL, doc JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS $S.meta_team_members (
    team_id $K NOT NULL, user_id $K NOT NULL, status $K NOT NULL, joined_at $K NOT NULL, doc JSONB NOT NULL,
    PRIMARY KEY (team_id, user_id)
);
CREATE TABLE IF NOT EXISTS $S.meta_agents (
    agent_id $K PRIMARY KEY, team_id $K NOT NULL, owner_user_id $K NOT NULL, status $K NOT NULL,
    name $K NOT NULL, created_at $K NOT NULL, doc JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS $S.meta_tasks (
    task_id $K PRIMARY KEY, team_id $K NOT NULL, creator_user_id $K NOT NULL, status $K NOT NULL,
    created_at $K NOT NULL, doc JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS $S.meta_task_agents (
    task_id $K NOT NULL, agent_id $K NOT NULL, created_at $K NOT NULL, doc JSONB NOT NULL,
    PRIMARY KEY (task_id, agent_id)
);
CREATE TABLE IF NOT EXISTS $S.meta_participation_logs (
    id $K PRIMARY KEY, team_id $K NOT NULL, task_id $K NOT NULL, agent_id $K NOT NULL, user_id $K NOT NULL,
    created_at $K NOT NULL, doc JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS $S.meta_assets (
    asset_id $K PRIMARY KEY, team_id $K NOT NULL, asset_type $K NOT NULL, status $K NOT NULL,
    owner_user_id $K NOT NULL, visibility $K NOT NULL, created_at $K NOT NULL, doc JSONB NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_assets_team ON $S.meta_assets(team_id, created_at DESC);
CREATE TABLE IF NOT EXISTS $S.meta_agent_bindings (
    agent_id $K NOT NULL, asset_id $K NOT NULL, priority BIGINT NOT NULL, doc JSONB NOT NULL,
    PRIMARY KEY (agent_id, asset_id)
);
CREATE INDEX IF NOT EXISTS ix_bindings_asset ON $S.meta_agent_bindings(asset_id);
CREATE TABLE IF NOT EXISTS $S.meta_asset_acl (
    id $K PRIMARY KEY, asset_id $K NOT NULL, subject_type $K NOT NULL, subject_id $K NOT NULL,
    permission $K NOT NULL, doc JSONB NOT NULL,
    UNIQUE (asset_id, subject_type, subject_id, permission)
);
CREATE TABLE IF NOT EXISTS $S.meta_redaction_rules (
    rule_id $K PRIMARY KEY, created_at $K NOT NULL, doc JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS $S.meta_config_params (
    scope $K NOT NULL, user_id $K NOT NULL, module $K NOT NULL, param_name $K NOT NULL, param_value TEXT NOT NULL,
    PRIMARY KEY (scope, user_id, module, param_name)
);
INSERT INTO $S.meta_config_params VALUES
    ('global', '', 'quota', 'max_users_per_department', '500'),
    ('global', '', 'quota', 'max_teams_per_department', '100'),
    ('global', '', 'asset_type', 'skill.enabled', '1'),
    ('global', '', 'asset_type', 'llm_wiki.enabled', '1'),
    ('global', '', 'asset_type', 'code_graph.enabled', '1'),
    ('global', '', 'asset_type', 'chat_memory.enabled', '1')
ON CONFLICT DO NOTHING;
"#;

/// HNSW indexes on `vector` support at most this many dimensions; above
/// it vector search falls back to an exact scan.
pub const HNSW_MAX_DIMS: usize = 2000;

pub fn registry_ddl(registry: &str) -> String {
    REGISTRY_DDL.replace("$R", registry)
}

pub fn department_ddl(schema: &str) -> String {
    DEPARTMENT_DDL
        .replace("$S", schema)
        .replace("$K", "TEXT COLLATE \"C\"")
}

/// Adds the embedding column (no-op when present).
pub fn embedding_column_ddl(schema: &str, dims: usize) -> String {
    format!("ALTER TABLE {schema}.atoms ADD COLUMN IF NOT EXISTS embedding vector({dims})")
}

/// Cosine HNSW index over the embedding column.
pub fn embedding_index_ddl(schema: &str) -> String {
    format!(
        "CREATE INDEX IF NOT EXISTS ix_atoms_embedding ON {schema}.atoms USING hnsw (embedding vector_cosine_ops)"
    )
}
