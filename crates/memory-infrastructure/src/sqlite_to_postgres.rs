//! One-shot copy of every SQLite instance database into Postgres, for an
//! install outgrowing the single-node backend.
//!
//! Rows are copied as stored (ids, `seq` numbers, versions, timestamps), so
//! pipeline cursors, which point at `seq`, resume exactly where they
//! stopped. Derived data is rebuilt instead of copied: keyword tokens
//! become `tsvector`s, and the sqlite-vec index gives way to the `embedding`
//! column. Files under the data directory (L2/L3 objects, skill resources)
//! and the local coordination state are shared by both backends and stay
//! where they are.
//!
//! The source must not change while this runs: stop memory-core first.
//! Each instance is copied in one transaction and checked by row count
//! before it commits, so a failure leaves its Postgres schema empty and the
//! run can simply be repeated. An instance whose schema already holds
//! exactly the source's rows is reported as already migrated.

use std::path::{Path, PathBuf};

use memory_domain::error::{DomainError, DomainResult};
use memory_domain::scope::{InstanceId, SegmentId};
use memory_domain::skill::SkillVersion;
use pgvector::Vector;
use rusqlite::types::Value;
use serde::Serialize;

use crate::postgres::db::{pg_err, quote_ident, schema_name, PgDb};
use crate::postgres::schema::REGISTRY_SCHEMA;
use crate::postgres::skill_store::index_source;
use crate::postgres::sql::{args, count, execute, fetch_optional, tsvector_literal, Arg};
use crate::sqlite::db::Db;
use crate::sqlite::schema;
use crate::sqlite::sql::blob_to_embedding;

/// Rows read from SQLite per round trip.
const PAGE_ROWS: usize = 1_000;
/// Bound parameters per INSERT, well under Postgres' 65 535.
const MAX_PARAMS: usize = 30_000;

#[derive(Debug, Clone, Copy)]
enum Kind {
    Text,
    Int,
    /// SQLite `INTEGER` 0/1 → `BOOLEAN`.
    Bool,
    /// SQLite `TEXT` holding JSON → `JSONB`.
    Json,
}

/// Columns Postgres needs beyond the copied ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Derived {
    None,
    /// `tokens` from `content`.
    ConversationTokens,
    /// `tokens` from `content` + `background`, and `embedding` from the BLOB.
    AtomTokensAndEmbedding,
    /// `tokens` for head versions, from the skill document.
    SkillTokens,
}

struct Table {
    name: &'static str,
    columns: &'static [(&'static str, Kind)],
    derived: Derived,
    /// For tables the Postgres DDL seeds: the source row wins.
    upsert: Option<&'static str>,
    /// A `BIGSERIAL seq` copied verbatim, whose sequence must move past it.
    copies_seq: bool,
}

use Kind::{Bool, Int, Json, Text};

