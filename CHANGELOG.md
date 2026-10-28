# Changelog

[繁體中文](CHANGELOG.zh-tw.md)

Notable changes of each release. The [release notes](https://github.com/hsuanshao/AgentMemory/releases) have the details and upgrade steps; [UPGRADE.md](docs/UPGRADE.md) explains upgrading across versions. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/) (pre-1.0: a minor release may change data or APIs, and says so).

## [Unreleased]

## [0.5.1] - 2026-10-28

**0.5.1 replaces 0.5.0**, whose GitHub release was withdrawn. It is 0.5.0 plus the fix below. From 0.4.3, read [UPGRADE → To 0.5.1](docs/UPGRADE.md#to-051): everything about 0.5.0 applies, including schema migration 7.

### Fixed
- The memory-core image builds where outbound HTTP is blocked: its runtime stage fetches Debian packages over HTTPS instead of from `http://deb.debian.org`. `upgrade.sh` and `install.sh` build this image, so they failed on such networks.

## [0.5.0] - 2026-10-28

Its GitHub release was withdrawn the same day; use 0.5.1.

**Read [UPGRADE → To 0.5.0](docs/UPGRADE.md#to-050) before upgrading.** Redaction rules gain a team and a user level, and the Panel gets its own Redaction page and seven languages. Going back to 0.4.3 needs the backup restored (schema migration 7). Members can now read the rules for all teams and their teams' through `redaction-rule/list`.

### Added
- Redaction rules for one team or one user, on top of the rules for all teams. A team's admins manage its rules, which apply to its members' writes and its shared scenes and persona; each user manages their own, which apply to their writes in every team. Members can read the rules for all teams and their teams'. Redaction has its own page in the Panel's menu ([DISTILLATION](docs/ops/DISTILLATION.md#7-secrets-and-redaction)). Schema migration 7: going back to an earlier version needs the backup restored.
- The Panel is available in English, Traditional Chinese, Simplified Chinese, French, Vietnamese, Spanish and German. It starts in the browser's language; the menu in the sidebar and on the sign-in page changes it, and the browser remembers the choice ([Panel README](panel/README.md#languages)).
- `AGENT_MEMORY_DISTILLATION_LLM_TIMEOUT_MS` sets `llm.timeoutMs`, the timeout of each call to the distillation LLM (default 180000), without editing the config file. A self-hosted model that needs minutes for skill extraction no longer times out on every attempt ([DISTILLATION](docs/ops/DISTILLATION.md)).

### Fixed
- The worker's `last_error` (pipeline status, Panel dashboard) only shows the error of a job that was dead-lettered. A failed attempt that is retried no longer sets it, so a job that later succeeded no longer leaves an error behind that the dead-letter list does not have.

## [0.4.3] - 2026-10-24

**Read [UPGRADE → To 0.4.3](docs/UPGRADE.md#to-043) before upgrading.** This release finishes the October security review: user keys are stored as digests, refused credentials are throttled per client address and recorded in an audit log, new installs keep their ports on this machine (with an optional HTTPS profile), and the distillation model gets all its input as JSON data. Rootless Docker shows every client as one address: decide on the lockout before upgrading ([UPGRADE](docs/UPGRADE.md#rootless-docker-every-client-shows-one-address)). Going back to 0.4.2 needs the backup restored (schema migration 6).

### Security
- **User keys are stored as SHA-256 digests.** Schema migration 6 converts the stored keys in place; existing keys keep working, and the Panel shows their last four characters. A key chosen by the caller (`init-admin`, `user/create-with-key`) must be `sk-mem-` and at least 32 letters, digits, `-` or `_` ([UPGRADE](docs/UPGRADE.md#user-keys-are-stored-as-digests)).
- **Refused credentials are throttled per client address**: from the 5th in 15 minutes each refusal waits longer (250 ms, doubling up to 5 s), and the 20th locks the address out for 15 minutes (429 `too_many_failures`, `Retry-After`). `X-Forwarded-For` (every such header, ports dropped) counts only from `server.trustedProxies` (`AGENT_MEMORY_TRUSTED_PROXIES`, new; Compose trusts the Panel and Caddy). Limits: `server.throttle` or `AGENT_MEMORY_THROTTLE_{DELAY_AFTER,LOCK_AFTER,LOCK_MINUTES}` (0 turns them off, for deployments where every client shows one address, such as [rootless Docker](docs/UPGRADE.md#rootless-docker-every-client-shows-one-address)). Counts are shared through Redis when coordination is Redis ([UPGRADE](docs/UPGRADE.md#refused-keys-are-throttled-and-audited)).
- **Credential audit log**: refusals, lockouts and keys issued with the server API key (`init-admin`, `internal/upgrade`), with time, address, forwarding chain, user agent, department, path and the start of the key's digest. Kept 90 days (`AGENT_MEMORY_AUDIT_RETENTION_DAYS`), across departments (`audit.db` / `agent_memory.audit_events`). System admins see their department's on Panel → Metadata → Audit (`/v3/audit/list`); the operator sees all with `/v3/internal/audit/list`. Past 20 refusals from an address in a window, one in 100 is recorded.
- **`internal/upgrade` issues a key only with legacy data to move**, or with `AGENT_MEMORY_ALLOW_KEY_RECOVERY=true` (new) to recover a lost administrator key.
- **The distillation model gets its input as JSON data** in L1 extraction and deduplication, L2 scenes and L3 persona, as skill extraction already did: a conversation cannot end the data or pose as the instructions ([UPGRADE](docs/UPGRADE.md#the-distillation-model-gets-its-input-as-json-data)).
- **New installs keep the ports on this machine**: `install.sh` asks, defaulting to `127.0.0.1` (`BIND_ADDR`, new); `upgrade.sh` adds `BIND_ADDR=0.0.0.0` so an existing install keeps listening where it did. An optional Compose profile `tls` puts Caddy with HTTPS in front of memory-core and the Panel ([TLS](docs/ops/TLS.md)).
- Panel: the session key is kept per browser tab (sessionStorage, not localStorage), a revoked key stops working within 10 s (was 60 s), verdicts are cached by digest, and a Content-Security-Policy is sent. The Panel forwards the browser's address to Core, and relays a lockout (429) or a Core failure during the key check instead of calling the key invalid.
- `amem` and the operator commands (`memory-server generations`, `dead-letters`) do not follow redirects, so a key never goes to another address. `amem` creates its files readable by the owner only from the start, and `amem login --ca-cert` (`AMEM_CA_CERT`) trusts a private CA, such as Caddy's for an IP address. `install.sh` and `upgrade.sh` pass the server API key to curl on stdin, not on the command line.
- A recalled context `amem` cuts to the host's limit closes the blocks it opened.
- An internal or unavailable error answers with its code and the request id; the detail (paths, hosts, queries) is only in memory-core's log.
- An agent's owner who left its team can no longer change the agent or its bindings; a binding records the caller who set it (a `created_by` in the request is ignored).
- `init-admin` is serialised per department, so two calls at once create one first admin.
- User, team and asset name filters match `%` and `_` literally.

### Fixed
- `amem recall` takes every word after `--` as the query; the Hermes plugin passes it that way, so a query starting with `-` is not read as an option.
- Tasks whose skill proposals all expired undecided are closed hourly instead of being read on every list.

## [0.4.2] - 2026-10-24

**Read [UPGRADE → To 0.4.2](docs/UPGRADE.md#to-042) before upgrading.** This release tightens access: departments are created only by `init-admin`, the server API key no longer opens department routes, the distillation CLI gets an allowlisted environment, and extracted skills wait for approval. Going back to 0.4.1 needs the backup restored (schema migrations 4 and 5).

### Security
- **Skill extraction proposes; people approve.** The model gets the archived conversation as one JSON array, so text in it (web pages, tool output) cannot end the transcript or pose as instructions; it may change only the agent's skills it was shown; and what it creates or changes waits as a proposal until the conversation's user (on an agent they may use), an admin of the team, or a system admin approves it on the Panel's Skills page or through `skill/proposal/{list,approve,reject}`. Approval refuses a change to a skill that moved since. Proposals are stored redacted and expire undecided after 30 days (`AGENT_MEMORY_SKILL_PROPOSAL_TTL_DAYS`, new). `skill.extraction.requireApproval: false` (`AGENT_MEMORY_SKILL_REQUIRE_APPROVAL`, new) applies changes at once, as before ([UPGRADE](docs/UPGRADE.md#extracted-skills-wait-for-approval)).
- **Each department is administered by its own users; the server API key is for the operator only.** `meta/*`, `pipeline/*`, `stats/overview` and `memory-generation-log/*` take a user key of the department and apply that user's role; `memory-prompt/*`, `knowledge/*` and `chat-memory/clear` take a system admin's. The server API key no longer opens them; it opens only `internal/*` and `department/destroy`, plus new `internal/pipeline/dead-letter/{list,redrive}` and `internal/memory-generation-log/{list,summary}` that `memory-server dead-letters` / `generations` now use. The Panel no longer needs or sends `CORE_API_KEY` (it logs a warning when it is still set), and `PANEL_REQUIRE_USER_KEY=false` no longer gives anonymous access, since Core requires the user key. **Scripts that called these routes with the server API key alone get 401 and need a system admin's user key** ([UPGRADE](docs/UPGRADE.md#the-server-api-key-no-longer-opens-department-routes), [ARCHITECTURE](docs/ARCHITECTURE.md#three-trust-zones)).
- **The distillation CLI no longer inherits memory-core's environment.** It gets the basics (`PATH`, `HOME`, locale, certificates, proxies), the settings of the CLI in use (`ANTHROPIC_*`/`CLAUDE_*`, `OPENAI_*`/`CODEX_*`, `GEMINI_*`/`GOOGLE_*`), `llm.cli.env`, and the variables `llm.cli.passEnv` (`AGENT_MEMORY_LLM_CLI_PASS_ENV`, new) names. `AGENT_MEMORY_*` (the API key, database and Redis URLs, LLM and embedding keys) never reaches it; a `passEnv` entry matching them stops memory-core at startup ([DISTILLATION §6](docs/ops/DISTILLATION.md#what-the-cli-inherits)). `llm.cli.env` cannot set `AGENT_MEMORY_*` either. A CLI that relied on another variable of the container needs it listed, notably a CLI going through a cloud provider (Bedrock, Vertex AI, Azure) or run through a wrapper script ([UPGRADE](docs/UPGRADE.md#the-distillation-cli-no-longer-inherits-memory-cores-environment)).
- **Departments are created only by `init-admin`.** A request for a department that does not exist no longer creates its storage (a SQLite database or a Postgres schema). Data-plane and department routes answer as for a wrong user key, `meta/auth/verify` with `valid: false`, and operator routes with 404 `unknown_department` ([COLLABORATION §13](docs/COLLABORATION.md#13-departments-and-storage)). `upgrade.sh` creates the admin when an install never had one, as before. **A deployment that sends requests for a department it never initialized must run `init-admin` for it after upgrading** ([UPGRADE](docs/UPGRADE.md#departments-are-created-only-by-init-admin)). Storage opens only departments created that way, so background work cannot create one either: a pipeline job queued before `department/destroy` is now dropped instead of re-creating the department empty, `department/destroy` also removes the department's dead letters, and with several replicas a department destroyed on one is unknown to the others at once.
- **Recall output quotes what is stored.** In the persona, scene navigation and recalled memories that integrations inject, a `<` that could start a tag (even with whitespace or invisible formatting before the tag name) is written as `&lt;` (`x<1` stays as written), and each block says it is data, not instructions. New scene paths written through the API are limited to letters, digits, `-`, `_` and `.` in `/`-separated segments, and summaries to 500 characters; existing scenes keep their names and can still be updated ([ARCHITECTURE §5](docs/ARCHITECTURE.md#5-recall)).
- **A private agent is its owner's.** The memory API now applies an agent's visibility: a private agent (its memory, scenes, persona and skills, including skill lists and searches across the team) is usable by its owner, the team's admins and system admins only, and an archived agent is refused. Agents keep the visibility they were created with (`team` by default): a team with one agent per person should set each to `"visibility": "private"` with `/v3/meta/agent/update` ([COLLABORATION §9](docs/COLLABORATION.md#9-authorization-of-memory-requests)).
- **memory-core redacts secrets on every write path** before storing or distilling anything: conversations, raw session turns, the skill buffer and skills, atoms (including what the L1 model writes) and scenes and the persona. Before, only the `amem` client redacted, so text written straight to the HTTP API was stored as sent, and the model could carry a secret into L1 memories (#57).
- Redaction catches a password named in natural language with its value, in English and the major languages of Asia (Chinese, Japanese, Korean, Vietnamese, Thai, Indonesian/Malay, Hindi, Arabic) plus Spanish, Portuguese, French, German, Turkish and Russian, abbreviations included (`pw`, `pwd`, `pass`, `mk`, `비번`, `パス`, …); words with an everyday meaning, like `token` or `pass`, need `:`, `=` or "is" before the value. It also catches password-shaped strings (8+ characters mixing letters, digits and a symbol), except URLs, paths, email addresses, query strings, code operators and model references. In a raw session turn, values under a secret field name (`password`, `access_token`, …) are redacted whole, and skill files that are text are redacted like the skill. The L1 and skill prompts tell the model never to record a credential's value. `amem` uses the same rules (#57).
- `redaction.passwordShape` (`AGENT_MEMORY_REDACTION_PASSWORD_SHAPE`) turns the shape rule off; `redaction.patterns` (`AGENT_MEMORY_REDACTION_PATTERNS`, a JSON list) adds patterns such as national ids or phone numbers. A pattern that does not compile stops memory-core at startup ([DISTILLATION §7](docs/ops/DISTILLATION.md#7-secrets-and-redaction)). Data stored before the upgrade is not rewritten.

### Added
- **Department redaction rules** (#57): a department's system admins add their own patterns on top of the built-in and deployment rules, in the Panel (Metadata → Redaction) or through `/v3/meta/redaction-rule/{list,create,update,disable,enable,test}`. Writes follow a change at once (other replicas within 30 s); a rule is disabled, never deleted, and records who changed it. `test` previews a pattern on sample text before it is saved. A pattern too broad to be meant (it matches a single character, or more than 20% of everyday text) is refused, since it would erase every write. Rules leave existing `[REDACTED:…]` markers of known kinds alone, here and in `amem`; text dressed up as a marker (`[REDACTED:hunter2xyz99]`) is redacted like any other. A `redaction.patterns` kind must be 1-40 characters of a-z, 0-9 and `-`, or memory-core does not start. Schema migration 5 adds the table; going back to an earlier version afterwards means restoring the backup ([DISTILLATION §7](docs/ops/DISTILLATION.md#7-secrets-and-redaction)).
- Atoms stored without an embedding (the embedding call failed or timed out) get one later: memory-core retries every 10 minutes (`memory.embedding.backfillIntervalSecs`), and `memory-server reembed` does it now. The atom is not rewritten, so L2 does not see it as new ([DISTILLATION §8](docs/ops/DISTILLATION.md#8-embeddings-and-load), #57).
- Schema migration 4 adds an index of the atoms without an embedding, the backfill's queue. It runs when memory-core starts (or with `memory-server migrate`); going back to 0.4.1 afterwards means restoring the backup, since 0.4.1 refuses data at a newer schema version. On Postgres the index is built on each department's first use after the upgrade; with a large `atoms` table that first request waits for it ([SCHEMA-MIGRATIONS](docs/ops/SCHEMA-MIGRATIONS.md)).
- A numeric `AGENT_MEMORY_*` variable that does not parse now stops memory-core at startup, naming it, instead of being ignored; `check-config` (and so `upgrade.sh`) reports it before anything is replaced.
- `memory-server` operator commands (`reembed`, `generations`, `dead-letters`, `migrate`, `check-config`, …) log to stderr, so their stdout (`--json`) can be parsed; the server still logs to stdout.
- The embedding timeout, the embedding requests in flight (`memory.embedding.maxConcurrency`, default 4, new), the worker concurrency and the CLI model concurrency can be set by environment: `AGENT_MEMORY_EMBEDDING_TIMEOUT_MS`, `AGENT_MEMORY_EMBEDDING_MAX_CONCURRENCY`, `AGENT_MEMORY_WORKER_CONCURRENCY`, `AGENT_MEMORY_LLM_CLI_MAX_CONCURRENCY` (#57).

### Changed
- CI and the test instructions run the Postgres suites on PostgreSQL 17 (`pgvector/pgvector:pg17`).
- `amem` sends the persona and scene navigation to a Claude Code session only when they changed since the session last received them, and again after `SessionStart` (start, resume, `/clear`, fork, compaction); the matching memories still go out on every prompt. On a deployment of about 20 agents this part was two thirds of all injected context and repeated unchanged on 3 prompts out of 4. They are also repeated every 20 prompts by default, for a compaction or `/rewind` without a `SessionStart`; `AMEM_SYSTEM_CONTEXT_EVERY` (or `system_context_every` in `client.json`) sets another `N`, `0` for only on change, `1` for the previous behaviour. The Claude Code plugin registers a `SessionStart` hook for this; a hand-written `settings.json` needs `amem hook claude-code session-start` added. The hooks that write a session's state take a `flock` on a `.lock` file next to it, so they do not overwrite each other; a hook waits at most a second for it and then writes anyway. Codex and Antigravity are unchanged (#39).

### Fixed
- `/v3/session/add` stores the turn again: every authorized request got 422 `unknown field: team_id`, because the authorization layer writes the resolved scope into the body. The scope fields are now taken out before the turn is checked, and the turn is stored under its agent too (`sessions/<source>/<team>/<agent>/<user>/<session>/`, with `agent` in the record); an `agent`, `user` or `session` field sent by the client is still refused ([API](docs/API.md)).

## [0.4.1] - 2026-10-23

### Added
- memory-core refuses to start on data written by a newer version (SQLite `user_version`, Postgres `schema_migrations`, Redis `schema_version`), with an error that says to run the newer version or restore the backup. Going back to 0.4.0 after this upgrade needs the backup restored ([UPGRADE](docs/UPGRADE.md#to-041)).

### Changed
- **Scenes (L2) and personas (L3) are stored as append-only versions** (`scenes`, `scene_versions`, `persona_versions`) instead of rewritten files. An L2 update, a merge or `/v3/scenario/rm` adds a version and never deletes one. Each scene has a fixed id; a merge records the scenes it absorbed and retires them, so merged and removed scenes stay readable with `version`. The key fields (name, summary, heat, how the version came to be, author) are columns, the body stays markdown, and other attributes are kept as they are. Schema migration 3 imports `profile_files` at startup and keeps the table.
- The persona no longer gets a new version each time the scene navigation changes: the navigation is rendered from the current scenes when the persona is read.
- `/v3/scenario/write` and `/v3/core/write` record the calling user as the author of the new version. A write identical to the latest version adds nothing and returns that version, with its original author. Clearing a chat memory remains the one way scene and persona history is deleted.
- Scenes have a fixed `scene_id` in `/v3/scenario/{ls,read,write}`; `/v3/scenario/read` accepts `scene_id`, which reaches a scene whose name another one took. `/v3/core/read` with `version` returns that version's body without the Scene Navigation.
- A scene the user writes or removes through the API while L2 is running keeps the user's version: L2 drops the operations it planned on the older snapshot (an update, a merge, or a create whose name the user took) instead of overwriting the user's version or failing the batch. Those memories stay in L1.

## [0.4.0] - 2026-10-22

### Added
- memory-core turns off the built-in tools of `claude` and `codex` before they distil, with `llm.cli.hardening` (`AGENT_MEMORY_LLM_CLI_HARDENING`, default `auto`); every CLI is told in the prompt that it has no tools (#37).
- A tool call from a self-hosted OpenAI-compatible model (`tool_calls`, `function_call` or tool-call markup, with no answer) fails with the new code `LLM_TOOL_REQUEST` and is dropped without retries, like a refused CLI tool call; an answer next to one is used without the markup (#37).
- `memory-server check-config` validates the configuration without starting; `upgrade.sh` runs it with the new version before starting it; on a refusal, or any other failure before the new version answers `/health`, it restores `.env` and runs the previous version again on its previous image.
- The distillation codex runs with `--ignore-user-config` and a read-only sandbox: it no longer reads `config.toml`, so MCP servers and profiles there are not loaded. Move needed settings into `AGENT_MEMORY_LLM_CLI_ARGS` (#37).

### Changed
- **License: from 0.4.0 the server is licensed under the Business Source License 1.1** instead of MIT. Free for evaluation, personal and non-profit use, and for an organization with up to 1 department and 10 members; larger production use needs a commercial license. Each version becomes Apache 2.0 four years after release. The `amem` client (`crates/memory-client/`) and the plugins (`clients/`) stay MIT, and 0.3.2 and earlier remain MIT. See [COMMERCIAL.md](COMMERCIAL.md).
- **memory-core refuses to start when `llm.cli.args` give the distillation CLI more than its defaults, e.g. `--dangerously-skip-permissions`, `--allowedTools` or a writable codex sandbox. The error names the argument (#37).** This includes codex `--search`, `--profile` and `-c` settings that add MCP servers, web search, features or a writable sandbox. Check `AGENT_MEMORY_LLM_CLI_ARGS` before upgrading ([UPGRADE](docs/UPGRADE.md#to-040)).
- L1 extraction sees the session's 10 newest episodic memories (up to 3,000 characters). A memory that makes one of them out of date names it in `supersedes`, and that memory is revised in place instead of staying next to the news (#43).
- L2 writes scene summaries and bodies as the current state rather than a log of the batch's changes, and drops settled open questions (#43).
- A refused CLI tool call is now reported as `LLM_TOOL_REQUEST` instead of `LLM_UNAVAILABLE`; dead letters written before still count as not retryable.

### Fixed
- The Docker build could keep the previous `memory-server` binary when the sources came from an archive (git archive, a release tarball) whose file times are older than the cached build; the build now always recompiles the workspace when its sources change.

## [0.3.2] - 2026-10-21

### Added
- `amem` captures messages typed while Claude Code is still working, as part of the current turn (#40).
- Dead letters expire after a day or 3 redrives, and redrive themselves after 1 h, 2 h and 4 h; settings `worker.deadLetter*` (#41).

### Changed
- A refused tool call during distillation is no longer retried; L1/L2 drop the input at once (#41).

### Fixed
- A redrive whose queue write failed could lose the rest of its batch (#41).

## [0.3.1] - 2026-10-20

### Changed
- L1/L2 drop a batch that fails every attempt, so a session or agent is no longer blocked; the dead letter redoes exactly that input (#38).

### Fixed
- `upgrade.sh` backs up an external PostgreSQL 17+ with a matching `pg_dump` (#35).

## [0.3.0] - 2026-10-20

### Changed
- **Breaking:** "instance" is now "department": header `x-memory-department-id`, `amem` `department_id`, `.env` `CORE_DEPARTMENT_ID`, API routes. Clients and server must be updated together (#32).
- Team roles are `admin`, `leader`, `member`; pipeline state, generation logs and dead letters are filtered by role (#33).

### Added
- Versioned storage migrations at start-up and `memory-server migrate` (#32).

## [0.2.3] - 2026-10-20

### Added
- Failed and empty distillation runs are logged; dead letters keep their error and can be redriven from the CLI and the Panel (#28).
- `amem` `skip_prompt_prefixes` (#27).

### Fixed
- `migrate-to-postgres.sh` honours `AGENT_MEMORY_POSTGRES_URL` given on the command line (#25).

## [0.2.2] - 2026-10-20

### Fixed
- PostgreSQL stores messages containing U+0000 (#23).
- `amem` splits long replies and sessions to fit Core's limits (#22) and sends each message's own time (#21).

## [0.2.1] - 2026-10-20

### Fixed
- Clearer failure reasons from CLI LLMs, without leaking the distilled conversation (#17, #18, #19).

### Changed
- The project is licensed under MIT.

## [0.2.0] - 2026-10-20

### Added
- Migration from SQLite to PostgreSQL (#12).
- Prebuilt `amem` binaries for Linux and macOS, and container images on GHCR (#15).
- CI on every pull request (#14).

### Fixed
- LLM JSON repair when a model drops the closing bracket (#3).
- Panel dependency upgrades clearing all audit findings (#13).

## [0.1.1] - 2026-10-19

First public release: memory-core, the L0→L3 distillation pipeline, hybrid recall, the metadata plane, the Monitor Panel, `amem` with plugins for Claude Code, Codex, Antigravity and Hermes, and `install.sh`. Clients authenticate with per-user keys.

[Unreleased]: https://github.com/hsuanshao/AgentMemory/compare/v0.5.1...HEAD
[0.5.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.5.1
[0.5.0]: https://github.com/hsuanshao/AgentMemory/tree/87079c7
[0.4.3]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.4.3
[0.4.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.4.2
[0.4.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.4.1
[0.4.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.4.0
[0.3.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.2
[0.3.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.1
[0.3.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.0
[0.2.3]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.3
[0.2.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.2
[0.2.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.1
[0.2.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.0
[0.1.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.1.1
