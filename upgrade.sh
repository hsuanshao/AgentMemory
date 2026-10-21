#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────
# Agent Memory upgrade: run it from the updated checkout of an existing
# install (the directory holding its .env). It backs up the data, renames
# and adds settings in .env, rebuilds and restarts the services (memory-core
# migrates the stored data as it starts) and, on an install older than
# 0.1.1, moves the memories in the "default" bucket to a real team and
# agent and prints the user key and `amem login` line for the clients.
# Every step is safe to repeat. Manual equivalent: docs/UPGRADE.md.
#
#   ./upgrade.sh                     interactive
#   ./upgrade.sh --non-interactive   take answers from the environment
#   ./upgrade.sh --help
#
# Non-interactive variables (all optional):
#   UPGRADE_ADMIN_NAME   system admin who owns the moved data   (default: oldest admin)
#   UPGRADE_TEAM_NAME    team receiving it, created if missing   (default default-team)
#   UPGRADE_AGENT_NAME   agent receiving it, created if missing  (default default-agent-<admin>)
#   UPGRADE_SKIP_BACKUP  1 to skip the backup (not recommended)
# ──────────────────────────────────────────────────────────────────────
set -euo pipefail

cd "$(dirname "$0")"
# shellcheck source=install.sh
source ./install.sh

BACKUP_ROOT="backups"
# Settings added since the first release, with the value an old install
# implicitly used (an exported value wins). Missing ones are appended to .env.
NEW_SETTINGS=("MEMORY_IMAGE_TAG=${MEMORY_IMAGE_TAG:-local}")
# Settings renamed since, OLD=NEW.
RENAMED_SETTINGS=("CORE_INSTANCE_ID=CORE_DEPARTMENT_ID")

usage() {
  sed -n '3,19p' "$0" | sed 's/^# \{0,1\}//'
}

load_env() {
  [[ -f $ENV_FILE ]] || fail "No $ENV_FILE here. Run this from an existing install, or use ./install.sh for a new one."
  read_env_file "$ENV_FILE"
  IFS=, read -r -a PROFILES <<<"${COMPOSE_PROFILES:-}"
  [[ -n ${AGENT_MEMORY_API_KEY:-} ]] || fail "AGENT_MEMORY_API_KEY is empty in $ENV_FILE; the upgrade needs it."
  CORE_PORT=${CORE_PORT:-8420}
  PANEL_PORT=${PANEL_PORT:-8125}
  # Read before rename_settings runs, so the old name still counts.
  DEPARTMENT=${CORE_DEPARTMENT_ID:-${CORE_INSTANCE_ID:-default}}
}

# ── 1. backup ────────────────────────────────────────────────────────

