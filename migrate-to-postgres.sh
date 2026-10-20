#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────
# Agent Memory: move an install from SQLite to PostgreSQL. Run it from the
# install directory (the one holding .env), after updating the code. It
# backs up the data, points .env at PostgreSQL (an existing server, or one
# started for you with Docker), copies every memory, user, team, agent and
# skill, then restarts memory-core on PostgreSQL and checks it. The SQLite
# files are left as they were, so going back only takes the old .env.
# Manual equivalent: docs/MIGRATE-TO-POSTGRES.md.
#
#   ./migrate-to-postgres.sh                     interactive
#   ./migrate-to-postgres.sh --non-interactive   take answers from the environment
#   ./migrate-to-postgres.sh --help
#
# Non-interactive variables:
#   MIGRATE_TARGET             postgres-docker (default) or postgres-url
#   AGENT_MEMORY_POSTGRES_URL  target database, for MIGRATE_TARGET=postgres-url
#   MIGRATE_SKIP_BACKUP        1 to skip the backup (not recommended)
# ──────────────────────────────────────────────────────────────────────
set -euo pipefail

cd "$(dirname "$0")"
# shellcheck source=install.sh
source ./install.sh

BACKUP_ROOT="backups"

usage() {
  sed -n '3,19p' "$0" | sed 's/^# \{0,1\}//'
}

load_env() {
  [[ -f $ENV_FILE ]] || fail "No $ENV_FILE here. Run this from an existing install."
  # A URL given when calling the script wins over .env, where every install
  # has the setting, usually empty.
  local url=${AGENT_MEMORY_POSTGRES_URL:-}
  read_env_file "$ENV_FILE"
  AGENT_MEMORY_POSTGRES_URL=${url:-${AGENT_MEMORY_POSTGRES_URL:-}}
  IFS=, read -r -a PROFILES <<<"${COMPOSE_PROFILES:-}"
  CORE_PORT=${CORE_PORT:-8420}
  case ${AGENT_MEMORY_STORAGE_BACKEND:-sqlite} in
    sqlite) ;;
    postgres)
      ok "This install already stores its data in PostgreSQL; nothing to do."
      exit 0
      ;;
    *) fail "Unknown AGENT_MEMORY_STORAGE_BACKEND in $ENV_FILE: $AGENT_MEMORY_STORAGE_BACKEND" ;;
  esac
}

# set_env KEY VALUE — replace KEY's line in .env, or append it. The value is
# written literally, as read_env_file reads it back.
set_env() {
  local key=$1 value=$2 tmp
  tmp=$(mktemp "$ENV_FILE.XXXXXX")
  awk -v k="$key" -v v="$value" '
    BEGIN { done = 0 }
    $0 ~ "^" k "=" { if (!done) print k "=" v; done = 1; next }
    { print }
    END { if (!done) print k "=" v }
  ' "$ENV_FILE" >"$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$ENV_FILE"
}

# ── 1. target ────────────────────────────────────────────────────────

choose_target() {
  echo
  choose MIGRATE_TARGET "Move the data to" postgres-docker \
    "postgres-docker|PostgreSQL started for you with Docker (pgvector/pgvector:pg16)" \
    "postgres-url|An existing PostgreSQL server (needs the pgvector extension)"
  case $MIGRATE_TARGET in
    postgres-docker)
      [[ -n ${POSTGRES_PASSWORD:-} ]] || POSTGRES_PASSWORD=$(random_secret)
      AGENT_MEMORY_POSTGRES_URL="postgres://${POSTGRES_USER:-memory}:${POSTGRES_PASSWORD}@postgres:5432/${POSTGRES_DB:-memory}"
      [[ " ${PROFILES[*]:-} " == *" postgres "* ]] || PROFILES+=(postgres)
      ;;
    postgres-url)
      ask_url AGENT_MEMORY_POSTGRES_URL "PostgreSQL URL (postgres://user:pass@host:5432/db)" check_postgres_url PostgreSQL
      ;;
    *) fail "Unknown MIGRATE_TARGET: $MIGRATE_TARGET" ;;
  esac
}

# ── 2. backup ────────────────────────────────────────────────────────

