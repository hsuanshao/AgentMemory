# Releases

[繁體中文](RELEASE.zh-tw.md)

## Contents

- [1. Version numbers](#1-version-numbers)
- [2. Where the version lives](#2-where-the-version-lives)
- [3. Release checklist](#3-release-checklist)
- [4. Release notes](#4-release-notes)
- [5. Automation](#5-automation)

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
4. **Check the docs** that a release can affect: `CHANGELOG.md` and `CHANGELOG.zh-tw.md` (move *Unreleased* under the new version), README (both languages), the UPGRADE guide when operators must act, API.md for changed endpoints. Every user-facing document exists in English and Traditional Chinese, so change both. Two of them follow the code and are checked by `check-consistency.sh X.Y.Z` (step 5), which names what is left:
   - **[SCHEMA-MIGRATIONS](SCHEMA-MIGRATIONS.md) §3**: the last row is the highest version in the code (`SCHEMA_VERSION` in `sqlite/schema.rs`, the last entry of `MIGRATIONS` in `postgres/schema.rs`, `SCHEMA_VERSION` in `redis/migrate.rs` where Redis has a step), and every row that says `next` gets this version.
   - **UPGRADE**: the section for the next release gets its version number, in its heading, the contents and links to it. When this release adds a migration, its warning names the release being left: "Going back to X.Y.Z means restoring the backup".
5. **Scan for secrets and private details** before anything is pushed: `.github/scripts/check-consistency.sh X.Y.Z` does steps 2 and 5 together. By hand, `git grep -nE 'sk-(ant|proj|mem)-[A-Za-z0-9_-]{16}|ghp_[A-Za-z0-9]{20}|BEGIN [A-Z ]*PRIVATE KEY'` should find nothing, and neither should a search for internal host names or home-directory paths.
6. **Open a PR**, let it be reviewed and merged.
7. **Tag** the merge commit on `main` and push the tag:

   ```bash
   git switch main && git pull
   git tag -a vX.Y.Z -m vX.Y.Z
   git push origin vX.Y.Z
   ```

8. **Publish the GitHub release.** The tag starts `release.yml` ([section 5](#releases)), which builds everything and creates a draft release; add the release notes (next section) and publish it.

## 4. Release notes

Suggested shape:

- Lead with what an operator must do, if anything (upgrade steps, client changes), then new features, then fixes.
- Link the guides rather than repeating them.
- Credit contributors by their GitHub handle and PR number.

## 5. Automation

### CI

[`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) runs on every pull request and every push to `main`:

| Job | Runs |
| --- | --- |
| fmt | `cargo fmt --all -- --check` |
| clippy | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| test | `cargo test --workspace --locked` with `pgvector/pgvector:pg17` and `redis:8.4.0` service containers and the `AGENT_MEMORY_TEST_*_URL` variables, so the SQLite, Postgres and Redis tests all run |
| msrv | `cargo check --workspace --all-targets --locked` on Rust 1.88 |
| panel | Node 22: `npm ci`, typecheck, test, build |
| scripts | shellcheck v0.10.0 on every script; the Hermes plugin compiles |
| consistency | [`.github/scripts/check-consistency.sh`](../../.github/scripts/check-consistency.sh): every file in [section 2](#2-where-the-version-lives) carries the same version, the list of schema migrations ends at the version in the code, and the secret and private-path scan from step 5 |
| docker | Build both images without pushing (core with `INSTALL_CLAUDE/ANTIGRAVITY/CODEX=false` to stay fast) |

Run the consistency check locally before a release; with a version argument it also checks that every file says that version, and that no migration in SCHEMA-MIGRATIONS and no UPGRADE section is still waiting for its release number: `.github/scripts/check-consistency.sh 0.2.0`.

When `rust-version` in `Cargo.toml` changes, change the toolchain of the msrv job with it.

### Releases

[`.github/workflows/release.yml`](../../.github/workflows/release.yml) runs when a `vX.Y.Z` tag is pushed (step 7 of the checklist):

| Job | Does |
| --- | --- |
| version | `check-consistency.sh X.Y.Z`: the tag matches every file in [section 2](#2-where-the-version-lives) |
| ci | All of `ci.yml` |
| amem | Builds `amem` for `linux-x86_64`, `linux-arm64` (static, musl), `macos-arm64` and `macos-x86_64` as `amem-X.Y.Z-<platform>.tar.gz` |
| image | Builds `agentmemory-core` (with the bundled LLM CLIs) and `agentmemory-panel` for amd64 and arm64, each on a native runner. Each image is started first: core must answer `/health` with the release version and run its bundled CLIs, the Panel must answer `/health`. Only then is it pushed by digest |
| manifest | Joins both architectures on `ghcr.io/<owner>/agentmemory-core` and `-panel` as `X.Y.Z`, `X.Y` and `latest` |
| release | A **draft** GitHub release with the archives and `SHA256SUMS` |

Afterwards:

1. Open the draft release, write the notes (section 4), and choose whether it is a pre-release. Then publish it.
2. The first time only: GHCR makes new packages private. Open each package (`agentmemory-core`, `agentmemory-panel`) under the repository's Packages, then Package settings → Change visibility → Public.

If a job fails, fix it on `main` and run the workflow again for the tag. A re-run replaces the release files and keeps the notes. A wrong tag: delete the draft release and the tag (`git push origin :refs/tags/vX.Y.Z`), then tag again.

To try the whole build without publishing, start it by hand: Actions → Release → Run workflow. It builds every binary and image, but pushes nothing and creates no release; the archives stay as workflow artifacts.

Not done yet: Compose and the install scripts still build the images locally rather than pulling them from GHCR, and REMOTE-CLIENTS still installs `amem` with `cargo install`.