const fn table(name: &'static str, columns: &'static [(&'static str, Kind)]) -> Table {
    Table {
        name,
        columns,
        derived: Derived::None,
        upsert: None,
        copies_seq: false,
    }
}

/// Every table of `sqlite/schema.rs` except derived ones (FTS5 tables,
/// `index_meta`, the sqlite-vec index). A unit test keeps this in step
/// with the SQLite DDL.
const TABLES: &[Table] = &[
    Table {
        derived: Derived::ConversationTokens,
        copies_seq: true,
        ..table(
            "conversations",
            &[
                ("seq", Int),
                ("id", Text),
                ("team_id", Text),
                ("agent_id", Text),
                ("user_id", Text),
                ("task_id", Text),
                ("session_id", Text),
                ("role", Text),
                ("content", Text),
                ("timestamp_ms", Int),
                ("recorded_at_ms", Int),
                ("version", Int),
            ],
        )
    },
    Table {
        derived: Derived::AtomTokensAndEmbedding,
        copies_seq: true,
        ..table(
            "atoms",
            &[
                ("seq", Int),
                ("id", Text),
                ("team_id", Text),
                ("agent_id", Text),
                ("user_id", Text),
                ("task_id", Text),
                ("session_id", Text),
                ("atom_type", Text),
                ("content", Text),
                ("background", Text),
                ("priority", Int),
                ("source_ids", Text),
                ("version", Int),
                ("created_at_ms", Int),
                ("updated_at_ms", Int),
            ],
        )
    },
    table(
        "profile_files",
        &[
            ("team_id", Text),
            ("agent_id", Text),
            ("path", Text),
            ("version", Int),
            ("content", Text),
            ("is_head", Bool),
            ("created_at_ms", Int),
            ("updated_at_ms", Int),
        ],
    ),
    table(
        "memory_prompts",
        &[
            ("id", Text),
            ("layer", Text),
            ("status", Text),
            ("created_at_ms", Int),
            ("doc", Json),
        ],
    ),
    table(
        "prompt_settings",
        &[
            ("target_type", Text),
            ("team_id", Text),
            ("agent_id", Text),
            ("layer", Text),
            ("memory_prompt_id", Text),
            ("updated_at_ms", Int),
        ],
    ),
    table(
        "prompt_logs",
        &[
            ("id", Text),
            ("memory_prompt_id", Text),
            ("layer", Text),
            ("target_type", Text),
            ("team_id", Text),
            ("agent_id", Text),
            ("action", Text),
            ("created_at_ms", Int),
            ("doc", Json),
        ],
    ),
    table(
        "generation_logs",
        &[
            ("log_id", Text),
            ("layer", Text),
            ("status", Text),
            ("started_at_ms", Int),
            ("doc", Json),
        ],
    ),
    // Postgres numbers these itself (`seq`); copying in rowid order keeps
    // their relative order.
    table(
        "generation_refs",
        &[
            ("layer", Text),
            ("record_id", Text),
            ("log_id", Text),
            ("started_at_ms", Int),
        ],
    ),
    Table {
        derived: Derived::SkillTokens,
        ..table(
            "skills",
            &[
                ("skill_id", Text),
                ("version", Int),
                ("is_head", Bool),
                ("name", Text),
                ("status", Text),
                ("team_id", Text),
                ("owner_agent_id", Text),
                ("owner_user_id", Text),
                ("updated_at_ms", Int),
                ("created_at_ms", Int),
                ("doc", Json),
            ],
        )
    },
    table(
        "skill_buffers",
        &[
            ("buffer_key", Text),
            ("messages", Json),
            ("tool_call_count", Int),
            ("byte_count", Int),
        ],
    ),
    table(
        "skill_archives",
        &[
            ("archive_key", Text),
            ("messages", Json),
            ("created_at_ms", Int),
        ],
    ),
    table("skill_tasks", &[("task_id", Text), ("doc", Json)]),
    table(
        "knowledge",
        &[
            ("id", Text),
            ("team_id", Text),
            ("kind", Text),
            ("created_at", Text),
            ("doc", Json),
        ],
    ),
    table(
        "meta_users",
        &[
            ("user_id", Text),
            ("username", Text),
            ("user_type", Text),
            ("status", Text),
            ("created_at", Text),
            ("doc", Json),
        ],
    ),
    table(
        "meta_user_keys",
        &[
            ("key_id", Text),
            ("user_id", Text),
            ("key_value", Text),
            ("created_at", Text),
            ("doc", Json),
        ],
    ),
    table(
        "meta_teams",
        &[
            ("team_id", Text),
            ("name", Text),
            ("created_at", Text),
            ("doc", Json),
        ],
    ),
    table(
        "meta_team_members",
        &[
            ("team_id", Text),
            ("user_id", Text),
            ("status", Text),
            ("joined_at", Text),
            ("doc", Json),
        ],
    ),
    table(
        "meta_agents",
        &[
            ("agent_id", Text),
            ("team_id", Text),
            ("owner_user_id", Text),
            ("status", Text),
            ("name", Text),
            ("created_at", Text),
            ("doc", Json),
        ],
    ),
    table(
        "meta_tasks",
        &[
            ("task_id", Text),
            ("team_id", Text),
            ("creator_user_id", Text),
            ("status", Text),
            ("created_at", Text),
            ("doc", Json),
        ],
    ),
    table(
        "meta_task_agents",
        &[
            ("task_id", Text),
            ("agent_id", Text),
            ("created_at", Text),
            ("doc", Json),
        ],
    ),
    table(
        "meta_participation_logs",
        &[
            ("id", Text),
            ("team_id", Text),
            ("task_id", Text),
            ("agent_id", Text),
            ("user_id", Text),
            ("created_at", Text),
            ("doc", Json),
        ],
    ),
    table(
        "meta_assets",
        &[
            ("asset_id", Text),
            ("team_id", Text),
            ("asset_type", Text),
            ("status", Text),
            ("owner_user_id", Text),
            ("visibility", Text),
            ("created_at", Text),
            ("doc", Json),
        ],
    ),
    table(
        "meta_agent_bindings",
        &[
            ("agent_id", Text),
            ("asset_id", Text),
            ("priority", Int),
            ("doc", Json),
        ],
    ),
    table(
        "meta_asset_acl",
        &[
            ("id", Text),
            ("asset_id", Text),
            ("subject_type", Text),
            ("subject_id", Text),
            ("permission", Text),
            ("doc", Json),
        ],
    ),
    Table {
        upsert: Some(
            "ON CONFLICT (scope, user_id, module, param_name) DO UPDATE SET param_value = EXCLUDED.param_value",
        ),
        ..table(
            "meta_config_params",
            &[
                ("scope", Text),
                ("user_id", Text),
                ("module", Text),
                ("param_name", Text),
                ("param_value", Text),
            ],
        )
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Copied and verified (or, in a dry run, would be).
    Copied,
    /// The target already holds exactly the source's rows; nothing written.
    AlreadyMigrated,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableReport {
    pub table: &'static str,
    pub source: usize,
    pub target: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstanceReport {
    pub instance: String,
    pub outcome: Outcome,
    pub tables: Vec<TableReport>,
    /// Atom embeddings written to the `embedding` column.
    pub embeddings_copied: usize,
    /// Embeddings left behind: embeddings are off for the target, or the
    /// stored vector has another dimension. Recall falls back to keywords
    /// for those atoms.
    pub embeddings_dropped: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub dry_run: bool,
    pub instances: Vec<InstanceReport>,
}

/// Copies every instance under `<data_dir>/instances` into `pg`. With
/// `dry_run` nothing is written to Postgres: the report shows what would be
/// copied, and a target that would refuse the copy still fails.
pub async fn migrate(data_dir: &Path, pg: &PgDb, dry_run: bool) -> DomainResult<MigrationReport> {
    let mut instances = Vec::new();
    for (instance, path) in sqlite_instances(data_dir)? {
        let source = Db::open(&path, schema::migrate_instance)?;
        instances.push(migrate_instance(&source, pg, &instance, dry_run).await?);
    }
    Ok(MigrationReport { dry_run, instances })
}

/// `(instance, memory.db)` for each instance directory, sorted by id.
fn sqlite_instances(data_dir: &Path) -> DomainResult<Vec<(InstanceId, PathBuf)>> {
    let root = data_dir.join("instances");
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(DomainError::unavailable(format!(
                "read {}: {e}",
                root.display()
            )))
        }
    };
    let mut found = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path().join("memory.db");
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if path.is_file() {
            found.push((SegmentId::parse("instance", &name)?, path));
        }
    }
    found.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    Ok(found)
}

async fn migrate_instance(
    source: &Db,
    pg: &PgDb,
    instance: &InstanceId,
    dry_run: bool,
) -> DomainResult<InstanceReport> {
    let mut tables = Vec::with_capacity(TABLES.len());
    for t in TABLES {
        let source_rows = source_count(source, t.name).await?;
        let target = target_count(pg, instance, t.name).await?;
        tables.push(TableReport {
            table: t.name,
            source: source_rows,
            target,
        });
    }
    let mut report = InstanceReport {
        instance: instance.as_str().to_owned(),
        outcome: plan(instance, &tables)?,
        tables,
        embeddings_copied: 0,
        embeddings_dropped: 0,
    };
    if report.outcome == Outcome::AlreadyMigrated || dry_run {
        if dry_run && report.outcome == Outcome::Copied {
            let (copied, dropped) = count_embeddings(source, pg.dims()).await?;
            report.embeddings_copied = copied;
            report.embeddings_dropped = dropped;
        }
        return Ok(report);
    }

    let s = pg.schema(instance).await?;
    let mut tx = pg.begin().await?;
    for t in TABLES {
        let (copied, dropped) = copy_table(source, &mut tx, &s.to_string(), t, pg.dims()).await?;
        report.embeddings_copied += copied;
        report.embeddings_dropped += dropped;
        if t.copies_seq {
            execute(
                &mut tx,
                &format!(
                    "SELECT setval(pg_get_serial_sequence(?, 'seq'), COALESCE((SELECT max(seq) FROM {s}.{}), 0) + 1, false)",
                    t.name
                ),
                &args![format!("{s}.{}", t.name)],
            )
            .await?;
        }
    }
    for row in &mut report.tables {
        row.target = count(
            &mut tx,
            &format!("SELECT count(*) FROM {s}.{}", row.table),
            &[],
        )
        .await?;
        let seeded = TABLES
            .iter()
            .any(|t| t.name == row.table && t.upsert.is_some());
        let matches = if seeded {
            row.target >= row.source
        } else {
            row.target == row.source
        };
        if !matches {
            return Err(DomainError::internal(format!(
                "instance {}: {} has {} rows in Postgres after the copy, expected {}; nothing was committed",
                instance.as_str(),
                row.table,
                row.target,
                row.source
            )));
        }
    }
    tx.commit().await.map_err(pg_err)?;
    Ok(report)
}

/// What to do with an instance, given each table's row counts.
fn plan(instance: &InstanceId, tables: &[TableReport]) -> DomainResult<Outcome> {
    let data = || {
        TABLES
            .iter()
            .zip(tables)
            .filter(|(t, _)| t.upsert.is_none())
            .map(|(_, r)| r)
    };
    if data().all(|r| r.target == 0) {
        return Ok(Outcome::Copied);
    }
    if data().all(|r| r.target == r.source) {
        return Ok(Outcome::AlreadyMigrated);
    }
    let differing: Vec<String> = data()
        .filter(|r| r.target != r.source)
        .map(|r| {
            format!(
                "{} ({} in SQLite, {} in Postgres)",
                r.table, r.source, r.target
            )
        })
        .collect();
    Err(DomainError::conflict(
        "target_not_empty",
        format!(
            "instance {}: its Postgres schema already holds other data: {}. Migrate into an empty database, or drop schema {} first.",
            instance.as_str(),
            differing.join(", "),
            schema_name(instance)
        ),
    ))
}

async fn source_count(source: &Db, table: &'static str) -> DomainResult<usize> {
    source
        .call(move |c| {
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
        })
        .await
        .map(|n| n as usize)
}

/// Rows in the instance's table, 0 when the instance has no schema yet
/// (looked up without creating it, so a dry run writes nothing).
async fn target_count(pg: &PgDb, instance: &InstanceId, table: &str) -> DomainResult<usize> {
    let mut conn = pg.conn().await?;
    let registry = quote_ident(REGISTRY_SCHEMA);
    let registered = fetch_optional(
        &mut conn,
        &format!("SELECT schema_name FROM {registry}.instances WHERE instance_id = ?"),
        &args![instance.as_str()],
    )
    .await?;
    if registered.is_none() {
        return Ok(0);
    }
    let s = quote_ident(&schema_name(instance));
    count(&mut conn, &format!("SELECT count(*) FROM {s}.{table}"), &[]).await
}

/// `(usable, unusable)` stored embeddings for a target with `dims`.
async fn count_embeddings(source: &Db, dims: Option<usize>) -> DomainResult<(usize, usize)> {
    let blobs: Vec<usize> = source
        .call(|c| {
            let mut stmt =
                c.prepare("SELECT length(embedding) FROM atoms WHERE embedding IS NOT NULL")?;
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
            rows.map(|n| n.map(|n| n as usize)).collect()
        })
        .await?;
    let usable = blobs
        .iter()
        .filter(|&&bytes| dims.is_some_and(|d| bytes == d * 4))
        .count();
    Ok((usable, blobs.len() - usable))
}

/// Copies one table page by page. Returns `(embeddings copied, dropped)`.
async fn copy_table(
    source: &Db,
    tx: &mut sqlx::PgConnection,
    schema: &str,
    t: &'static Table,
    dims: Option<usize>,
) -> DomainResult<(usize, usize)> {
    let with_vector = t.derived == Derived::AtomTokensAndEmbedding && dims.is_some();
    let mut select: Vec<&str> = t.columns.iter().map(|(c, _)| *c).collect();
    if t.derived == Derived::AtomTokensAndEmbedding {
        select.push("embedding");
    }
    let mut insert: Vec<&str> = t.columns.iter().map(|(c, _)| *c).collect();
    let mut row_values: Vec<&str> = vec!["?"; t.columns.len()];
    if t.derived != Derived::None {
        insert.push("tokens");
        row_values.push("?::tsvector");
    }
    if with_vector {
        insert.push("embedding");
        row_values.push("?");
    }
    let row_sql = format!("({})", row_values.join(","));
    let per_row = row_values.len();
    let rows_per_insert = (MAX_PARAMS / per_row).max(1);

    let (mut copied, mut dropped) = (0, 0);
    let mut after = i64::MIN;
    loop {
        let page = read_page(source, t.name, select.join(", "), after).await?;
        let Some((last, _)) = page.last() else {
            break;
        };
        after = *last;
        for chunk in page.chunks(rows_per_insert) {
            let mut params = Vec::with_capacity(chunk.len() * per_row);
            for (_, values) in chunk {
                let (c, d) = row_args(t, values, dims, with_vector, &mut params)?;
                copied += c;
                dropped += d;
            }
            let sql = format!(
                "INSERT INTO {schema}.{} ({}) VALUES {} {}",
                t.name,
                insert.join(", "),
                vec![row_sql.as_str(); chunk.len()].join(","),
                t.upsert.unwrap_or("")
            );
            execute(tx, &sql, &params).await?;
        }
    }
    Ok((copied, dropped))
}

/// Up to [`PAGE_ROWS`] rows after `after` (a rowid), as `(rowid, values)`.
async fn read_page(
    source: &Db,
    table: &'static str,
    columns: String,
    after: i64,
) -> DomainResult<Vec<(i64, Vec<Value>)>> {
    source
        .call(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT rowid, {columns} FROM {table} WHERE rowid > ? ORDER BY rowid LIMIT ?"
            ))?;
            let width = stmt.column_count();
            let rows = stmt.query_map(rusqlite::params![after, PAGE_ROWS as i64], |r| {
                let values = (1..width)
                    .map(|i| r.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((r.get::<_, i64>(0)?, values))
            })?;
            rows.collect()
        })
        .await
}

