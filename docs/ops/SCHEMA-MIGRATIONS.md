# Versioned schema migrations

[繁體中文](SCHEMA-MIGRATIONS.zh-tw.md)

A release that changes what is stored brings that data up to date itself. memory-core migrates at start-up, so an operator only updates the code and restarts; `upgrade.sh` handles the host side (backup, `.env`). This page explains how migrations are recorded and how to add one.

## Contents

- [1. Where the version lives](#1-where-the-version-lives)
- [2. When migrations run](#2-when-migrations-run)
- [3. Migrations so far](#3-migrations-so-far)
- [4. Adding a migration](#4-adding-a-migration)

## 1. Where the version lives

Every store keeps its own version, so each one is migrated by the code that owns it:

| Store | Version kept in | Code |
| --- | --- | --- |
| Data directory | none; the layout itself (`<data>/instances` vs `<data>/departments`) | `fs::migrate_data_dir` |
| SQLite department `memory.db` | `PRAGMA user_version` | `sqlite/schema.rs` `migrate_department` |
| SQLite `system.db` (local coordination) | `PRAGMA user_version` | `sqlite/schema.rs` `migrate_system` |
| Postgres | `agent_memory.schema_migrations` (one row per step: version, name, time) | `postgres/schema.rs` `MIGRATIONS`, `postgres/db.rs` `migrate` |
| Redis | `{prefix}:schema_version` | `redis/migrate.rs` |

The DDL in `sqlite/schema.rs` and `postgres/schema.rs` always describes the **latest** version. A migration step only turns older data into that shape. A new install runs the DDL and records the latest version without running any step.

## 2. When migrations run

```mermaid
flowchart LR
  S["memory-core starts<br/>or memory-server migrate"] --> D["data directory"]
  D --> ST["storage backend"]
  ST --> C["coordination backend"]
  C --> R["serve requests"]
```

- **At start-up**, before anything is served. A failed step stops the boot with the error, and the version stays at the last completed step.
- **`memory-server migrate`** runs the same steps and exits. It also opens every SQLite department, which the server otherwise migrates on first use. Run it with `docker compose run --rm -T memory-core migrate` to see the result before the service is back.
- **Several replicas.**
  - Postgres steps run under the bootstrap advisory lock.
  - Redis steps run under `{prefix}:lock:migrate`, and the other replicas wait until `schema_version` is reached.
  - SQLite is single-node by design.
- **Old replicas must be stopped first.** A replica of an earlier version cannot read migrated data. The upgrade guide says so for every release with a migration.
- **One way only.** There are no down migrations. Going back means restoring the backup `upgrade.sh` takes (see [UPGRADE.md](../UPGRADE.md)).
- **Newer data is refused.** A store whose version is above the one the code knows (`user_version`, `max(schema_migrations.version)`, `schema_version`) stops memory-core with an error, and nothing is migrated or rewritten. Without this, an older SQLite build would reset `user_version` and a later upgrade would replay migrations over data written in between. Versions up to 0.4.0 do not have the check.

## 3. Migrations so far

| Version | Release | What changes |
| --- | --- | --- |
| 1 | 0.3.0 | "instance" becomes "department":<br/>• `<data>/instances` → `<data>/departments`<br/>• `system.db` column `instance` → `department`, and the same field inside each document<br/>• Postgres `agent_memory.instances(instance_id)` → `agent_memory.departments(department_id)`<br/>• prompt targets `instance` → `department`<br/>• quota `max_*_per_instance` → `max_*_per_department` (keeping the operator's values)<br/>• Redis `{p}:pipe:inst:*` → `{p}:pipe:department:*`, and the `instance` field of state documents, queued jobs and dead letters |
| 2 | 0.3.0 | The team role `reviewer` is removed: such members become `member` (SQLite and Postgres; Redis has no step 2) |
| 3 | 0.4.1 | Scenes and personas become append-only versions: every row of `profile_files` is imported into `scenes` / `scene_versions` / `persona_versions` (version numbers kept, scene ids derived from team, agent and path, persona versions that only refreshed the navigation skipped). `profile_files` is kept but no longer written (SQLite and Postgres; Redis has no step 3) |
| 4 | 0.4.2 | A partial index of the atoms without an embedding, `ix_atoms_missing_embedding`, the embedding backfill's queue. SQLite creates it with the department tables; Postgres with the embedding column, so only when embeddings are on, on each department's first use after the upgrade. That build is a plain `CREATE INDEX` in a transaction: on a large `atoms` table the first request to the department waits for it, and writes to it are held meanwhile (Redis has no step 4) |
| 5 | 0.4.2 | Administrators' redaction rules: the table `meta_redaction_rules` in each department (SQLite and Postgres; Redis has no step 5). Nothing is converted, but the version keeps an older memory-server off the data: it would store what those rules hide |
| 6 | 0.4.3 | User keys are stored as SHA-256 digests: `key_value` becomes `sha256:<hex>` and the document gains `key_hint` (the last four characters), skipping rows already converted. Postgres converts every department when memory-core starts; SQLite each department's database when it is first opened. Postgres also gets the credential audit log, `agent_memory.audit_events`, in the registry (SQLite keeps it in `<data>/audit.db`, outside the versioned databases; Redis has no step 6). Existing keys keep working; an older memory-server could not check them |
| 7 | 0.5.0 | Redaction rules for one team or one user (`team_id`, `user_id` in `meta_redaction_rules.doc`). Nothing is converted, but an older memory-server must not run on the data: it would apply those rules to every team and drop their level when it saved one (SQLite and Postgres; Redis has no step 7) |
| 8 | 0.5.2 | Which atoms each scene was built from (`scene_atoms`: an atom is current in at most one scene, with a partial unique index `ux_scene_atoms_current`) and why a scene may be out of date (`scene_stale`), both in each department and removed with their scene. Nothing is converted: scenes written before have no recorded atoms until L2 places atoms in them again. An older memory-server must not run on the data: its atom writes and deletes would leave scenes unmarked (SQLite and Postgres; Redis has no step 8) |

## 4. Adding a migration

1. Change the DDL to the new shape.
2. Add a step for each store whose data changes:
   - **SQLite**: raise `SCHEMA_VERSION` and add an `if user_version(conn)? < N` block. Put it before the DDL batch if the DDL would fail on the old shape (an index on a renamed column), after it if the step needs the new tables.
   - **Postgres**: append `(N, "what changed")` to `MIGRATIONS` and call the step from `migrate` when `applied < N`. The row is recorded for you.
   - **Redis**: raise `SCHEMA_VERSION` in `redis/migrate.rs` and add an `if from < N` step.
3. **Make each step safe to repeat.** Check before changing anything (`has_column`, `to_regclass`, the key exists), because a step can be interrupted after part of its work.
4. **Test from the old shape.** Write data with the current code, turn it back into the old shape, migrate twice, then read it with the normal adapters. See `sqlite/schema.rs` tests and `tests/storage_migrations.rs` (Postgres and Redis; run them as described in [TESTING.md §2](TESTING.md#2-postgres-and-redis-integration-tests)).
5. **Add a row to §3 in the same PR**, in both languages, with `next` as its release; the release PR replaces it with the version number. CI's [`check-consistency.sh`](../../.github/scripts/check-consistency.sh) fails when the last row of either language is not the version in the code, and a release fails while a row still says `next`.
6. **Add an upgrade section** to [UPGRADE.md](../UPGRADE.md) (and UPGRADE.zh-tw.md) when operators must do anything besides restarting, and always when the step leaves data that the previous release refuses. Its warning names that release: "**Going back to 0.5.1 means restoring the backup**", not "going back to an older version". The operator then knows which backup to keep: the one `upgrade.sh` took before this release.
7. If a setting in `memory-core.yaml` is renamed, add it to `RENAMED_KEYS` in `memory-server/src/config.rs`. If a `.env` setting is renamed, add it to `RENAMED_SETTINGS` in `upgrade.sh`.
