#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────
# Agent Memory installer: asks a few questions, writes .env, starts the
# services with Docker Compose and creates the first administrator.
#
#   ./install.sh                     interactive
#   ./install.sh --non-interactive   take answers from the environment
#   ./install.sh --help
#
# Non-interactive variables (all optional):
#   INSTALL_STORAGE        sqlite | postgres-docker | postgres-url   (default sqlite)
#   INSTALL_COORDINATION   local | redis-docker | redis-url          (default local)
#   INSTALL_LLM            openai | claude | antigravity | codex     (default openai)
#   INSTALL_ADMIN_NAME     first administrator's username            (default admin)
#   AGENT_MEMORY_POSTGRES_URL, AGENT_MEMORY_REDIS_URL, AGENT_MEMORY_LLM_BASE_URL,
#   AGENT_MEMORY_LLM_API_KEY, AGENT_MEMORY_LLM_MODEL, ANTHROPIC_API_KEY,
#   PROJECT_DIR, CORE_PORT, PANEL_PORT, MEMORY_IMAGE_TAG
# ──────────────────────────────────────────────────────────────────────
set -euo pipefail

cd "$(dirname "$0")"

ENV_FILE=".env"
INTERACTIVE=true
PG_CHECK_IMAGE="postgres:16-alpine"
REDIS_CHECK_IMAGE="redis:8.4.0"
HEALTH_TIMEOUT_SECS=180

# ── output helpers ───────────────────────────────────────────────────

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
info() { printf '  %s\n' "$*"; }
warn() { printf '\033[33m! %s\033[0m\n' "$*" >&2; }
fail() { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }
ok() { printf '\033[32m✓ %s\033[0m\n' "$*"; }

usage() {
  sed -n '3,20p' "$0" | sed 's/^# \{0,1\}//'
}

# ── prompts (fall back to defaults when non-interactive) ─────────────

# ask VAR "Question" "default"
ask() {
  local __var=$1 question=$2 default=${3:-} answer
  if [[ $INTERACTIVE == false ]]; then
    printf -v "$__var" '%s' "${!__var:-$default}"
    return
  fi
  read -r -p "  $question${default:+ [$default]}: " answer
  printf -v "$__var" '%s' "${answer:-$default}"
}

# ask_secret VAR "Question" — input is not echoed
ask_secret() {
  local __var=$1 question=$2 answer
  if [[ $INTERACTIVE == false ]]; then
    printf -v "$__var" '%s' "${!__var:-}"
    return
  fi
  read -r -s -p "  $question: " answer
  echo
  printf -v "$__var" '%s' "$answer"
}

