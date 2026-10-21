# Contributing to AgentMemory

[繁體中文](CONTRIBUTING.zh-tw.md)

Thanks for helping. AgentMemory is a self-hosted memory service for agents that work in teams. One server holds departments, a department holds teams, and a team holds members and their agents ([COLLABORATION.md](../docs/COLLABORATION.md)). Contributions of every size are welcome: a typo, a bug report, a fix, a feature.

By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Contents

- [Reporting bugs and asking for features](#reporting-bugs-and-asking-for-features)
- [Before you write code](#before-you-write-code)
- [Development setup](#development-setup)
- [Checks before a pull request](#checks-before-a-pull-request)
- [Conventions](#conventions)
- [Pull requests](#pull-requests)
- [Security](#security)

## Reporting bugs and asking for features

Use the issue templates. For a bug, include:
- the version (`/health` → `version`, and `amem --version` on the client);
- the storage and coordination backends;
- the LLM provider;
- the steps to reproduce.

Remove keys and personal data from logs before you paste them.

Report a security problem privately instead, as described in [SECURITY.md](SECURITY.md).

## Before you write code

For anything beyond a small fix, open an issue first and describe the problem and the change you have in mind. This avoids work on something that does not fit, such as a change of scope or a new dependency.

## Development setup

- **Rust:** stable. The workspace's minimum is Rust 1.88 (`rust-version`), and CI checks it.
- **Node.js 22:** for the Panel (`panel/`).
- **Docker:** to run the Postgres and Redis integration tests and the full stack.

The layout and the design are described in [ARCHITECTURE.md](../docs/ARCHITECTURE.md). Maintainer notes (testing, releases, migrations) are in [docs/ops/](../docs/ops/README.md).

## Checks before a pull request

CI runs all of these. Run the ones that match what you changed ([TESTING.md](../docs/ops/TESTING.md) has the details):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

- **Postgres or Redis code**, or any SQL: also run the integration tests against real servers ([TESTING.md §2](../docs/ops/TESTING.md#2-postgres-and-redis-integration-tests)). Without their URLs those tests are skipped and pass.
- **Panel:** `cd panel && npm ci && npm run typecheck && npm test && npm run build`.
- **Shell scripts:** shellcheck v0.10.0 ([TESTING.md §4](../docs/ops/TESTING.md#4-shell-scripts)).
- **`install.sh`, `upgrade.sh`, `migrate-to-postgres.sh` or the Compose file:** run the full-stack smoke test ([TESTING.md §5](../docs/ops/TESTING.md#5-full-stack-smoke-test)).

## Conventions

- **Commit and PR titles** follow [Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `ci:`, `chore:`, with an optional scope (`feat(amem): …`). PRs are squash-merged, so the PR title becomes the commit on `main`.
- **Documentation is bilingual.** Every user-facing document has an English version and a Traditional Chinese one (`*.zh-tw.md`). Update both in the same PR.
- **Stored data changes go through versioned migrations.** Never just edit the `CREATE … IF NOT EXISTS` DDL; add a migration step, as described in [SCHEMA-MIGRATIONS.md](../docs/ops/SCHEMA-MIGRATIONS.md).
- **Renamed settings fail loudly.** A renamed setting, header or API field must be rejected with its new name; it must not be silently ignored.
- **Terminology:** department, team, member, agent. The project does not use "tenant" or "multi-tenancy".
- **Distillation never gets tools.** The distillation LLM reads untrusted conversation content. Do not give it tool or command permissions, and do not add flags that skip permission checks (#37).
- **Tests build secrets at runtime.** No real or realistic-looking keys in the repository; CI scans for them.

## Pull requests

- Keep a PR to one change. Describe the why, the change, and how you tested it; the PR template lists what to cover.
- CI must pass. A maintainer reviews, and may ask for changes, including to the docs.
- Releases are cut by maintainers ([RELEASE.md](../docs/ops/RELEASE.md)). You don't need to bump versions.

Contributions are released under the license of the directory they change: the [Business Source License 1.1](../LICENSE) for the server, and the MIT License for `crates/memory-client/` and `clients/`. The maintainer may also offer the server under commercial licenses ([COMMERCIAL.md](../COMMERCIAL.md)); by contributing you agree that your contribution may be included in them.

## Security

See [SECURITY.md](SECURITY.md). Please don't open public issues for vulnerabilities.