backup() {
  local stamp old_umask
  stamp=$(date +%Y%m%d-%H%M%S)
  BACKUP_DIR="$BACKUP_ROOT/pre-postgres-$stamp"
  old_umask=$(umask)
  umask 077
  mkdir -p "$BACKUP_DIR"
  cp "$ENV_FILE" "$BACKUP_DIR/env"
  echo
  bold "Stopping memory-core so the SQLite files stop changing…"
  docker compose stop memory-core >/dev/null
  if [[ ${MIGRATE_SKIP_BACKUP:-0} == 1 ]]; then
    warn "Skipping the data backup (MIGRATE_SKIP_BACKUP=1); .env saved to $BACKUP_DIR/env."
  else
    bold "Backing up to $BACKUP_DIR…"
    # stdin is closed so `compose run` cannot swallow later answers.
    docker compose run --rm --no-deps -T --entrypoint tar memory-core czf - -C /data . \
      </dev/null >"$BACKUP_DIR/memory-core-data.tgz"
    ok "memory-core data archived to $BACKUP_DIR/memory-core-data.tgz"
  fi
  umask "$old_umask"
}

# ── 3. copy ──────────────────────────────────────────────────────────

# migrate_storage [--dry-run] — run the copy in a one-off memory-core
# container: same image, same /data volume, same settings as the service.
migrate_storage() {
  docker compose run --rm -T -e RUST_LOG=warn memory-core migrate-storage "$@" </dev/null
}

copy_data() {
  local profiles
  profiles=$(IFS=,; echo "${PROFILES[*]:-}")
  set_env COMPOSE_PROFILES "$profiles"
  set_env AGENT_MEMORY_POSTGRES_URL "$AGENT_MEMORY_POSTGRES_URL"
  [[ $MIGRATE_TARGET == postgres-docker ]] && set_env POSTGRES_PASSWORD "$POSTGRES_PASSWORD"
  ok "Pointed $ENV_FILE at PostgreSQL (storage backend still sqlite until the copy succeeds)."

  echo
  bold "Building memory-core from this checkout…"
  docker compose build memory-core

  echo
  bold "What will be copied"
  migrate_storage --dry-run ||
    fail "The check failed (see above). Nothing was copied; restore with: cp $BACKUP_DIR/env $ENV_FILE && docker compose up -d"
  local go
  ask go "Copy it now? (y/n)" y
  [[ $go =~ ^[Yy] ]] ||
    fail "Stopped before copying. To undo the .env changes: cp $BACKUP_DIR/env $ENV_FILE && docker compose up -d"

  echo
  bold "Copying…"
  migrate_storage ||
    fail "The copy failed (see above). SQLite is untouched; fix the cause and re-run, or restore: cp $BACKUP_DIR/env $ENV_FILE && docker compose up -d"
}

# ── 4. switch ────────────────────────────────────────────────────────

switch_backend() {
  set_env AGENT_MEMORY_STORAGE_BACKEND postgres
  start_services
  local health
  health=$(curl -fsS "http://127.0.0.1:$CORE_PORT/health")
  [[ $health == *'"backend":"postgres"'* ]] ||
    fail "memory-core is up but not on PostgreSQL: $health"
  ok "memory-core now stores its data in PostgreSQL."
}

print_migrate_summary() {
  echo
  bold "Migration complete"
  info "Storage : PostgreSQL${PROFILES[*]:+ (profiles: ${PROFILES[*]})}"
  info "Backup  : $BACKUP_DIR"
  info "Clients need no change: same URL, same user keys, same team and agent ids."
  echo
  bold "To go back to SQLite"
  info "cp $BACKUP_DIR/env $ENV_FILE && docker compose up -d"
  info "Anything written after this migration lives only in PostgreSQL."
  echo
  bold "Next, for several memory-core replicas"
  info "Add shared coordination (Redis): docs/MIGRATE-TO-POSTGRES.md#next-several-replicas"
}

main_migrate() {
  for arg in "$@"; do
    case $arg in
      --non-interactive) INTERACTIVE=false ;;
      -h | --help) usage; exit 0 ;;
      *) fail "Unknown option: $arg (see --help)" ;;
    esac
  done

  bold "Agent Memory: SQLite → PostgreSQL"
  require_docker
  load_env
  choose_target
  backup
  copy_data
  switch_backend
  print_migrate_summary
}

main_migrate "$@"