/// Appends one row's parameters. Returns `(embeddings copied, dropped)`.
fn row_args(
    t: &Table,
    values: &[Value],
    dims: Option<usize>,
    with_vector: bool,
    params: &mut Vec<Arg>,
) -> DomainResult<(usize, usize)> {
    for ((column, kind), value) in t.columns.iter().zip(values) {
        params.push(to_arg(t.name, column, *kind, value)?);
    }
    let text = |name: &str| match t.columns.iter().position(|(c, _)| *c == name) {
        Some(i) => match &values[i] {
            Value::Text(s) => s.as_str(),
            _ => "",
        },
        None => "",
    };
    match t.derived {
        Derived::None => Ok((0, 0)),
        Derived::ConversationTokens => {
            params.push(Arg::from(tsvector_literal(text("content"))));
            Ok((0, 0))
        }
        Derived::AtomTokensAndEmbedding => {
            let source = format!("{} {}", text("content"), text("background"));
            params.push(Arg::from(tsvector_literal(&source)));
            let embedding = match values.last() {
                Some(Value::Blob(b)) => Some(blob_to_embedding(b)),
                _ => None,
            };
            let usable = embedding.filter(|e| dims == Some(e.len()));
            let counts = match (&usable, values.last()) {
                (Some(_), _) => (1, 0),
                (None, Some(Value::Blob(_))) => (0, 1),
                _ => (0, 0),
            };
            if with_vector {
                params.push(Arg::Vector(usable.map(Vector::from)));
            }
            Ok(counts)
        }
        Derived::SkillTokens => {
            let head =
                t.columns.iter().zip(values).any(|((c, _), v)| {
                    *c == "is_head" && matches!(v, Value::Integer(n) if *n != 0)
                });
            let tokens = if head {
                let skill: SkillVersion = serde_json::from_str(text("doc")).map_err(|e| {
                    DomainError::internal(format!("skills.doc is not a skill version: {e}"))
                })?;
                Some(tsvector_literal(&index_source(&skill)))
            } else {
                None
            };
            params.push(Arg::Text(tokens));
            Ok((0, 0))
        }
    }
}