# choose VAR "Title" default option1 option2 … — options are "value|label"
choose() {
  local __var=$1 title=$2 default=$3
  shift 3
  if [[ $INTERACTIVE == false ]]; then
    printf -v "$__var" '%s' "${!__var:-$default}"
    return
  fi
  echo
  bold "$title"
  local i=1 values=() option
  for option in "$@"; do
    values+=("${option%%|*}")
    printf '  [%d] %s\n' "$i" "${option#*|}"
    i=$((i + 1))
  done
  local pick
  while true; do
    read -r -p "  Choose 1-$# [1]: " pick
    pick=${pick:-1}
    if [[ $pick =~ ^[0-9]+$ ]] && ((pick >= 1 && pick <= $#)); then
      printf -v "$__var" '%s' "${values[$((pick - 1))]}"
      return
    fi
    warn "Please enter a number between 1 and $#."
  done
}

# read_env_file FILE — load KEY=VALUE lines as plain shell variables, taking
# values literally and without exporting them. `source` would strip the quotes
# inside JSON values such as AGENT_MEMORY_LLM_CLI_ARGS, and an exported value
# overrides .env when docker compose interpolates docker-compose.yml.
read_env_file() {
  local line key value
  while IFS= read -r line || [[ -n $line ]]; do
    [[ $line =~ ^[[:space:]]*(#|$) ]] && continue
    [[ $line =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]] || continue
    key=${BASH_REMATCH[1]} value=${BASH_REMATCH[2]}
    if [[ $value =~ ^\'(.*)\'$ ]]; then
      value=${BASH_REMATCH[1]}
    fi
    printf -v "$key" '%s' "$value"
  done <"$1"
}

random_secret() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 24
  else
    head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n'
  fi
}

# ── environment checks ───────────────────────────────────────────────

require_docker() {
  command -v docker >/dev/null 2>&1 || fail "Docker is required: https://docs.docker.com/get-docker/"
  docker info >/dev/null 2>&1 || fail "Docker is installed but the daemon is not reachable (is it running?)."
  docker compose version >/dev/null 2>&1 || fail "Docker Compose v2 is required (the 'docker compose' command)."
  command -v curl >/dev/null 2>&1 || fail "curl is required."
}

port_in_use() {
  ss -ltnH "sport = :$1" 2>/dev/null | grep -q .
}

ask_port() {
  local __var=$1 question=$2 default=$3 port
  while true; do
    ask port "$question" "$default"
    if [[ ! $port =~ ^[0-9]+$ ]] || ((port < 1 || port > 65535)); then
      warn "Not a valid port: $port"
    elif port_in_use "$port" && ! container_owns_port "$port"; then
      warn "Port $port is already in use; pick another."
    else
      printf -v "$__var" '%s' "$port"
      return
    fi
    [[ $INTERACTIVE == false ]] && fail "Port $port is not usable."
  done
}

# A re-run of this installer may find its own containers on the port.
container_owns_port() {
  docker compose ps --format '{{.Ports}}' 2>/dev/null | grep -q ":$1->"
}

warn_if_loopback() {
  if [[ $1 =~ @(localhost|127\.0\.0\.1)[:/] || $1 =~ ://(localhost|127\.0\.0\.1)[:/] ]]; then
    warn "'localhost' inside the memory-core container is the container itself."
    warn "For a server on this machine use host.docker.internal instead."
  fi
}

# Tests connectivity from a container, i.e. from memory-core's point of view.
check_postgres_url() {
  docker run --rm --add-host=host.docker.internal:host-gateway "$PG_CHECK_IMAGE" \
    pg_isready -q -t 5 -d "$1" >/dev/null 2>&1
}

check_redis_url() {
  [[ $(docker run --rm --add-host=host.docker.internal:host-gateway "$REDIS_CHECK_IMAGE" \
    redis-cli -u "$1" ping 2>/dev/null) == "PONG" ]]
}

# ask_url VAR "Question" checker "name"
ask_url() {
  local __var=$1 question=$2 checker=$3 name=$4 url
  while true; do
    ask url "$question" "${!__var:-}"
    [[ -z $url ]] && { warn "A URL is required."; [[ $INTERACTIVE == false ]] && fail "$name URL missing."; continue; }
    warn_if_loopback "$url"
    info "Testing the $name connection from a container…"
    if "$checker" "$url"; then
      ok "$name is reachable."
      printf -v "$__var" '%s' "$url"
      return
    fi
    warn "Could not reach $name at that URL."
    [[ $INTERACTIVE == false ]] && fail "$name is not reachable."
  done
}

# ── questions ────────────────────────────────────────────────────────

configure_storage() {
  choose INSTALL_STORAGE "Storage backend" sqlite \
    "sqlite|SQLite (default, nothing to install)" \
    "postgres-url|PostgreSQL: use an existing server (needs the pgvector extension)" \
    "postgres-docker|PostgreSQL: start one for me with Docker (pgvector/pgvector:pg16)"
  STORAGE_BACKEND=sqlite
  case $INSTALL_STORAGE in
    sqlite) ;;
    postgres-url)
      STORAGE_BACKEND=postgres
      ask_url AGENT_MEMORY_POSTGRES_URL "PostgreSQL URL (postgres://user:pass@host:5432/db)" check_postgres_url PostgreSQL
      ;;
    postgres-docker)
      STORAGE_BACKEND=postgres
      POSTGRES_PASSWORD=$(random_secret)
      AGENT_MEMORY_POSTGRES_URL="postgres://memory:${POSTGRES_PASSWORD}@postgres:5432/memory"
      PROFILES+=(postgres)
      ;;
    *) fail "Unknown INSTALL_STORAGE: $INSTALL_STORAGE" ;;
  esac
}

configure_coordination() {
  choose INSTALL_COORDINATION "Coordination backend (pipeline timers, queue, locks)" local \
    "local|Single node (default, nothing to install)" \
    "redis-url|Redis: use an existing server (lets several memory-core replicas share work)" \
    "redis-docker|Redis: start one for me with Docker (redis:8.4.0)"
  COORDINATION_BACKEND=local
  case $INSTALL_COORDINATION in
    local) ;;
    redis-url)
      COORDINATION_BACKEND=redis
      ask_url AGENT_MEMORY_REDIS_URL "Redis URL (redis://[:password@]host:6379/0)" check_redis_url Redis
      ;;
    redis-docker)
      COORDINATION_BACKEND=redis
      AGENT_MEMORY_REDIS_URL="redis://redis:6379/0"
      PROFILES+=(redis)
      ;;
    *) fail "Unknown INSTALL_COORDINATION: $INSTALL_COORDINATION" ;;
  esac
}