backup() {
  if [[ ${UPGRADE_SKIP_BACKUP:-0} == 1 ]]; then
    warn "Skipping the backup (UPGRADE_SKIP_BACKUP=1)."
    return
  fi
  local stamp old_umask
  stamp=$(date +%Y%m%d-%H%M%S)
  BACKUP_DIR="$BACKUP_ROOT/$stamp"
  old_umask=$(umask)
  umask 077
  mkdir -p "$BACKUP_DIR"
  echo
  bold "Backing up to $BACKUP_DIR…"
  cp "$ENV_FILE" "$BACKUP_DIR/env"
  # The code was updated just before this script ran: the commit before
  # that checkout is the previous reflog entry.
  git rev-parse 'HEAD@{1}' >"$BACKUP_DIR/previous-commit" 2>/dev/null || true
  # Keep the running images under a second tag, so a rollback needs no rebuild.
  local image tag=${MEMORY_IMAGE_TAG:-local}
  for image in memory-core memory-panel; do
    if docker image inspect "$image:$tag" >/dev/null 2>&1; then
      docker image tag "$image:$tag" "$image:pre-upgrade-$stamp"
      echo "$image:pre-upgrade-$stamp" >>"$BACKUP_DIR/images"
    fi
  done
  [[ -f $BACKUP_DIR/images ]] && ok "Previous images kept as :pre-upgrade-$stamp"

  case ${AGENT_MEMORY_STORAGE_BACKEND:-sqlite} in
    postgres)
      if [[ " ${PROFILES[*]:-} " == *" postgres "* ]]; then
        docker compose exec -T postgres pg_dump -U "${POSTGRES_USER:-memory}" "${POSTGRES_DB:-memory}" \
          </dev/null >"$BACKUP_DIR/postgres.sql"
      else
        local dump_image
        dump_image=$(pg_dump_image) ||
          fail "Cannot read the version of the server at AGENT_MEMORY_POSTGRES_URL. Back the database up yourself, then re-run with UPGRADE_SKIP_BACKUP=1."
        docker run --rm --network host "$dump_image" pg_dump "$AGENT_MEMORY_POSTGRES_URL" \
          </dev/null >"$BACKUP_DIR/postgres.sql" ||
          fail "pg_dump of AGENT_MEMORY_POSTGRES_URL failed. Back the database up yourself, then re-run with UPGRADE_SKIP_BACKUP=1."
      fi
      ok "Database dumped to $BACKUP_DIR/postgres.sql"
      ;;
  esac
  # memory-core's /data holds the SQLite files, local coordination state
  # and the L2/L3 profile files on every backend. Stop writers first so the
  # copies are consistent; start_services brings everything back.
  docker compose stop memory-core >/dev/null
  archive_volume memory-core memory-core-data
  if [[ " ${PROFILES[*]:-} " == *" redis "* ]]; then
    # Redis runs with appendonly, so archive its whole data directory.
    docker compose stop redis >/dev/null
    archive_volume redis redis-data
  fi
  umask "$old_umask"
}

# pg_dump_image — the postgres image whose pg_dump matches the server at
# AGENT_MEMORY_POSTGRES_URL. pg_dump refuses a server newer than itself, so
# a fixed client version breaks as soon as the server is upgraded.
pg_dump_image() {
  local num
  num=$(docker run --rm --network host "$PG_CHECK_IMAGE" \
    psql "$AGENT_MEMORY_POSTGRES_URL" -tAc 'SHOW server_version_num' </dev/null) || return 1
  [[ $num =~ ^[0-9]+$ ]] || return 1
  echo "postgres:$((num / 10000))-alpine"
}

# archive_volume SERVICE NAME — tar the service's /data into the backup.
# stdin is closed so `compose run` cannot swallow the answers to later prompts.
archive_volume() {
  docker compose run --rm --no-deps -T --entrypoint tar "$1" czf - -C /data . </dev/null >"$BACKUP_DIR/$2.tgz"
  ok "$1 data archived to $BACKUP_DIR/$2.tgz"
}

# ── 2. settings ──────────────────────────────────────────────────────

