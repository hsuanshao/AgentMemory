#!/usr/bin/env bash
# Checks that every file carrying the release number agrees (docs/ops/RELEASE.md §2),
# that the list of schema migrations ends at the version in the code (and, for a
# release, that none still waits for its release number), and that nothing secret
# or private is tracked (RELEASE.md §3 step 5).
#
#   .github/scripts/check-consistency.sh            # versions agree with Cargo.toml
#   .github/scripts/check-consistency.sh 0.2.0      # ... and equal 0.2.0 (release tags)
set -euo pipefail
cd "$(dirname "$0")/../.."

failed=0
problem() {
  echo "::error::$*"
  failed=1
}

cargo_version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
[[ -n $cargo_version ]] || problem "No [workspace.package] version in Cargo.toml"
expected=${1:-$cargo_version}
[[ $cargo_version == "$expected" ]] || problem "Cargo.toml has $cargo_version, expected $expected"

check() {
  local file=$1 query=$2 found
  found=$(jq -r "$query" "$file")
  [[ $found == "$expected" ]] || problem "$file ($query) has $found, expected $expected"
}
check clients/claude-code/.claude-plugin/plugin.json .version
check clients/codex/.codex-plugin/plugin.json .version
check panel/package.json .version
check panel/server/package.json .version
check panel/web/package.json .version
check panel/package-lock.json .version
check panel/package-lock.json '.packages[""].version'
check panel/package-lock.json '.packages.server.version'
check panel/package-lock.json '.packages.web.version'

# Cargo.lock follows the workspace version once something is built.
for crate in memory-domain memory-application memory-infrastructure memory-gateway memory-server memory-client; do
  locked=$(grep -A1 "^name = \"$crate\"$" Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')
  [[ $locked == "$expected" ]] || problem "Cargo.lock has $crate $locked, expected $expected"
done

# The list of schema migrations (docs/ops/SCHEMA-MIGRATIONS §3) ends at the
# version in the code, in both languages. Redis numbers its own steps, so it
# is not compared.
sqlite_schema=$(sed -n 's/^pub const SCHEMA_VERSION: i64 = \([0-9]*\);/\1/p' \
  crates/memory-infrastructure/src/sqlite/schema.rs)
postgres_schema=$(sed -n '/^pub const MIGRATIONS/,/^];/p' crates/memory-infrastructure/src/postgres/schema.rs |
  grep -oE '^ *\(? *[0-9]+,' | grep -oE '[0-9]+' | sort -n | tail -1)
[[ -n $sqlite_schema && -n $postgres_schema ]] || problem "Cannot read the schema version from sqlite/schema.rs or postgres/schema.rs"
[[ $sqlite_schema == "$postgres_schema" ]] ||
  problem "SQLite is at schema $sqlite_schema, Postgres at $postgres_schema"
for doc in docs/ops/SCHEMA-MIGRATIONS.md docs/ops/SCHEMA-MIGRATIONS.zh-tw.md; do
  listed=$(grep -oE '^\| [0-9]+ \|' "$doc" | grep -oE '[0-9]+' | sort -n | tail -1)
  [[ $listed == "$postgres_schema" ]] ||
    problem "$doc lists migrations up to ${listed:-none}, the code is at $postgres_schema"
done

# A release names every migration's release and the UPGRADE section it brings.
if [[ $# -gt 0 ]]; then
  if grep -nE '^\| [0-9]+ \| (next|下一版) \|' docs/ops/SCHEMA-MIGRATIONS.md docs/ops/SCHEMA-MIGRATIONS.zh-tw.md; then
    problem "Migrations above have no release yet; give them $expected"
  fi
  if grep -nE '^## (To the next release|升級到下一版)' docs/UPGRADE.md docs/UPGRADE.zh-tw.md; then
    problem "UPGRADE sections above have no version yet; give them $expected"
  fi
fi

# API keys and private keys. Test fixtures build fake keys at runtime so they never match.
if git grep -nE 'sk-(ant|proj|mem)-[A-Za-z0-9_-]{16}|ghp_[A-Za-z0-9]{20}|BEGIN [A-Z ]*PRIVATE KEY'; then
  problem "Tracked files contain what looks like a key (above)"
fi
# Private hosts and home directories; /home/memory is the container's service user.
if git grep -nP 'ssh://|/home/(?!memory\b)[a-z]|/Users/[a-z]' -- ':!.github/scripts/'; then
  problem "Tracked files contain an ssh URL or a home directory (above)"
fi

((failed == 0)) && echo "Versions are $expected everywhere; no keys or private paths found."
exit "$failed"
