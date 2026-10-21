# Changelog

[繁體中文](CHANGELOG.zh-tw.md)

Notable changes of each release. The [release notes](https://github.com/hsuanshao/AgentMemory/releases) have the details and upgrade steps; [UPGRADE.md](docs/UPGRADE.md) explains upgrading across versions. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/) (pre-1.0: a minor release may change data or APIs, and says so).

## [Unreleased]

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

[Unreleased]: https://github.com/hsuanshao/AgentMemory/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.2
[0.3.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.1
[0.3.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.0
[0.2.3]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.3
[0.2.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.2
[0.2.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.1
[0.2.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.0
[0.1.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.1.1