configure_llm() {
  choose INSTALL_LLM "LLM used to distil memories" openai \
    "openai|OpenAI-compatible HTTP API (base URL + API key; works with self-hosted models)" \
    "claude|Claude Code CLI (bundled in the image)" \
    "antigravity|Antigravity CLI (bundled in the image)" \
    "codex|Codex CLI (bundled in the image)"
  LLM_PROVIDER=openai
  LLM_CLI_COMMAND="" LLM_CLI_ARGS="" LLM_CLI_PROMPT_MODE=stdin
  case $INSTALL_LLM in
    openai)
      ask AGENT_MEMORY_LLM_BASE_URL "API base URL" "${AGENT_MEMORY_LLM_BASE_URL:-https://api.openai.com/v1}"
      ask_secret AGENT_MEMORY_LLM_API_KEY "API key (leave empty to add later)"
      ask AGENT_MEMORY_LLM_MODEL "Model" "${AGENT_MEMORY_LLM_MODEL:-gpt-4o-mini}"
      ;;
    claude)
      LLM_PROVIDER=cli LLM_CLI_COMMAND=claude LLM_CLI_ARGS=-p
      ask_secret ANTHROPIC_API_KEY "ANTHROPIC_API_KEY (leave empty to log in with /login later)"
      ;;
    antigravity)
      LLM_PROVIDER=cli LLM_CLI_COMMAND=agy LLM_CLI_ARGS='["-p","{prompt}"]' LLM_CLI_PROMPT_MODE=arg
      ;;
    codex)
      # Read-only, no session file, and no hooks: a memory client installed
      # for Codex must never capture the distillation prompts themselves.
      LLM_PROVIDER=cli LLM_CLI_COMMAND=codex
      LLM_CLI_ARGS='["exec","--skip-git-repo-check","--ephemeral","-s","read-only","-c","features.hooks=false","-"]'
      ;;
    *) fail "Unknown INSTALL_LLM: $INSTALL_LLM" ;;
  esac
  if [[ $LLM_PROVIDER == cli ]]; then
    ask PROJECT_DIR "Project directory the CLI works in" "${PROJECT_DIR:-./workspace}"
    # The CLI runs inside the container as a non-root user with this as its
    # working directory, so a directory we create must stay traversable even
    # under a restrictive umask. An existing directory keeps its permissions.
    if [[ ! -d $PROJECT_DIR ]]; then
      mkdir -p "$PROJECT_DIR"
      chmod 755 "$PROJECT_DIR"
    fi
  fi
}

configure_ports() {
  echo
  bold "Ports"
  ask_port CORE_PORT "memory-core API port" "${CORE_PORT:-8420}"
  ask_port PANEL_PORT "Monitor Panel port" "${PANEL_PORT:-8125}"
}

# ── outputs ──────────────────────────────────────────────────────────

write_env() {
  if [[ -f $ENV_FILE ]]; then
    local backup
    backup="$ENV_FILE.bak.$(date +%Y%m%d%H%M%S)"
    cp "$ENV_FILE" "$backup"
    info "Previous settings saved to $backup"
  fi
  local profiles
  profiles=$(IFS=,; echo "${PROFILES[*]:-}")
  (
    umask 077
    cat >"$ENV_FILE" <<EOF
# Generated by install.sh on $(date -u +%Y-%m-%dT%H:%M:%SZ). Keep this file private.
COMPOSE_PROFILES=$profiles
AGENT_MEMORY_API_KEY=$AGENT_MEMORY_API_KEY
CORE_PORT=$CORE_PORT
PANEL_PORT=$PANEL_PORT
CORE_INSTANCE_ID=default
MEMORY_IMAGE_TAG=${MEMORY_IMAGE_TAG:-local}

AGENT_MEMORY_STORAGE_BACKEND=$STORAGE_BACKEND
AGENT_MEMORY_POSTGRES_URL=${AGENT_MEMORY_POSTGRES_URL:-}
POSTGRES_USER=memory
POSTGRES_DB=memory
POSTGRES_PASSWORD=${POSTGRES_PASSWORD:-}

AGENT_MEMORY_COORDINATION_BACKEND=$COORDINATION_BACKEND
AGENT_MEMORY_REDIS_URL=${AGENT_MEMORY_REDIS_URL:-}

AGENT_MEMORY_LLM_PROVIDER=$LLM_PROVIDER
AGENT_MEMORY_LLM_BASE_URL=${AGENT_MEMORY_LLM_BASE_URL:-https://api.openai.com/v1}
AGENT_MEMORY_LLM_API_KEY=${AGENT_MEMORY_LLM_API_KEY:-}
AGENT_MEMORY_LLM_MODEL=${AGENT_MEMORY_LLM_MODEL:-gpt-4o-mini}
AGENT_MEMORY_LLM_CLI_COMMAND=$LLM_CLI_COMMAND
AGENT_MEMORY_LLM_CLI_ARGS=$LLM_CLI_ARGS
AGENT_MEMORY_LLM_CLI_PROMPT_MODE=$LLM_CLI_PROMPT_MODE
ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY:-}
PROJECT_DIR=${PROJECT_DIR:-./workspace}

AGENT_MEMORY_EMBEDDING_PROVIDER=none
EOF
  )
  ok "Wrote $ENV_FILE (mode 600)"
}

