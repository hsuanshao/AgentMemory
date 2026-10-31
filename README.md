<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./docs/imgs/svg/agentmemory-lockup-dark-transparent.svg">
    <img src="./docs/imgs/svg/agentmemory-lockup-light-transparent.svg" alt="AgentMemory" width="420">
  </picture>
</h1>

[![CI](https://img.shields.io/github/actions/workflow/status/hsuanshao/TeamAgentMemory/ci.yml?branch=main&label=CI)](https://github.com/hsuanshao/TeamAgentMemory/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/hsuanshao/TeamAgentMemory)](https://github.com/hsuanshao/TeamAgentMemory/releases/latest)
[![License: BUSL-1.1](https://img.shields.io/badge/license-BUSL--1.1-blue)](COMMERCIAL.md)
[![MSRV](https://img.shields.io/badge/MSRV-1.88-orange?logo=rust)](Cargo.toml)
[![Docker image](https://img.shields.io/badge/ghcr.io-agentmemory--core-2496ED?logo=docker&logoColor=white)](https://github.com/hsuanshao/TeamAgentMemory/pkgs/container/agentmemory-core)
[![Conventional Commits](https://img.shields.io/badge/Conventional%20Commits-1.0.0-FE5196?logo=conventionalcommits&logoColor=white)](https://www.conventionalcommits.org)
[![Last commit](https://img.shields.io/github/last-commit/hsuanshao/TeamAgentMemory)](https://github.com/hsuanshao/TeamAgentMemory/commits/main)
<!-- ALL-CONTRIBUTORS-BADGE:START - Do not remove or modify this section -->
[![All Contributors](https://img.shields.io/github/all-contributors/hsuanshao/TeamAgentMemory?color=ee8449)](#contributors)
<!-- ALL-CONTRIBUTORS-BADGE:END -->

English | [繁體中文](./README.zh-tw.md)

**Self-hosted long-term memory for teams that work with AI agents.** AgentMemory gives Claude Code, Codex, Antigravity, Hermes and any MCP client a memory that persists across sessions. It captures each conversation, distils it into facts, scenes and a persona in the background, and recalls what is relevant before every prompt. It all runs on your own servers.

## Built for Team Collaboration

Most agent memory is personal: each person's assistant learns on its own, and what one teammate's agent worked out yesterday is invisible to the next person who asks. AgentMemory starts from the team instead. The agents a team works with build up a shared understanding of the team's work (its projects, conventions, decisions and recurring problems), while each person's own conversations stay their own.

```mermaid
flowchart LR
  subgraph private["Private to each member"]
    A["alice: conversations → facts"]
    B["bob: conversations → facts"]
  end
  subgraph shared["Shared by everyone who uses the agent"]
    S["Scenes (L2)"] --> P["Persona (L3)"]
  end
  A --> S
  B --> S
  S -- recall --> R["every member's next prompt"]
  P -- recall --> R
```

- **Shared where it helps, private where it matters.** Each member's conversations (L0) and facts (L1) are scoped to that member. The scenes (L2) and persona (L3) of an agent are distilled from the facts of everyone who uses it, so recall gives each member the team's accumulated context, never a teammate's transcript.
- **Skills spread across the team.** A procedure extracted from one member's work becomes a skill every member of the team can find, once a person approves it. Skills of a private agent stay with that agent.
- **The team chooses how much to share.** Put everyone on one agent to pool what the team learns, or give each person a private agent to keep profiles apart while still managing them as one team.
- **Hard boundaries.** Nothing crosses teams or agents, and departments share nothing at all: not users, keys or data. One authorization layer enforces this before every memory call, so it does not depend on clients behaving.
- **Oversight without surveillance.** Team leaders see whether distillation is healthy for every member, but cannot open anyone's conversations or facts.

| You want | Set it up as |
| --- | --- |
| A team that pools what it learns | Every member uses the same agent, e.g. `team-assistant` |
| One team, separate profiles | One private agent per person (`"visibility": "private"`) |
| Groups that must not share anything | Separate teams, or separate departments |

Because a shared agent's scenes and persona mix everyone's facts, use one only when members are comfortable with that. [COLLABORATION.md](docs/COLLABORATION.md) covers the levels, roles, layouts and how isolation is enforced.

## Why AgentMemory

- **Your data stays with you.** Memory, database and even the distillation model can all run inside your network. There is no dependency on any vendor's cloud service: any OpenAI-compatible endpoint works, including self-hosted models, as does a local CLI such as `claude`, `codex` or `agy`.
- **Built for teams, not just one user.** A server holds departments, a department holds teams, and a team holds members and their agents. Personal memory stays private; what an agent learns is shared by the people who use it. See [Built for Team Collaboration](#built-for-team-collaboration).
- **Memory that compounds.** Raw turns (L0) are distilled into facts (L1), consolidated into scenes (L2) and summarised into a persona (L3). Recall combines BM25 and vector search with rank fusion. See [How memory works](#how-memory-works).
- **Built to be operated.**
  - A Monitor Panel with distillation health, generation logs and dead letters that expire and retry on their own.
  - SQLite to start, PostgreSQL with pgvector and Redis to scale.
  - Versioned storage migrations, and an `upgrade.sh` that backs up first.
- **One client for every agent.** The `amem` CLI plugs into Claude Code, Codex, Antigravity and Hermes hooks, and serves recall over MCP. It redacts secrets before anything leaves the machine and queues while the server is unreachable.

Written in Rust (memory-core, `amem`) and TypeScript (Panel), following Clean Architecture: `domain ← application ← infrastructure / gateway ← server`.

## Quick Start

```bash
git clone https://github.com/hsuanshao/TeamAgentMemory.git && cd TeamAgentMemory
./install.sh
```

The installer asks, in order:

1. **Storage backend**: SQLite (default, nothing to install) / existing PostgreSQL (enter a URL; the connection is tested from inside the container first) / start PostgreSQL (pgvector) for me with Docker
2. **Coordination backend**: single node (default) / existing Redis / start Redis for me with Docker
3. **LLM**: OpenAI-compatible API / Claude Code CLI / Antigravity CLI / Codex CLI
4. Project directory for the CLI and the exposed ports
5. **Who reaches it**: this machine only (default, ports on `127.0.0.1`) / other machines over plain HTTP / other machines over HTTPS with Caddy ([TLS](docs/ops/TLS.md))

> [!IMPORTANT]
> **Rootless Docker:** memory-core sees every client as one address, so 20 wrong keys from anyone in 15 minutes lock everyone out for 15 minutes. Either set `AGENT_MEMORY_THROTTLE_LOCK_AFTER=0` in `.env` (delays and the audit log stay), or put a reverse proxy on the host that sees the real address and list it in `AGENT_MEMORY_TRUSTED_PROXIES` ([UPGRADE](docs/UPGRADE.md#rootless-docker-every-client-shows-one-address)).

It then generates a Gateway API key and database password, writes `.env` with mode 600 (including `COMPOSE_PROFILES`), starts the services and waits for the health checks, and finally creates the first admin and shows a one-time login user key.

- Automated deployment: `INSTALL_STORAGE=postgres-docker INSTALL_COORDINATION=redis-docker ./install.sh --non-interactive` (see `./install.sh --help` for the variables).
- To restart later, just run `docker compose up -d`; `.env` carries the same profiles and settings.
- For a database on the host machine, use `host.docker.internal` instead of `localhost`.

| Service | Address | Dockerfile | Started when |
| --- | --- | --- | --- |
| memory-core | http://localhost:8420 | `docker/core.Dockerfile` | Always |
| memory-panel | http://localhost:8125 | `panel/Dockerfile` | Always |
| postgres (pgvector/pgvector:pg16) | Container network only | — | profile `postgres` |
| redis (redis:8.4.0) | Container network only | — | profile `redis` |

Manual installation (without `install.sh`):

```bash
cp .env.example .env        # set at least AGENT_MEMORY_API_KEY
docker compose up -d --build
curl -s -X POST http://localhost:8420/v3/internal/meta/user/init-admin \
  -H "Authorization: Bearer $AGENT_MEMORY_API_KEY" \
  -H "x-memory-department-id: default" -H "content-type: application/json" \
  -d '{"username":"admin"}'
```

Or start each service separately:

```bash
docker build -f docker/core.Dockerfile -t memory-core:local .
docker run --rm -p 8420:8420 -e AGENT_MEMORY_API_KEY=change-me \
  -e AGENT_MEMORY_LLM_API_KEY=sk-... -v memory-core-data:/data memory-core:local

docker build -t memory-panel:local panel
docker run --rm -p 8125:8125 -e CORE_URL=http://host.docker.internal:8420 memory-panel:local
```

## Users, Teams and Access

Sign in to the Panel with the admin user key. Under **Metadata**, a system admin creates users (each gets a user key, shown once), creates teams, assigns users to teams with a role, and creates agents. Everyone else gets a read-only Panel.

Clients (agents on any machine) authenticate with their **user key only**; they never need `AGENT_MEMORY_API_KEY`. Every memory call names a `team_id` and an `agent_id` the user belongs to, and members only read and write their own conversations and facts; the scenes and persona of an agent are shared by the people who use it. [docs/COLLABORATION.md](docs/COLLABORATION.md) explains departments, teams, users and agents, from choosing a layout to how isolation is enforced. A department exists once `init-admin` has run for it; requests that name any other department are refused. See [docs/API.md](docs/API.md) for the exact rules and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how it fits together.

**Server and agents on different machines?** Follow [docs/REMOTE-CLIENTS.md](docs/REMOTE-CLIENTS.md). It covers TLS, issuing keys, installing the `amem` client and the Claude Code, Codex, Antigravity and Hermes integrations, and the client side of an upgrade.

**Upgrading from an earlier version?** Run `./upgrade.sh` in your install directory, after updating the code. [docs/UPGRADE.md](docs/UPGRADE.md) describes the same steps by hand, plus how to roll back.

**Outgrowing SQLite?** `./migrate-to-postgres.sh` moves an existing install to PostgreSQL: users, keys, memories and skills come along, and clients need no change. [docs/MIGRATE-TO-POSTGRES.md](docs/MIGRATE-TO-POSTGRES.md) covers when to move, the manual steps and how to go back.

**Removing it:** `./uninstall.sh client` removes the client from a machine that runs agents: the Claude Code, Codex, Antigravity and Hermes integrations plus `amem`. On the server, `./uninstall.sh server` removes the containers and images and keeps the data; add `--purge-data` to delete the data as well. Add `--dry-run` to either command to see what would be removed first. Details and the manual steps are in [docs/UNINSTALL.md](docs/UNINSTALL.md).

## Local Development

```bash
cargo test --workspace                 # unit + integration + end-to-end tests (Postgres/Redis tests skipped by default)
# Run integration tests against real Postgres and Redis:
docker run -d --rm --name amem-pg -e POSTGRES_PASSWORD=test -p 25432:5432 pgvector/pgvector:pg17
docker run -d --rm --name amem-redis -p 26379:6379 redis:8.4.0
AGENT_MEMORY_TEST_POSTGRES_URL=postgres://postgres:test@127.0.0.1:25432/postgres \
AGENT_MEMORY_TEST_REDIS_URL=redis://127.0.0.1:26379 cargo test --workspace
AGENT_MEMORY_LLM_API_KEY=sk-... cargo run -p memory-server
cd panel && npm install && npm run dev # Panel (Vite + Hono)
```

## How Memory Works

```text
/v3/conversation/add ──► L0 conversation (SQLite + FTS5)
        │  warm-up 1→2→4→every N turns, or idle 600s
        ▼
   L1 extraction (LLM: context segmentation + persona/episodic/instruction, priority threshold)
   └─ dedup: hybrid-recall candidates → LLM decides store / update / merge / skip
        │  delay 90s and ≥ 900s since last L2; then a sweep every 3600s
        ▼
   L2 scenes (shared per team+agent; UPDATE > MERGE > CREATE, heat, max 15, META header)
   └─ records which facts each scene holds; a scene whose facts were rewritten, deleted or moved
      is marked stale, and the next L2 run is told what changed (even with no new facts)
        │  cold start / every 50 new memories / when a scene requests an update
        ▼
   L3 Persona (persona.md + auto-appended Scene Navigation)
```

Recall (`/v3/recall`): strip previously injected blocks → L1 hybrid retrieval (BM25 + vector + RRF k=60, falls back to BM25 without embeddings) → thresholds, count and character budgets → output `<relevant-memories>` plus `<user-persona>` and `<scene-navigation>`.

Chinese full-text search: text is tokenized into "overlapping CJK bigrams + Latin words" before going to FTS5 — no dictionary needed, and results are predictable.

## Project Structure

```text
crates/
├── memory-domain/          Entities, value objects, pure policies (isolation, RRF, ACL, Skill, scene format, trigger rules)
├── memory-application/     Use cases and ports (memory, recall, pipeline, metadata, Skill)
├── memory-infrastructure/  SQLite / filesystem / OpenAI client / background workers
├── memory-gateway/         axum v3 HTTP API, envelopes, validation, CORS
├── memory-server/          Configuration (memory-core.yaml compatible) and wiring (composition root)
└── memory-client/          `amem` client: recall/capture hooks, redaction, offline queue, MCP server
clients/                    Claude Code, Codex and Antigravity plugins, Hermes memory provider
panel/                      TypeScript Monitor Panel (Hono BFF + React)
docker/core.Dockerfile      Core image (bundles the claude and agy CLIs)
install.sh                  Interactive installer
upgrade.sh                  Upgrade an existing install
uninstall.sh                Remove the client or the server
docs/ARCHITECTURE.md        Architecture overview (mermaid diagrams)
docs/API.md                 API reference and design trade-offs
docs/REMOTE-CLIENTS.md      Server/client split deployment guide
docs/UPGRADE.md             Upgrading an existing install (upgrade.sh, manual steps, rollback)
docs/UNINSTALL.md           Removing the client or the server (uninstall.sh, manual steps)
docs/COLLABORATION.md       Departments, teams, users and agents: layouts, setup, isolation internals
docs/MIGRATE-TO-POSTGRES.md Moving an install from SQLite to PostgreSQL (script, manual steps, rollback)
docs/ops/                   For maintainers: testing, releases, storage-migration internals
```

## Configuration

Config file keys (`server`, `data`, `llm`, `memory.pipeline`, `memory.recall`, `memory.embedding`, `skill`, …); see `config/memory-core.example.yaml` for an example. Common environment variables:

| Variable | Default | Description |
| --- | --- | --- |
| `AGENT_MEMORY_CONFIG` | Auto-discover `memory-core.yaml` | Config file path |
| `AGENT_MEMORY_HOST` / `AGENT_MEMORY_PORT` | `127.0.0.1` / `8420` | Bind address; an API key is required for non-loopback addresses |
| `AGENT_MEMORY_API_KEY` | Unset | Server API key (Bearer) for the operator routes only: `internal/*` (init-admin, upgrades) and `department/destroy`. Clients, department admins and the Panel use user keys. Treat it as a root credential ([ARCHITECTURE](docs/ARCHITECTURE.md#three-trust-zones)) |
| `AGENT_MEMORY_STORAGE_BACKEND` / `AGENT_MEMORY_POSTGRES_URL` | `sqlite` / unset | Storage backend (see "Storage and Coordination Backends") |
| `AGENT_MEMORY_COORDINATION_BACKEND` / `AGENT_MEMORY_REDIS_URL` | `local` / unset | Coordination backend |
| `AGENT_MEMORY_DATA_DIR` | `~/.agent-memory` | Data directory (one SQLite per department) |
| `AGENT_MEMORY_LLM_PROVIDER` | `openai` | `openai` (HTTP API) or `cli` (local tool) |
| `AGENT_MEMORY_LLM_BASE_URL` / `AGENT_MEMORY_LLM_API_KEY` / `AGENT_MEMORY_LLM_MODEL` | OpenAI | Used when provider=openai |
| `AGENT_MEMORY_LLM_CLI_*` | Unset | Used when provider=cli; see "Using a Local CLI as the LLM" |
| `AGENT_MEMORY_EMBEDDING_PROVIDER` etc. | `none` | Enables vector retrieval |
| `AGENT_MEMORY_CORS_ORIGINS` | Empty | Allowed origins (comma-separated) |
| `AGENT_MEMORY_TRUSTED_PROXIES` | Unset (Compose: `memory-panel,caddy`) | Proxies whose `X-Forwarded-For` names the client; refused keys are counted and locked out per client address ([UPGRADE](docs/UPGRADE.md#refused-keys-are-throttled-and-audited)) |
| `AGENT_MEMORY_THROTTLE_DELAY_AFTER` / `_LOCK_AFTER` / `_LOCK_MINUTES` | `5` / `20` / `15` | Refused credentials per client address: delays from this refusal, a lockout at this one, its length; 0 turns delays or the lockout off (rootless Docker: [UPGRADE](docs/UPGRADE.md#rootless-docker-every-client-shows-one-address)) |
| `AGENT_MEMORY_AUDIT_RETENTION_DAYS` | `90` | Days the credential audit log keeps an event |
| `AGENT_MEMORY_ALLOW_KEY_RECOVERY` | `false` | Lets `internal/upgrade` issue an admin key without legacy data to move |
| `BIND_ADDR` (Compose) | `127.0.0.1` from `install.sh` | Where ports 8420 and 8125 listen; `0.0.0.0` for every network |

## Storage and Coordination Backends

| Setting | Option | Description |
| --- | --- | --- |
| `storage.backend` (`AGENT_MEMORY_STORAGE_BACKEND`) | `sqlite` (default) | One SQLite file per department; full-text search via FTS5, vectors via **sqlite-vec** (`vec0` KNN with isolation filters applied inside the index; falls back to brute-force comparison if the extension can't load) |
| | `postgres` | One schema per department (`am_<hash>`); full-text search via tsvector + GIN, vectors via **pgvector HNSW** (exact search when dimensions > 2000) |
| `coordination.backend` (`AGENT_MEMORY_COORDINATION_BACKEND`) | `local` (default) | Pipeline state in `system.db`, queues and locks in-process; single node |
| | `redis` | Pipeline state, job queue and distributed locks all in Redis; multiple memory-core replicas can run concurrently |

### PostgreSQL

- Connection: `AGENT_MEMORY_POSTGRES_URL=postgres://user:pass@host:5432/db`.
- Moving an existing SQLite install: `./migrate-to-postgres.sh`, see [docs/MIGRATE-TO-POSTGRES.md](docs/MIGRATE-TO-POSTGRES.md).
- Permissions: requires CREATE privilege on the database (to create the `agent_memory` and `am_*` schemas). With embeddings enabled, the `vector` extension must be created: run as superuser, or have a DBA run `CREATE EXTENSION vector` beforehand.
- Differences from SQLite: vector search is approximate (HNSW); changing the embedding dimension clears existing vectors; text must not contain NUL characters.

### Redis

- Connection: `AGENT_MEMORY_REDIS_URL=redis://[:password@]host:6379/0`; all keys are prefixed with `coordination.redis.keyPrefix` (default `agent_memory`).
- The queue is **at-least-once**: dequeued jobs carry a lease (`coordination.redis.queueLeaseMs`, default 15 minutes) that is extended automatically while running; if a node crashes, the job is redelivered after the lease expires. In rare cases a job may run twice, which the pipeline is designed to tolerate.
- Locks are acquired with `SET NX PX` and renewed while held (`lockTtlMs`, default 10 minutes).
- Redis Cluster is not supported (scripts build keys dynamically); for high availability use a single primary with replicas / Sentinel.
- The dead-letter list of failed jobs keeps the latest 10,000 entries.

### sqlite-vec

- The index is only created when embeddings are configured (`memory.embedding.provider` is not `none`); `stores.vectorIndex` in `/health` reports `sqlite-vec`, `brute-force`, `pgvector-hnsw`, `pgvector-exact` or `none`.
- A single KNN query returns at most 4096 rows; each team partition takes roughly a 1024-vector chunk of space (about 6 MB at 1536 dimensions).

## Using a Local CLI as the LLM

Besides an OpenAI-compatible API (API key), memory-core can call a local command-line tool to generate memories, with a specified project directory as the tool's working directory (so the tool can read that project's settings and context):

```yaml
llm:
  provider: cli
  timeoutMs: 180000
  cli:
    command: claude              # or ollama, or any tool that reads stdin and prints a text answer
    args: ["-p"]                 # ollama example: ["run", "llama3.1"]
    promptMode: stdin            # use arg for tools that only take arguments, and put {prompt} in args
    workingDir: ~/projects/my-app
    maxConcurrency: 2
```

Equivalent environment variables: `AGENT_MEMORY_LLM_PROVIDER=cli`, `AGENT_MEMORY_LLM_CLI_COMMAND`, `AGENT_MEMORY_LLM_CLI_ARGS` (JSON array or whitespace-separated), `AGENT_MEMORY_LLM_CLI_PROMPT_MODE`, `AGENT_MEMORY_LLM_CLI_WORKDIR`, `AGENT_MEMORY_LLM_CLI_PASS_ENV`.

- No shell is involved: the child process is spawned directly with an argument array; the prompt goes through stdin by default and never shows up in the process list.
- The tool does not inherit memory-core's environment: it gets `PATH`, `HOME`, locale, certificate and proxy variables, its own settings (`ANTHROPIC_*`/`CLAUDE_*` for claude, `OPENAI_*`/`CODEX_*` for codex, `GEMINI_*`/`GOOGLE_*` for agy), `llm.cli.env`, and what `llm.cli.passEnv` names; never `AGENT_MEMORY_*` ([DISTILLATION §6](docs/ops/DISTILLATION.md#what-the-cli-inherits)).
- Each call is bounded by `timeoutMs`, and the child process is killed on timeout; a non-zero exit code or empty output is treated as the LLM being unavailable and left to the pipeline to retry.
- Refer to each tool's own documentation for its flags; the tool must print the complete answer to stdout.

### CLIs Bundled in the Docker Image

By default `docker/core.Dockerfile` installs three CLIs with their official native install scripts (no npm; checksums verified), and runs them as a non-root service user:

| CLI | Command | Recommended settings | Login |
| --- | --- | --- | --- |
| Claude Code | `claude` | `ARGS=-p`, `PROMPT_MODE=stdin` | Set `ANTHROPIC_API_KEY`, or run `docker compose exec -it memory-core claude` and enter `/login` |
| Antigravity CLI | `agy` | `ARGS='["-p","{prompt}"]'`, `PROMPT_MODE=arg` (`agy -p` doesn't read stdin) | Run `docker compose exec -it memory-core agy` and open the displayed Google sign-in URL in a browser |
| Codex CLI | `codex` | `ARGS='["exec","--skip-git-repo-check","--ephemeral","-s","read-only","-c","features.hooks=false","-"]'`, `PROMPT_MODE=stdin` | Run `docker compose exec -it memory-core codex login --device-auth` and follow the printed steps, or pipe an API key into `codex login --with-api-key` |

- Login state is stored in named volumes (`claude-config` → `~/.claude`, `antigravity-config` → `~/.gemini`, `codex-config` → `~/.codex`), so it survives container rebuilds.
- Project directory: set the host path with `PROJECT_DIR`; it is mounted at `/workspace` in the container and used as the CLI's working directory.
- When not logged in, the error shows up in the Panel's worker metrics (e.g. `Not logged in · Please run /login`), and the pipeline retries.
- `agy` receives the prompt as an argument, so it is subject to Linux's 128 KiB single-argument limit; prompts beyond that fail with an error.
- `agy` requires a CPU with AES-NI (in VMs, set the CPU type to host).
- The Codex arguments keep it read-only, leave no session file, and turn hooks off, so a memory client installed for Codex never captures the distillation prompts themselves.
- Skip CLIs you don't need at build time: `--build-arg INSTALL_CLAUDE=false`, `--build-arg INSTALL_ANTIGRAVITY=false`, `--build-arg INSTALL_CODEX=false`; pin the Claude Code version with `--build-arg CLAUDE_VERSION=latest` or a specific version number.
- As a result the image is about 940 MB; if you only use the HTTP API, skip the CLIs with the arguments above.

## Design Trade-offs

See [docs/API.md](docs/API.md) for details. Highlights:

- L2/L3 and Skill extraction use structured JSON operations (rather than an agentic loop with tools), giving a fixed output contract that is easy to test.
- Scope isolation is always applied to recall and search; pipeline cursors use database sequence numbers, so messages with identical timestamps or backfilled messages are never skipped.
- Not provided: analytics reports, external task import, upstream model-group routing, `/v3/skill/export`.

## Contributing

Issues and pull requests are welcome. See [CONTRIBUTING.md](.github/CONTRIBUTING.md) for the development setup, checks and conventions, the [Code of Conduct](.github/CODE_OF_CONDUCT.md), and [SECURITY.md](.github/SECURITY.md) to report a vulnerability privately.

### Contributors

Thanks to everyone who has contributed ([emoji key](https://allcontributors.org/docs/en/emoji-key)). To credit someone, comment on their issue or pull request: `@all-contributors please add <github-user> for code, doc`, and the bot opens a pull request that updates this list.

<!-- ALL-CONTRIBUTORS-LIST:START - Do not remove or modify this section -->
<!-- prettier-ignore-start -->
<!-- markdownlint-disable -->
<table>
  <tbody>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://www.linkedin.com/in/hsuan-shao-chang-59412894/"><img src="https://avatars.githubusercontent.com/u/5172404?v=4?s=100" width="100px;" alt="William Chang"/><br /><sub><b>William Chang</b></sub></a><br /><a href="https://github.com/hsuanshao/TeamAgentMemory/commits?author=hsuanshao" title="Code">💻</a> <a href="https://github.com/hsuanshao/TeamAgentMemory/commits?author=hsuanshao" title="Documentation">📖</a> <a href="#maintenance-hsuanshao" title="Maintenance">🚧</a> <a href="#infra-hsuanshao" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a> <a href="#security-hsuanshao" title="Security">🛡️</a> <a href="https://github.com/hsuanshao/TeamAgentMemory/commits?author=hsuanshao" title="Tests">⚠️</a> <a href="https://github.com/hsuanshao/TeamAgentMemory/pulls?q=is%3Apr+reviewed-by%3Ahsuanshao" title="Reviewed Pull Requests">👀</a> <a href="#projectManagement-hsuanshao" title="Project Management">📆</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://blog.seancheng.space"><img src="https://avatars.githubusercontent.com/u/29519438?v=4?s=100" width="100px;" alt="Sean Zheng"/><br /><sub><b>Sean Zheng</b></sub></a><br /><a href="https://github.com/hsuanshao/TeamAgentMemory/commits?author=blackhorseya" title="Code">💻</a> <a href="https://github.com/hsuanshao/TeamAgentMemory/commits?author=blackhorseya" title="Tests">⚠️</a> <a href="https://github.com/hsuanshao/TeamAgentMemory/pulls?q=is%3Apr+reviewed-by%3Ablackhorseya" title="Reviewed Pull Requests">👀</a> <a href="https://github.com/hsuanshao/TeamAgentMemory/issues?q=author%3Ablackhorseya" title="Bug reports">🐛</a> <a href="#ideas-blackhorseya" title="Ideas, Planning, & Feedback">🤔</a></td>
    </tr>
  </tbody>
</table>

<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->

<!-- ALL-CONTRIBUTORS-LIST:END -->

## License

From 0.4.0 the server is licensed under the [Business Source License 1.1](LICENSE): free for evaluation, personal and non-profit use, and for an organization with up to 1 department and 10 members; larger production use needs a commercial license. Each version becomes Apache 2.0 four years after release. The `amem` client and the agent plugins stay MIT, and 0.3.2 and earlier remain MIT. See [COMMERCIAL.md](COMMERCIAL.md).

## Acknowledgements

The layered memory and Skill design draws on ideas from the open-source project TencentDB Agent Memory (MIT); this project is an independent implementation, contains none of its code, and does not connect to any Tencent services.
