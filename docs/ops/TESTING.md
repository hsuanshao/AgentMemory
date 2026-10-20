# Testing

[繁體中文](TESTING.zh-tw.md)

Run the layers that match what you changed; before a release, run all of them. CI runs sections 1 to 4 on every pull request ([RELEASE.md §5](RELEASE.md#5-automation)); section 5 and the screenshot comparison stay manual.

## Contents

- [1. Rust: format, lint, tests](#1-rust-format-lint-tests)
- [2. Postgres and Redis integration tests](#2-postgres-and-redis-integration-tests)
- [3. Panel](#3-panel)
- [4. Shell scripts](#4-shell-scripts)
- [5. Full-stack smoke test](#5-full-stack-smoke-test)
- [6. Rules for test code](#6-rules-for-test-code)

## 1. Rust: format, lint, tests

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

- Formatting uses the rustfmt defaults; there is no `rustfmt.toml`.
- Clippy must be clean with `-D warnings`. A new toolchain can add lints, so run it on a current stable.
- The workspace declares `rust-version = "1.88"` and every crate inherits it. A newer local toolchain will happily compile code that 1.88 rejects; check with `cargo +1.88 check --workspace --locked` when you use a recent language or library feature. `.cargo/config.toml` makes `cargo update` choose dependency versions that still support 1.88; CI's msrv job catches the rest.
- `cargo test` covers unit tests and the end-to-end tests in `crates/memory-server/tests/e2e.rs`, which start a real server on SQLite with local coordination. The Postgres and Redis tests in the same run print `skipping: …` and pass unless you set them up (next section).

## 2. Postgres and Redis integration tests

These run only when their URL is set, so a plain `cargo test` does **not** exercise PostgreSQL or Redis. Any change to `crates/memory-infrastructure/src/postgres/`, `src/redis/`, `src/sqlite_to_postgres.rs` or SQL in general needs this run:

```bash
docker run -d --rm --name amem-pg -e POSTGRES_PASSWORD=test -p 25432:5432 pgvector/pgvector:pg16
docker run -d --rm --name amem-redis -p 26379:6379 redis:8.4.0
until docker exec amem-pg pg_isready -U postgres -q; do sleep 1; done

AGENT_MEMORY_TEST_POSTGRES_URL=postgres://postgres:test@127.0.0.1:25432/postgres \
AGENT_MEMORY_TEST_REDIS_URL=redis://127.0.0.1:26379 \
  cargo test --workspace --locked

docker stop amem-pg amem-redis
```

| Test file | Needs | Covers |
| --- | --- | --- |
| `memory-infrastructure/tests/postgres_stores.rs` | Postgres | Every Postgres adapter against the same expectations as SQLite (ordering, CJK search, isolation, keys) |
| `memory-infrastructure/tests/sqlite_to_postgres.rs` | Postgres | Data written through the SQLite adapters is read back through the Postgres ones after `migrate`: search, vectors, versions, keys, sequences, re-runs |
| `memory-server/tests/e2e.rs` (`the_pipeline_runs_on_the_postgres_backend`, `legacy_default_data_migrates_on_postgres`, `redis_coordination_runs_the_pipeline`) | Postgres / Redis | The whole server on those backends, including the legacy-scope migration |

Each test uses a randomly named department and drops it afterwards, so the same database can be reused. A failing test may leave its schema behind (`am_<hash>`); drop the container to start clean.

## 3. Panel

```bash
cd panel
npm ci
npm run typecheck
npm test
npm run build
```

Node 22 or newer (`engines` in `panel/package.json`). `npm ci` also refuses a `package-lock.json` that is out of sync with the `package.json` files.

### Styling changes: compare screenshots

Type checks and unit tests don't see CSS. When a change can alter how the Panel looks (Tailwind, PostCSS, Vite, shared components), compare the old and new builds:

1. Build the base branch and copy `panel/web/dist` aside; then build the change.
2. Start one memory-core with seeded data (see [§5](#5-full-stack-smoke-test) for the API calls), then two Panel servers on it, one per build: `PORT=… CORE_URL=… CORE_API_KEY=… UI_DIST_DIR=<absolute path to a dist> node server/dist/index.js`. `UI_DIST_DIR` must be absolute: `npm start -w server` resolves it from `panel/server`.
3. With Playwright, log in on both, screenshot every route (`#/dashboard`, `#/memory`, `#/logs`, `#/skills`, `#/meta`, plus the login page) in light and dark mode (`colorScheme`), and diff each pair with `pixelmatch`.
4. Live values (dashboard clock, uptime) always differ. For anything else, compare `getComputedStyle` of the element on both builds before deciding it's a real change: text rasterisation can move pixels with identical styles.
5. Also check what screenshots can't show: `cursor` on buttons, `::placeholder` colour, focus rings.

If Playwright can't download a browser for the host (it refuses unknown distributions), pass an existing Chromium or headless shell as `executablePath`.

## 4. Shell scripts

```bash
docker run --rm -v "$PWD:/mnt" -w /mnt koalaman/shellcheck:v0.10.0 -x \
  install.sh upgrade.sh uninstall.sh migrate-to-postgres.sh
```

`upgrade.sh` and `migrate-to-postgres.sh` `source ./install.sh` for their helpers, hence `-x`. CI runs the same command (plus `.github/scripts/*.sh`) and fails on any finding, so every script must stay clean.

The scripts' behaviour is only tested by running them (next section).

## 5. Full-stack smoke test

`install.sh`, `upgrade.sh` and `migrate-to-postgres.sh` drive Docker Compose, build images and edit `.env`. Unit tests can't cover that, so test them on a throwaway stack. Three things keep it from touching an install already running on the same machine:

- **A separate directory.** Compose names containers and volumes after the directory (`<dir>_memory-core-data`, …).
- **Other ports** via `CORE_PORT` / `PANEL_PORT`.
- **Another image tag** via `MEMORY_IMAGE_TAG`. Without it the build replaces `memory-core:local`, which a running install may use.

```bash
# 1. a copy of the working tree (tracked files only)
S=$(mktemp -d)/stack && mkdir -p "$S"
git ls-files -z | xargs -0 -I{} cp --parents {} "$S/"
cd "$S"

# 2. install on SQLite, non-interactively
CORE_PORT=28520 PANEL_PORT=28525 MEMORY_IMAGE_TAG=smoke \
INSTALL_STORAGE=sqlite INSTALL_COORDINATION=local INSTALL_LLM=openai INSTALL_ADMIN_NAME=admin \
  ./install.sh --non-interactive          # prints the admin user key

# 3. seed data through the API (team, agent, a few conversations), e.g.
API=$(grep '^AGENT_MEMORY_API_KEY=' .env | cut -d= -f2)
H=(-H "Authorization: Bearer $API" -H "x-memory-department-id: default"
   -H "x-memory-user-key: <admin key>" -H "content-type: application/json")
curl -s -X POST localhost:28520/v3/internal/meta/user/list-by-department "${H[@]}" -d '{}'   # the admin's user_id
curl -s -X POST localhost:28520/v3/meta/team/create  "${H[@]}" -d '{"name":"platform","owner_user_id":"usr-…"}'
curl -s -X POST localhost:28520/v3/meta/agent/create "${H[@]}" -d '{"team_id":"team-…","owner_user_id":"usr-…","name":"codex"}'
curl -s -X POST localhost:28520/v3/conversation/add  "${H[@]}" \
  -d '{"team_id":"team-…","agent_id":"agt-…","session_id":"s1","messages":[{"role":"user","content":"我們下週一發布第二版"}]}'

# 4. exercise the script under test, then check the result through the API
MIGRATE_TARGET=postgres-docker ./migrate-to-postgres.sh --non-interactive
curl -s localhost:28520/health                      # "backend":"postgres"
curl -s -X POST localhost:28520/v3/conversation/query "${H[@]}" -d '{"team_id":"team-…","agent_id":"agt-…"}'

# 5. also try the re-run and the documented rollback
./migrate-to-postgres.sh --non-interactive          # "already stores its data in PostgreSQL"
cp backups/pre-postgres-*/env .env && docker compose up -d

# 6. tear down everything this stack created
docker compose --profile postgres --profile redis down -v --remove-orphans
docker image rm memory-core:smoke memory-panel:smoke
cd / && rm -rf "$S"
```

The first build takes several minutes: the core image installs the bundled LLM CLIs. To try only `memory-server` itself, run the binary from `target/debug` with `AGENT_MEMORY_DATA_DIR`, `AGENT_MEMORY_PORT` and the storage variables set; that takes seconds.

## 6. Rules for test code

- **No secret-shaped literals.** Secret scanners (GitGuardian, GitHub push protection) flag anything that looks like a real token, even in tests. Build such values at runtime from a prefix and low-entropy filler, as `crates/memory-client/src/redact.rs` does with `fake("sk-mem-", 'x', 32)`. Never paste a key printed by a test server into a test.
- **Tests that need a service skip, never fail, when it is not configured**, and say so with `eprintln!("skipping: …")`. CI then decides which services to provide.
- **Read back through the other side.** For storage code, write with one adapter and assert with the other (or through the HTTP API), rather than only counting rows.