start_services() {
  echo
  bold "Starting services${PROFILES[*]:+ (with ${PROFILES[*]})}…"
  docker compose up -d --build
  info "Waiting for memory-core to become healthy…"
  local waited=0
  until curl -fsS "http://127.0.0.1:$CORE_PORT/health" >/dev/null 2>&1; do
    sleep 3
    waited=$((waited + 3))
    if ((waited >= HEALTH_TIMEOUT_SECS)); then
      docker compose logs --tail 40 memory-core >&2 || true
      fail "memory-core did not become healthy within ${HEALTH_TIMEOUT_SECS}s (logs above)."
    fi
  done
  ok "memory-core is healthy."
}

create_admin() {
  ask INSTALL_ADMIN_NAME "First administrator's username" "${INSTALL_ADMIN_NAME:-admin}"
  local response
  response=$(curl -sS -X POST "http://127.0.0.1:$CORE_PORT/v3/internal/meta/user/init-admin" \
    -H "Authorization: Bearer $AGENT_MEMORY_API_KEY" \
    -H "x-memory-service-id: default" -H "content-type: application/json" \
    -d "{\"username\":\"$INSTALL_ADMIN_NAME\"}")
  if [[ $response == *'"code":0'* ]]; then
    ADMIN_KEY=$(sed -n 's/.*"user_key":"\([^"]*\)".*/\1/p' <<<"$response")
  elif [[ $response == *already_initialized* ]]; then
    info "An administrator already exists; keeping it."
  else
    warn "Could not create the administrator: $response"
  fi
}

print_summary() {
  echo
  bold "Agent Memory is running"
  info "Monitor Panel : http://localhost:$PANEL_PORT"
  info "memory-core   : http://localhost:$CORE_PORT  (Bearer token in $ENV_FILE)"
  info "Storage       : $STORAGE_BACKEND    Coordination: $COORDINATION_BACKEND    LLM: $INSTALL_LLM"
  if [[ -n ${ADMIN_KEY:-} ]]; then
    echo
    bold "Administrator login key (shown only once — store it safely):"
    info "$ADMIN_KEY"
  fi
  case $INSTALL_LLM in
    claude)
      [[ -z ${ANTHROPIC_API_KEY:-} ]] && info "Log in to Claude Code once: docker compose exec -it memory-core claude   (then /login)"
      ;;
    antigravity)
      info "Log in to Antigravity once: docker compose exec -it memory-core agy   (open the printed URL)"
      ;;
    codex)
      info "Log in to Codex once: docker compose exec -it memory-core codex login --device-auth"
      info "  (or with an API key: docker compose exec -T memory-core codex login --with-api-key < key-file)"
      ;;
    openai)
      [[ -z ${AGENT_MEMORY_LLM_API_KEY:-} ]] && warn "No LLM API key yet: memories are stored but not distilled until you add AGENT_MEMORY_LLM_API_KEY to $ENV_FILE and run 'docker compose up -d'."
      ;;
  esac
}

main() {
  for arg in "$@"; do
    case $arg in
      --non-interactive) INTERACTIVE=false ;;
      -h | --help) usage; exit 0 ;;
      *) fail "Unknown option: $arg (see --help)" ;;
    esac
  done

  bold "Agent Memory installer"
  require_docker
  if [[ -f $ENV_FILE && $INTERACTIVE == true ]]; then
    local reuse
    ask reuse "$ENV_FILE exists. Reuse it and just (re)start? (y/n)" y
    if [[ $reuse =~ ^[Yy] ]]; then
      read_env_file "$ENV_FILE"
      STORAGE_BACKEND=${AGENT_MEMORY_STORAGE_BACKEND:-sqlite}
      COORDINATION_BACKEND=${AGENT_MEMORY_COORDINATION_BACKEND:-local}
      case ${AGENT_MEMORY_LLM_CLI_COMMAND:-} in
        claude) INSTALL_LLM=claude ;;
        agy) INSTALL_LLM=antigravity ;;
        codex) INSTALL_LLM=codex ;;
        *) INSTALL_LLM=openai ;;
      esac
      start_services
      create_admin
      print_summary
      return
    fi
  fi

  PROFILES=()
  AGENT_MEMORY_API_KEY=$(random_secret)
  configure_storage
  configure_coordination
  configure_llm
  configure_ports
  write_env
  start_services
  create_admin
  print_summary
}

# upgrade.sh sources this file for its helpers.
if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
