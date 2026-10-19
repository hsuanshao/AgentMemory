# Releases

[繁體中文](RELEASE.zh-tw.md)

## Contents

- [1. Version numbers](#1-version-numbers)
- [2. Where the version lives](#2-where-the-version-lives)
- [3. Release checklist](#3-release-checklist)
- [4. Release notes](#4-release-notes)
- [5. Automation (planned)](#5-automation-planned)

## 1. Version numbers

Releases are tagged `vMAJOR.MINOR.PATCH` on `main` (`v0.1.0`, `v0.1.1`). There is no written rule yet for which part to bump; note that `v0.1.1` already changed client authentication and needed an upgrade step, so a version number alone doesn't tell operators whether they must act. The release notes and the UPGRADE guide do.

Every component carries the release number. `memory-server` reports it in `/health` (`version`), `amem --version` prints it and the `amem` MCP server announces it, all from the Cargo workspace version; the Panel and the plugins have their own manifests that must say the same (section 2).

## 2. Where the version lives

| File | Field | Notes |
| --- | --- | --- |
| `Cargo.toml` | `[workspace.package] version` | All crates inherit it (`/health`, `amem --version`). Run `cargo build` afterwards so `Cargo.lock` follows |
| `clients/claude-code/.claude-plugin/plugin.json` | `version` | |
| `clients/codex/.codex-plugin/plugin.json` | `version` | |
| `panel/package.json`, `panel/server/package.json`, `panel/web/package.json` | `version` | Then `cd panel && npm install --package-lock-only` so `package-lock.json` follows |
| `docs/UPGRADE*.md`, `docs/REMOTE-CLIENTS*.md` | `git checkout vX`, `cargo install … --tag vX` | Only where the docs pin a release on purpose |

Not tied to the release number:

- `clients/hermes/agentmemory/plugin.yaml` has its own version (currently `2.0.0`).
- `clients/antigravity/plugin.json` and `.claude-plugin/marketplace.json` have no version field.
- Text such as "memories written before 0.1.1" in `upgrade.sh` and the code describes the release that changed the data model, and stays as it is.

To find what to change: `git grep -n '<old version>' -- ':!Cargo.lock' ':!*.md'`, then read each hit.

## 3. Release checklist

1. **Branch** from an up-to-date `main` (`git pull` first; contributors merge PRs on GitHub).
2. **Bump** every file in [section 2](#2-where-the-version-lives).
3. **Run every test layer** in [TESTING.md](TESTING.md): format, clippy, the full suite with Postgres and Redis, the Panel, shellcheck. When the release changes `install.sh`, `upgrade.sh`, `migrate-to-postgres.sh` or the Compose file, also run the full-stack smoke test.
4. **Check the docs** that a release can affect: README (both languages), the UPGRADE guide when operators must act, API.md for changed endpoints. Every user-facing document exists in English and Traditional Chinese, so change both.
5. **Scan for secrets and private details** before anything is pushed: `git grep -nE 'sk-(ant|proj|mem)-[A-Za-z0-9_-]{16}|ghp_[A-Za-z0-9]{20}|BEGIN [A-Z ]*PRIVATE KEY'` should find nothing, and neither should a search for internal host names or home-directory paths.
6. **Open a PR**, let it be reviewed and merged.
7. **Tag** the merge commit on `main` and push the tag:

   ```bash
   git switch main && git pull
   git tag -a vX.Y.Z -m vX.Y.Z
   git push origin vX.Y.Z
   ```

8. **Publish the GitHub release** for the tag with the release notes (next section).

## 4. Release notes

Suggested shape:

- Lead with what an operator must do, if anything (upgrade steps, client changes), then new features, then fixes.
- Link the guides rather than repeating them.
- Credit contributors by their GitHub handle and PR number.

## 5. Automation (planned)

There is no `.github/workflows` yet. The plan:

**`ci.yml`**, on every pull request and push to `main`:

| Job | Runs |
| --- | --- |
| fmt | `cargo fmt --all -- --check` |
| clippy | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| test | `cargo test --workspace --locked` (SQLite) |
| test-services | The same with `pgvector/pgvector:pg16` and `redis:8.4.0` service containers and the `AGENT_MEMORY_TEST_*_URL` variables |
| msrv | `cargo check --workspace --locked` on Rust 1.88 |
| panel | Node 22: `npm ci`, typecheck, test, build |
| scripts | shellcheck on every script |
| consistency | Every file in [section 2](#2-where-the-version-lives) carries the same version; the secret scan from step 5 |
| docker | Build both images without pushing (core with `INSTALL_CLAUDE/ANTIGRAVITY/CODEX=false` to stay fast) |

**`release.yml`**, on a pushed `v*.*.*` tag:

1. Check that the tag equals the Cargo workspace version, then run the CI jobs.
2. Build `amem` for Linux x86_64/arm64 (musl) and macOS arm64/x86_64, with a `SHA256SUMS` file, so client machines no longer need a Rust toolchain.
3. Build multi-arch (amd64/arm64) images and push them to the GitHub Container Registry as `agentmemory-core` and `agentmemory-panel`, tagged with the version, `MAJOR.MINOR` and `latest`.
4. Create a **draft** GitHub release with those files, for the maintainer to add the notes and publish.

Follow-ups that come with it: Compose and the install scripts pull the published images first (keeping `build:` for local changes), and REMOTE-CLIENTS documents downloading `amem`.

Before the shellcheck job can be required, the findings listed in [TESTING.md §4](TESTING.md#4-shell-scripts) need fixing.