rename_settings() {
  local pair old new
  for pair in "${RENAMED_SETTINGS[@]}"; do
    old=${pair%%=*} new=${pair#*=}
    if grep -q "^$old=" "$ENV_FILE"; then
      if grep -q "^$new=" "$ENV_FILE"; then
        sed -i.bak "/^$old=/d" "$ENV_FILE"
      else
        sed -i.bak "s/^$old=/$new=/" "$ENV_FILE"
      fi
      rm -f "$ENV_FILE.bak"
      ok "Renamed $old to $new in $ENV_FILE"
    fi
  done
}

add_new_settings() {
  local setting added=()
  for setting in "${NEW_SETTINGS[@]}"; do
    if ! grep -q "^${setting%%=*}=" "$ENV_FILE"; then
      printf '%s\n' "$setting" >>"$ENV_FILE"
      export "${setting?}"
      added+=("$setting")
    fi
  done
  if ((${#added[@]})); then
    ok "Added to $ENV_FILE: ${added[*]}"
  else
    info "$ENV_FILE already has every setting."
  fi
}

# The new version validates its configuration at startup and refuses some
# settings an older one accepted, e.g. distillation CLI arguments that grant
# the model capability (#37). Check with the new image before starting it.
# The check image gets its own tag, so the tag the running services use (and
# the backup's pre-upgrade copy of it) still points at the old version.
# Anything that fails between the backup and starting the new version (the
# build, the check, a settings step) ends in roll_back: .env as it was and the
# previous version running again.
CHECK_TAG=upgrade-check
check_config() {
  info "Building the new version and checking the configuration…"
  MEMORY_IMAGE_TAG=$CHECK_TAG docker compose build memory-core
  local out rc=0 name="agentmemory-upgrade-check-$$" limit=()
  # A binary without `check-config` would start the server instead and never
  # exit; the timeout turns that into a refusal rather than a hang. macOS has
  # no `timeout`, so the check runs without one there.
  command -v timeout >/dev/null && limit=(timeout 120)
  out=$(MEMORY_IMAGE_TAG=$CHECK_TAG ${limit[@]+"${limit[@]}"} \
    docker compose run --rm --no-deps -T --name "$name" memory-core check-config 2>&1 </dev/null) || rc=$?
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker image rm -f "memory-core:$CHECK_TAG" >/dev/null 2>&1 || true
  if ((rc != 0)); then
    printf '%s\n' "$out" >&2
    ((rc == 124)) && warn "The configuration check did not finish within 120 s."
    fail "The new version refuses this configuration (above). Fix $ENV_FILE and run ./upgrade.sh again."
  fi
  ok "Configuration accepted."
}

# roll_back — on any failure after the backup until the new version answers
# /health: put .env back and run the previous version again. The services may
# be stopped (backup) or already recreated on the new image (start_services),
# so the previous images, which the backup tagged :pre-upgrade-<stamp>, take
# back the running tag before `docker compose up`. Runs from the EXIT trap, so
# `set -e` exits are covered; `set +e` inside, so one failed step does not end
# the rollback early.
ROLL_BACK_ON_FAILURE=false
roll_back() {
  local rc=$? image tag=${MEMORY_IMAGE_TAG:-local} env_restored=true
  set +e
  if ((rc != 0)) && [[ $ROLL_BACK_ON_FAILURE == true ]]; then
    warn "The upgrade did not finish; going back to the previous version."
    if ! cp "$ENV_BEFORE_UPGRADE" "$ENV_FILE"; then
      env_restored=false
      if [[ -f $ENV_BEFORE_UPGRADE ]]; then
        warn "Could not restore $ENV_FILE. The copy from before the upgrade is kept at $ENV_BEFORE_UPGRADE${BACKUP_DIR:+ (also $BACKUP_DIR/env)}; copy it back by hand."
      else
        warn "Could not restore $ENV_FILE${BACKUP_DIR:+; the copy from before the upgrade is $BACKUP_DIR/env}."
      fi
    fi
    docker rm -f "agentmemory-upgrade-check-$$" >/dev/null 2>&1
    docker image rm -f "memory-core:$CHECK_TAG" >/dev/null 2>&1
    if [[ -n ${BACKUP_DIR:-} && -f $BACKUP_DIR/images ]]; then
      while read -r image; do
        docker image tag "$image" "${image%%:*}:$tag" ||
          warn "Could not restore $image as ${image%%:*}:$tag."
      done <"$BACKUP_DIR/images"
    else
      warn "No previous images were kept (UPGRADE_SKIP_BACKUP=1?); the services start on whatever image they have."
    fi
    if docker compose up -d --no-build >/dev/null 2>&1; then
      if [[ $env_restored == true ]]; then
        warn "$ENV_FILE is as it was and the previous version runs again. Backup: ${BACKUP_DIR:-none}."
      else
        warn "The previous version runs again, but $ENV_FILE was not restored (above). Backup: ${BACKUP_DIR:-none}."
      fi
      warn "If the new version had already migrated stored data, restore the backup as well (docs/UPGRADE.md, Roll back)."
    else
      warn "Could not start the previous version; run: docker compose up -d --no-build"
    fi
  fi
  # Keep the copy when it could not be put back: it may be the only one.
  [[ $env_restored == true ]] && rm -f "$ENV_BEFORE_UPGRADE"
  exit "$rc"
}

# ── 3. move the legacy data ──────────────────────────────────────────

# json_field JSON name — first string or number value of "name" (flat fields only)
json_field() {
  sed -n "s/.*\"$2\":\"\{0,1\}\([^\",}]*\).*/\1/p" <<<"$1" | head -n 1 | sed 's/^null$//'
}

core_upgrade() {
  curl -sS -X POST "http://127.0.0.1:$CORE_PORT/v3/internal/upgrade" \
    -H "Authorization: Bearer $AGENT_MEMORY_API_KEY" \
    -H "x-memory-department-id: $DEPARTMENT" -H "content-type: application/json" \
    -d "$1"
}

upgrade_body() {
  local body="{\"dry_run\":$1,\"issue_key\":$2"
  [[ -n ${UPGRADE_ADMIN_NAME:-} ]] && body+=",\"admin_username\":\"$UPGRADE_ADMIN_NAME\""
  [[ -n ${UPGRADE_TEAM_NAME:-} ]] && body+=",\"team_name\":\"$UPGRADE_TEAM_NAME\""
  [[ -n ${UPGRADE_AGENT_NAME:-} ]] && body+=",\"agent_name\":\"$UPGRADE_AGENT_NAME\""
  printf '%s}' "$body"
}

describe_moved() {
  printf 'conversations %s, memories %s, L2/L3 files %s, skills %s' \
    "$(json_field "$1" conversations)" "$(json_field "$1" atoms)" \
    "$(json_field "$1" profile_files)" "$(json_field "$1" skill_versions)"
}

# moved_nothing JSON — whether the report found no legacy rows at all.
moved_nothing() {
  local field
  for field in conversations atoms profile_files skill_versions; do
    [[ $(json_field "$1" "$field") == 0 ]] || return 1
  done
}

migrate_legacy() {
  echo
  bold "Memories written before 0.1.1"
  local plan
  plan=$(core_upgrade "$(upgrade_body true false)")
  if [[ $plan == *'no active system admin'* ]]; then
    info "This install has no administrator yet; creating one."
    create_admin
    plan=$(core_upgrade "$(upgrade_body true false)")
  fi
  [[ $plan == *'"code":0'* ]] || fail "Upgrade check failed: $(json_field "$plan" message)"
  if moved_nothing "$plan"; then
    info "None found; nothing to move."
    return
  fi
  LEGACY_MOVED=true

  local admin team agent
  admin=$(json_field "$plan" username)
  ask UPGRADE_TEAM_NAME "Team that receives the old memories" "$(json_field "$plan" team_name)"
  ask UPGRADE_AGENT_NAME "Agent that receives them (use the name you will give your main client)" \
    "$(json_field "$plan" agent_name)"
  UPGRADE_ADMIN_NAME=${UPGRADE_ADMIN_NAME:-$admin}
  plan=$(core_upgrade "$(upgrade_body true false)")
  [[ $plan == *'"code":0'* ]] || fail "Upgrade check failed: $(json_field "$plan" message)"
  team=$(json_field "$plan" team_id)
  agent=$(json_field "$plan" agent_id)
  info "Owner : $UPGRADE_ADMIN_NAME"
  info "Team  : $UPGRADE_TEAM_NAME ${team:-(will be created)}"
  info "Agent : $UPGRADE_AGENT_NAME ${agent:-(will be created)}"
  info "Found : $(describe_moved "$plan")"
  local go
  ask go "Proceed? (y/n)" y
  [[ $go =~ ^[Yy] ]] || fail "Stopped before moving anything. Re-run ./upgrade.sh when ready."

  local result
  result=$(core_upgrade "$(upgrade_body false true)")
  if [[ $result == *target_has_profile* ]]; then
    fail "Agent '$UPGRADE_AGENT_NAME' already has L2/L3 memories. Re-run and pick a new agent name."
  fi
  [[ $result == *'"code":0'* ]] || fail "Upgrade failed: $(json_field "$result" message)"
  ok "Moved: $(describe_moved "$result")"
  TEAM_ID=$(json_field "$result" team_id)
  AGENT_ID=$(json_field "$result" agent_id)
  USER_KEY=$(json_field "$result" user_key)
}

# ── 4. what the clients need ─────────────────────────────────────────

write_client_settings() {
  CLIENT_FILE="upgrade-client-settings.txt"
  (
    umask 077
    cat >"$CLIENT_FILE" <<EOF
# Written by upgrade.sh on $(date -u +%Y-%m-%dT%H:%M:%SZ). Contains a user key: keep it private.
# Replace 127.0.0.1 with this server's address when the client runs elsewhere.
AMEM_SERVER_URL=http://127.0.0.1:$CORE_PORT
AMEM_DEPARTMENT_ID=$DEPARTMENT
AMEM_TEAM_ID=$TEAM_ID
AMEM_AGENT_ID=$AGENT_ID
AMEM_USER_KEY=$USER_KEY

# On each client machine (see docs/REMOTE-CLIENTS.md to install amem):
#   amem login --server-url http://127.0.0.1:$CORE_PORT --department-id $DEPARTMENT \\
#     --team-id $TEAM_ID --agent-id $AGENT_ID --user-key -
# then paste the key above and press Enter.
EOF
  )
}

print_upgrade_summary() {
  echo
  bold "Upgrade complete"
  if [[ ${LEGACY_MOVED:-false} != true ]]; then
    info "Monitor Panel : http://localhost:$PANEL_PORT"
    [[ -n ${BACKUP_DIR:-} ]] && info "Backup        : $BACKUP_DIR  (restore steps: docs/UPGRADE.md)"
    info "Update amem on every client to this version; docs/UPGRADE.md lists what else changed."
    return
  fi
  info "Monitor Panel : http://localhost:$PANEL_PORT  (log in with the key below)"
  info "Team / agent  : $TEAM_ID / $AGENT_ID"
  [[ -n ${BACKUP_DIR:-} ]] && info "Backup        : $BACKUP_DIR  (restore steps: docs/UPGRADE.md)"
  if [[ -n ${USER_KEY:-} ]]; then
    echo
    bold "New user key for $UPGRADE_ADMIN_NAME (also saved in $CLIENT_FILE, mode 600):"
    info "$USER_KEY"
  fi
  echo
  bold "Next: point each client at the new server"
  info "amem login --server-url http://127.0.0.1:$CORE_PORT --department-id $DEPARTMENT --team-id $TEAM_ID --agent-id $AGENT_ID --user-key -"
  info "Old clients (Bearer key only) stop working now; docs/UPGRADE.md lists what to change for each one."
}

main_upgrade() {
  for arg in "$@"; do
    case $arg in
      --non-interactive) INTERACTIVE=false ;;
      -h | --help) usage; exit 0 ;;
      *) fail "Unknown option: $arg (see --help)" ;;
    esac
  done

  bold "Agent Memory upgrade"
  require_docker
  load_env
  # A copy of .env as it was, so a failed upgrade can put it back.
  ENV_BEFORE_UPGRADE=$(umask 077 && mktemp)
  trap roll_back EXIT
  cp "$ENV_FILE" "$ENV_BEFORE_UPGRADE"
  ROLL_BACK_ON_FAILURE=true
  backup
  rename_settings
  add_new_settings
  check_config
  # start_services waits for /health; only a version that answers is kept.
  start_services
  ROLL_BACK_ON_FAILURE=false
  migrate_legacy
  [[ ${LEGACY_MOVED:-false} == true ]] && write_client_settings
  print_upgrade_summary
}

main_upgrade "$@"
