# Moving from SQLite to PostgreSQL

[繁體中文](MIGRATE-TO-POSTGRES.zh-tw.md)

SQLite is the default storage backend: nothing to install, one file per department. As an install grows (more users, more agents, more memory) PostgreSQL becomes the better home. Its database can live on its own managed server with its own backups, and it is the storage that several memory-core replicas can share. This guide moves an existing install's data over once. You can let `./migrate-to-postgres.sh` do it ([A](#a-automatic-migrate-to-postgressh)) or run the same steps by hand ([B](#b-manual-the-same-steps-by-hand)).

## Contents

- [When to move](#when-to-move)
- [What moves and what stays](#what-moves-and-what-stays)
- [Before you start](#before-you-start)
- [A. Automatic: `migrate-to-postgres.sh`](#a-automatic-migrate-to-postgressh)
- [B. Manual: the same steps by hand](#b-manual-the-same-steps-by-hand)
- [C. Check the result](#c-check-the-result)
- [D. Roll back](#d-roll-back)
- [E. Troubleshooting](#e-troubleshooting)
- [Next: several replicas](#next-several-replicas)

## When to move

Stay on SQLite while one memory-core serves a team comfortably. Consider PostgreSQL when:

- you want to run **more than one memory-core replica** (behind a load balancer, or for availability). SQLite files can't be shared between machines.
- the data should sit in a **managed database** with point-in-time recovery, replicas and monitoring your operations team already runs.
- you want to inspect or report on memories with SQL tools.

## What moves and what stays

| | Where it ends up |
| --- | --- |
| Conversations (L0), memories (L1), scene and persona files (L2/L3) | PostgreSQL, ids, versions and timestamps unchanged |
| Users, user keys, teams, members, agents, assets, ACLs, settings | PostgreSQL, unchanged. **Clients keep working with the same keys and ids** |
| Skills (all versions), memory prompts, generation logs | PostgreSQL |
| Keyword search index | Rebuilt in PostgreSQL (`tsvector`), Chinese included |
| Vectors (when embeddings are on) | Copied into pgvector. A vector with a different dimension from the current embedding setting is left out; recall uses keywords for that memory |
| Pipeline progress (which turns were already distilled) | Stays in the coordination backend and still matches: the copy keeps every row's sequence number, so nothing is distilled twice or skipped |
| Files under memory-core's `/data` (skill resources, session objects) | Stay in the same volume; both backends use it |
| The SQLite files | **Left untouched.** They are your way back |

Every department under `/data/departments` is copied. Each one is copied in a single transaction and its row counts are compared with SQLite before it commits. If anything fails, PostgreSQL keeps nothing of that department and you can simply run the copy again.

## Before you start

- You need a shell on the server, inside the install directory (the one holding `.env`), with Docker Compose v2 and `curl`.
- Update the code to a version that has `migrate-to-postgres.sh`:

  ```bash
  cd /path/to/AgentMemory          # the directory with .env
  git pull
  ```

- **Plan for downtime.** memory-core is stopped from the backup until it restarts on PostgreSQL, usually a few minutes; the dry run in step 4 / B3 shows how many rows there are. Stop or pause the agents that write to it; clients with the `amem` queue retry later anyway.
- **Choose the target.**
  - *PostgreSQL in Docker* (the default): the script adds the `postgres` profile, which runs `pgvector/pgvector:pg16` next to memory-core with a generated password.
  - *An existing server*: PostgreSQL with the [pgvector](https://github.com/pgvector/pgvector) extension available (the bundled option and the tests use PostgreSQL 16). The user needs `CREATE` on the database. With embeddings on, the `vector` extension must exist, or the user must be allowed to create it. Use an **empty database**: the copy refuses a department whose schema already holds different data. Reach a server on the same machine as `host.docker.internal`, not `localhost`.

## A. Automatic: `migrate-to-postgres.sh`

```bash
./migrate-to-postgres.sh
```

It asks where the data should go, then:

1. **Backs up**: copies `.env` and archives memory-core's `/data` volume to `backups/pre-postgres-<time>/`, after stopping memory-core.
2. **Points `.env` at PostgreSQL**: sets `AGENT_MEMORY_POSTGRES_URL` (plus `POSTGRES_PASSWORD` and the `postgres` profile for the Docker option). The storage backend stays `sqlite` for now.
3. **Builds memory-core** from your checkout.
4. **Shows what will be copied** (a dry run that writes nothing), and asks before going on.
5. **Copies and verifies** every department.
6. **Switches** `AGENT_MEMORY_STORAGE_BACKEND` to `postgres`, starts the services and checks that `/health` reports `"backend":"postgres"`.

For automation:

```bash
MIGRATE_TARGET=postgres-docker ./migrate-to-postgres.sh --non-interactive
MIGRATE_TARGET=postgres-url AGENT_MEMORY_POSTGRES_URL=postgres://memory:…@db.internal:5432/memory \
  ./migrate-to-postgres.sh --non-interactive
```

`MIGRATE_SKIP_BACKUP=1` skips the archive of `/data` (`.env` is still saved); use it only when you back the volume up some other way.

Running it again after a successful move is harmless: it sees `AGENT_MEMORY_STORAGE_BACKEND=postgres` and stops. If it failed halfway, re-run it: departments already copied are recognised and skipped.

## B. Manual: the same steps by hand

### B1. Back up and stop

```bash
B=backups/pre-postgres-$(date +%Y%m%d-%H%M%S); mkdir -p "$B"; chmod 700 "$B"
cp .env "$B/env"
docker compose stop memory-core
docker compose run --rm --no-deps -T --entrypoint tar memory-core czf - -C /data . > "$B/memory-core-data.tgz"
```

### B2. Point `.env` at PostgreSQL

Edit `.env`. Leave `AGENT_MEMORY_STORAGE_BACKEND=sqlite` for now.

- PostgreSQL in Docker: add `postgres` to `COMPOSE_PROFILES` (comma-separated), set `POSTGRES_PASSWORD` to a long random value and `AGENT_MEMORY_POSTGRES_URL=postgres://memory:<that password>@postgres:5432/memory`.
- Existing server: set `AGENT_MEMORY_POSTGRES_URL=postgres://user:pass@host:5432/db`.

### B3. Look before copying (dry run)

```bash
docker compose build memory-core
docker compose run --rm -T memory-core migrate-storage --dry-run
```

The copy runs in a one-off memory-core container: the same image, the same `/data` volume and the same settings as the service, so the data directory and the embedding dimension match what memory-core will use. `compose run` starts the `postgres` service first when its profile is on. The output lists each department with its row counts, for example:

```text
department default: would be copied
  conversations                 18342
  atoms                          2210
  profile_files                    96
  meta_users                        4
  ...
  embeddings: 2210 kept, 0 dropped (embeddings off or another dimension)
Dry run: nothing was written.
```

Nothing is written. A target that already holds other data fails here with `target_not_empty`.

### B4. Copy

```bash
docker compose run --rm -T memory-core migrate-storage
```

Each department ends with `copied and verified`. Add `--json` to either command for a machine-readable report.

### B5. Switch and start

Set `AGENT_MEMORY_STORAGE_BACKEND=postgres` in `.env`, then:

```bash
docker compose up -d
curl -s http://127.0.0.1:${CORE_PORT:-8420}/health   # "backend":"postgres"
```

## C. Check the result

- `/health` shows `"stores": {"backend": "postgres", …}`.
- Sign in to the Panel with your usual user key: the same users, teams, agents and memories are there.
- On a client, `amem status` (or the next turn of a captured agent) succeeds without changing anything.
- Running `docker compose run --rm -T memory-core migrate-storage --dry-run` again reports every department as `already migrated, skipped`. Do this before memory-core starts writing new data, or the counts will differ.

## D. Roll back

The SQLite files were never modified. To go back:

```bash
cp backups/pre-postgres-<time>/env .env
docker compose up -d
```

Memory written **after** the switch exists only in PostgreSQL and is not carried back. If the `/data` volume itself was damaged, restore the archive first:

```bash
docker compose stop memory-core
docker compose run --rm --no-deps -T --entrypoint sh memory-core -c 'rm -rf /data/* && tar xzf - -C /data' \
  < backups/pre-postgres-<time>/memory-core-data.tgz
```

## E. Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `set AGENT_MEMORY_POSTGRES_URL …` | The URL is missing from `.env` (step B2). |
| `cannot connect to postgres at …` | Wrong host/port/password, or the server isn't reachable from the container. For a server on the same machine use `host.docker.internal`. |
| `pgvector is required when embeddings are enabled` | Install pgvector on the server, or have a superuser run `CREATE EXTENSION vector` in the target database. |
| `target_not_empty … drop schema am_… first` | The target database already holds a different copy of that department (an earlier test, or data written after a previous switch). Use an empty database, or drop the named schema and its row in `agent_memory.departments` if you are sure it is disposable. |
| `embeddings: N dropped` | Those vectors were made with another embedding dimension than `AGENT_MEMORY_EMBEDDING_DIMENSIONS` now says, or embeddings are off. The memories themselves are copied; recall finds them by keyword. |
| The copy stops with an error midway | Departments reported before the error are committed; the failing one left nothing behind. Fix the cause and run the copy again. |

## Next: several replicas

PostgreSQL makes the database shareable. Running more than one memory-core needs two more things:

- **A shared `/data` volume.** Skill resources and raw session objects are files under `/data/departments/<id>/` on every backend, so all replicas must mount the same storage (for example NFS or another network volume). See [ARCHITECTURE.md §8](ARCHITECTURE.md#8-backends-and-deployment-topologies).
- **Shared coordination** (pipeline timers, the job queue and locks).

Coordination is Redis: set `AGENT_MEMORY_COORDINATION_BACKEND=redis` and `AGENT_MEMORY_REDIS_URL` (or add the `redis` profile), as described under [Storage and Coordination Backends](../README.md#storage-and-coordination-backends) in the README. Do that as a separate step after this migration. Pipeline progress kept by the `local` backend (in `/data/system.db`) is not carried over to Redis: a session that receives new turns after that switch starts from the beginning, so its earlier turns may be distilled into memories a second time. Switching while sessions are idle (for example, over a weekend) keeps that small.
