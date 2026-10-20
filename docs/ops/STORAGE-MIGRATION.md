# SQLite → PostgreSQL migration: internals

[繁體中文](STORAGE-MIGRATION.zh-tw.md)

How `memory-server migrate-storage` works, and what to update when the storage schema changes. For running a migration, see [MIGRATE-TO-POSTGRES.md](../MIGRATE-TO-POSTGRES.md).

## Contents

- [1. Pieces](#1-pieces)
- [2. What a run does](#2-what-a-run-does)
- [3. Copying rows](#3-copying-rows)
- [4. Derived data](#4-derived-data)
- [5. Safety properties](#5-safety-properties)
- [6. When the schema changes](#6-when-the-schema-changes)
- [7. Known limits](#7-known-limits)

## 1. Pieces

| Piece | Role |
| --- | --- |
| `crates/memory-infrastructure/src/sqlite_to_postgres.rs` | The copy: table list, per-department transaction, verification, report |
| `crates/memory-server/src/migrate_storage.rs` | The `migrate-storage` subcommand: loads the normal server configuration, connects with the server's embedding dimension, prints the report |
| `migrate-to-postgres.sh` | Operator workflow around it: backup, `.env` changes, dry run, copy, switch, health check |
| `crates/memory-infrastructure/tests/sqlite_to_postgres.rs` | End-to-end check against a live Postgres ([TESTING.md §2](TESTING.md#2-postgres-and-redis-integration-tests)) |

The subcommand goes through `Config::load()`, so it reads exactly what the server would: `AGENT_MEMORY_DATA_DIR`, `AGENT_MEMORY_POSTGRES_URL`, the embedding settings. That is why the script runs it with `docker compose run memory-core`: same image, same `/data` volume, same environment.

## 2. What a run does

For each `<data>/departments/<id>/memory.db`, sorted by department id:

1. **Count** the rows of every table in SQLite, and in the department's Postgres schema. The target is looked up in the registry (`agent_memory.departments`) without creating it, so a dry run writes nothing.
2. **Plan** (`plan()`):
   - every target table empty → copy;
   - every target table equal to the source → `already_migrated`, skip;
   - anything else → error `target_not_empty`, naming the tables that differ. Nothing is overwritten or merged.

   `meta_config_params` is left out of this decision: the Postgres DDL seeds it with defaults.
3. **Copy** (unless dry run): create the schema through `PgDb::schema()` (the same path the server uses, embedding column included), then copy every table inside **one transaction**.
4. **Advance sequences** for tables whose `seq` was copied verbatim.
5. **Verify**: count every table again inside the transaction; any mismatch returns an error, and the transaction rolls back.
6. **Commit.**

Departments are independent: one that fails leaves earlier ones committed. A re-run skips those (step 2) and retries the rest.

## 3. Copying rows

The table list is the `TABLES` constant: every table of `sqlite/schema.rs` with its columns, each marked as one of

| Kind | SQLite | Postgres |
| --- | --- | --- |
| `Text` | `TEXT` / `NULL` | `TEXT` |
| `Int` | `INTEGER` / `NULL` | `BIGINT` |
| `Bool` | `INTEGER` 0/1 | `BOOLEAN` |
| `Json` | `TEXT` holding JSON | `JSONB` |

A value of the wrong type is an error, not a silent conversion.

Rows are read in `rowid` order, 1,000 at a time, and inserted with multi-row `INSERT`s kept under 30,000 bound parameters (Postgres allows 65,535).

**Sequence numbers are kept.** `conversations.seq` and `atoms.seq` are copied as they are, and afterwards each `BIGSERIAL` sequence is set past the copied maximum (`setval(pg_get_serial_sequence(…), max(seq) + 1, false)`). This matters because the pipeline's cursors (`l1_cursor` in the session state over `conversations.seq`, `l2_cursor` in the profile state over `atoms.seq`) are these numbers. Renumbering would make the pipeline re-distil or skip turns. `generation_refs.seq` exists only in Postgres; those rows get new numbers in `rowid` order.

`meta_config_params` uses `ON CONFLICT … DO UPDATE`, so a value changed in SQLite replaces the seeded default.

Not copied: the FTS5 tables (`*_fts`), `index_meta`, and the sqlite-vec index. They are derived (next section).

## 4. Derived data

Postgres keeps keyword tokens and vectors in the rows themselves. The copy computes them **with the same functions the Postgres adapters use when they write**, so a migrated row is indistinguishable from one written by the server:

| Table | Column | Computed from | Same as |
| --- | --- | --- | --- |
| `conversations` | `tokens` | `tsvector_literal(content)` | `PostgresMemoryStore::append` |
| `atoms` | `tokens` | `tsvector_literal(content + " " + background)` | `PostgresMemoryStore::upsert` |
| `atoms` | `embedding` | the SQLite `embedding` BLOB (little-endian `f32`) | `upsert` with an embedding |
| `skills` | `tokens` | head versions only: `tsvector_literal(index_source(doc))`; older versions `NULL` | `PostgresSkillStore::append_version` |

An embedding is copied only when the target has an `embedding` column (embeddings configured) and the stored vector has exactly that dimension. Otherwise it is counted in `embeddings_dropped` and left out; the atom itself is copied.

## 5. Safety properties

- **No source row is written.** The SQLite databases are opened with the normal setup, which may create a missing table (`CREATE … IF NOT EXISTS`), and are otherwise only read.
- **All or nothing per department**, enforced by the transaction and the count check.
- **Never overwrites or merges** a non-empty target (step 2).
- **Repeatable**: a re-run finishes what is missing and skips what is done.
- **The source must be quiet.** Pages are read in separate SQLite read transactions, so a server writing during the copy could produce an inconsistent snapshot; the count check would then usually fail. That is why the operator flow stops memory-core first. The subcommand itself cannot tell whether a server is running.

## 6. When the schema changes

The SQLite and Postgres DDL mirror each other table for table (`sqlite/schema.rs`, `postgres/schema.rs`). A change to existing data also needs a versioned migration ([SCHEMA-MIGRATIONS.md](SCHEMA-MIGRATIONS.md)). A schema change touches this copy too:

| Change | Also update |
| --- | --- |
| New table | Add it to `TABLES`, unless it is derived data |
| New, renamed or removed column | Its entry in `TABLES` (name and kind, in DDL order) |
| New derived column in Postgres (tokens, vectors, …) | A `Derived` case computing it with the adapter's own function |
| Different token text in an adapter | The matching `Derived` case. Prefer sharing the function, as `index_source` is shared |
| New column with a `BIGSERIAL` copied from SQLite | `copies_seq: true` |
| Postgres DDL seeds a new table | `upsert` with its conflict target |

The unit test `covers_every_sqlite_table_and_column` fails when `TABLES` no longer lists every SQLite table and column in order, so the first two rows can't be forgotten silently. The rest are caught by the live-Postgres test (search, vectors, versions, sequences): extend it when you add a derived column.

## 7. Known limits

- **Pipeline state is not part of the copy.** It belongs to the coordination backend. With `local` coordination it stays in `/data/system.db`, and the cursors still match because sequence numbers are kept. Switching coordination from `local` to Redis does not carry that state over: a session that receives new turns afterwards starts from its first turn, so earlier turns may be distilled again. A future `migrate-coordination` could copy `session_states` and `profile_states` into Redis.
- **One direction only.** There is no Postgres → SQLite copy. The way back is the untouched SQLite files plus the saved `.env`, and it loses what was written after the switch.
- **Memory use** is bounded by one page (1,000 rows) per table, but each department is a single transaction. A very large department means a long transaction on the target.