fn to_arg(table: &str, column: &str, kind: Kind, value: &Value) -> DomainResult<Arg> {
    Ok(match (kind, value) {
        (Text, Value::Null) => Arg::Text(None),
        (Text, Value::Text(s)) => Arg::Text(Some(s.clone())),
        (Int, Value::Null) => Arg::Int(None),
        (Int, Value::Integer(n)) => Arg::Int(Some(*n)),
        (Bool, Value::Integer(n)) => Arg::Bool(*n != 0),
        (Json, Value::Text(s)) => Arg::Json(serde_json::from_str(s).map_err(|e| {
            DomainError::internal(format!("{table}.{column} holds invalid JSON: {e}"))
        })?),
        (kind, other) => {
            return Err(DomainError::internal(format!(
                "{table}.{column}: expected {kind:?}, found {:?}",
                other.data_type()
            )))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tables the copy deliberately skips: rebuilt from the copied rows.
    fn derived(name: &str) -> bool {
        name.contains("_fts") || name == "index_meta" || name.starts_with("sqlite_")
    }

    #[test]
    fn covers_every_sqlite_table_and_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::migrate_instance(&conn).unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let mut sqlite_tables: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .filter(|n: &String| !derived(n))
            .collect();
        let mut ours: Vec<String> = TABLES.iter().map(|t| t.name.to_owned()).collect();
        sqlite_tables.sort();
        ours.sort();
        assert_eq!(ours, sqlite_tables, "TABLES must list every SQLite table");

        for t in TABLES {
            let mut stmt = conn
                .prepare(&format!("SELECT name FROM pragma_table_info('{}')", t.name))
                .unwrap();
            let columns: Vec<String> = stmt
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .filter(|c: &String| !(t.name == "atoms" && c == "embedding"))
                .collect();
            let listed: Vec<&str> = t.columns.iter().map(|(c, _)| *c).collect();
            assert_eq!(listed, columns, "columns of {}", t.name);
        }
    }

    fn report(table: &'static str, source: usize, target: usize) -> TableReport {
        TableReport {
            table,
            source,
            target,
        }
    }

    fn reports(f: impl Fn(&'static str) -> (usize, usize)) -> Vec<TableReport> {
        TABLES
            .iter()
            .map(|t| {
                let (s, d) = f(t.name);
                report(t.name, s, d)
            })
            .collect()
    }

    #[test]
    fn plans_a_copy_a_skip_or_a_refusal() {
        let instance = SegmentId::parse("instance", "default").unwrap();
        // The seeded config table never blocks a copy.
        let empty = reports(|n| (3, usize::from(n == "meta_config_params") * 6));
        assert_eq!(plan(&instance, &empty).unwrap(), Outcome::Copied);
        let done = reports(|_| (3, 3));
        assert_eq!(plan(&instance, &done).unwrap(), Outcome::AlreadyMigrated);
        let other = reports(|n| (3, if n == "atoms" { 1 } else { 0 }));
        let err = plan(&instance, &other).unwrap_err();
        assert_eq!(err.code, "target_not_empty");
        assert!(
            err.detail.contains("atoms (3 in SQLite, 1 in Postgres)"),
            "{err}"
        );
    }

    #[test]
    fn converts_sqlite_values_by_column_kind() {
        assert!(matches!(
            to_arg("t", "c", Bool, &Value::Integer(1)).unwrap(),
            Arg::Bool(true)
        ));
        assert!(matches!(
            to_arg("t", "c", Json, &Value::Text("{\"a\":1}".into())).unwrap(),
            Arg::Json(v) if v["a"] == 1
        ));
        assert!(matches!(
            to_arg("t", "c", Text, &Value::Null).unwrap(),
            Arg::Text(None)
        ));
        assert!(to_arg("t", "c", Int, &Value::Text("x".into())).is_err());
        assert!(to_arg("t", "c", Json, &Value::Text("{".into())).is_err());
    }
}
