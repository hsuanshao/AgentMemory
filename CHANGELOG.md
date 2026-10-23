# Changelog

[繁體中文](CHANGELOG.zh-tw.md)

Notable changes of each release. The [release notes](https://github.com/hsuanshao/AgentMemory/releases) have the details and upgrade steps; [UPGRADE.md](docs/UPGRADE.md) explains upgrading across versions. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/) (pre-1.0: a minor release may change data or APIs, and says so).

## [Unreleased]

### Changed
- `amem` sends the persona and scene navigation to a Claude Code session only when they changed since the session last received them, and again after `SessionStart` (start, resume, `/clear`, fork, compaction); the matching memories still go out on every prompt. On a deployment of about 20 agents this part was two thirds of all injected context and repeated unchanged on 3 prompts out of 4. They are also repeated every 20 prompts by default, for a compaction or `/rewind` without a `SessionStart`; `AMEM_SYSTEM_CONTEXT_EVERY` (or `system_context_every` in `client.json`) sets another `N`, `0` for only on change, `1` for the previous behaviour. The Claude Code plugin registers a `SessionStart` hook for this; a hand-written `settings.json` needs `amem hook claude-code session-start` added. Codex and Antigravity are unchanged (#39).

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

[Unreleased]: https://github.com/hsuanshao/AgentMemory/compare/v0.4.1...HEAD
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
