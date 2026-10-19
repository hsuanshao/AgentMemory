#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────
# Agent Memory uninstaller. Removes what install.sh / docs/REMOTE-CLIENTS.md
# set up, and only that; anything not found is skipped.
#
#   ./uninstall.sh client [--yes] [--dry-run]
#       On a machine running agents: the Claude Code plugin (every scope)
#       and its marketplace, the Codex plugin and its marketplace, the
#       Antigravity plugin (user, and this directory's workspace copy), the
#       Hermes user plugin installed from clients/hermes, amem's settings,
#       offline queue and binary.
#
#   ./uninstall.sh server [--purge-data] [--yes] [--dry-run]
#       In the server's install directory (the one holding .env): stops and
#       removes the containers and the images they were built as. Memories
#       and CLI logins (Docker volumes) are kept unless --purge-data.
#
#   --yes        do not ask before each part
#   --dry-run    only print what would be removed
# ──────────────────────────────────────────────────────────────────────
set -euo pipefail

# Where the user ran it from: a workspace plugin install lives there.
START_DIR=$PWD
cd "$(dirname "$0")"
# shellcheck source=install.sh
source ./install.sh

ASSUME_YES=false
DRY_RUN=false
PURGE_DATA=false
BACKUP_ROOT="backups"

usage() {
  sed -n '3,20p' "$0" | sed 's/^# \{0,1\}//'
}

# run CMD… — execute, or only print with --dry-run
run() {
  if [[ $DRY_RUN == true ]]; then
    info "would run: $*"
  else
    "$@"
  fi
}

# remove PATH — delete a file or directory if present
remove() {
  [[ -e $1 || -L $1 ]] || return 0
  run rm -rf -- "$1"
  [[ $DRY_RUN == true ]] || ok "Removed $1"
}

confirm() {
  [[ $ASSUME_YES == true || $DRY_RUN == true ]] && return 0
  local answer
  read -r -p "  $1 (y/n) [n]: " answer
  [[ $answer =~ ^[Yy] ]]
}

# ── client ───────────────────────────────────────────────────────────

# Local and project scopes belong to a project, so Claude Code commands run
# in the directory the uninstaller was started from.
claude_in_start_dir() {
  (cd "$START_DIR" && claude "$@")
}

uninstall_claude_code() {
  command -v claude >/dev/null 2>&1 || { info "Claude Code: not installed, skipped."; return; }
  local scope
  for scope in local project user; do
    if [[ $DRY_RUN == true ]]; then
      info "would run (in $START_DIR): claude plugin uninstall agent-memory@agent-memory --scope $scope"
    elif claude_in_start_dir plugin uninstall agent-memory@agent-memory --scope "$scope" >/dev/null 2>&1; then
      ok "Claude Code: plugin removed ($scope scope)"
    fi
  done
  if [[ $DRY_RUN == true ]]; then
    info "would run (in $START_DIR): claude plugin marketplace remove agent-memory"
  elif claude_in_start_dir plugin marketplace remove agent-memory >/dev/null 2>&1; then
    ok "Claude Code: marketplace agent-memory removed"
  fi
  remove "$HOME/.claude/plugins/cache/agent-memory"
}

uninstall_codex() {
  if command -v codex >/dev/null 2>&1; then
    if [[ $DRY_RUN == true ]]; then
      info "would run: codex plugin remove agent-memory@agent-memory"
      info "would run: codex plugin marketplace remove agent-memory"
    else
      codex plugin remove agent-memory@agent-memory >/dev/null 2>&1 && ok "Codex: plugin removed"
      codex plugin marketplace remove agent-memory >/dev/null 2>&1 && ok "Codex: marketplace agent-memory removed"
    fi
  fi
  remove "${CODEX_HOME:-$HOME/.codex}/plugins/cache/agent-memory"
}

uninstall_antigravity() {
  if command -v agy >/dev/null 2>&1; then
    # `uninstall` succeeds even when nothing is installed; ask first.
    if ! agy plugin list --json 2>/dev/null | grep -q '"agent-memory"'; then
      :
    elif [[ $DRY_RUN == true ]]; then
      info "would run: agy plugin uninstall agent-memory"
    elif agy plugin uninstall agent-memory >/dev/null 2>&1; then
      ok "Antigravity: plugin removed"
    fi
  fi
  remove "$HOME/.gemini/config/plugins/agent-memory"
  # A workspace install lives in the project; check the directory the
  # uninstaller was started from.
  remove "$START_DIR/.agents/plugins/agent-memory"
  [[ $DRY_RUN == true ]] || rmdir "$START_DIR/.agents/plugins" "$START_DIR/.agents" 2>/dev/null || true
}

uninstall_hermes() {
  local plugin=${HERMES_HOME:-$HOME/.hermes}/plugins/agentmemory
  [[ -d $plugin ]] || return 0
  # Only the provider shipped in clients/hermes (it runs amem); an older
  # provider with the same name is the user's own and is left alone.
  if ! grep -q 'amem' "$plugin/plugin.yaml" 2>/dev/null; then
    warn "Hermes: $plugin is not the amem provider; left in place."
    return
  fi
  remove "$plugin"
  info "Hermes: if memory.provider is still agentmemory, pick another one: hermes config set memory.provider <name>"
}

uninstall_amem() {
  local config=${AMEM_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/agent-memory/client.json}
  local data=${AMEM_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/agent-memory}
  local queued=0
  [[ -d $data/queue ]] && queued=$(find "$data/queue" -maxdepth 1 -name '*.json' | wc -l)
  if ((queued > 0)); then
    warn "$queued capture(s) are still queued and will be lost. Run 'amem flush' first to send them."
    confirm "Delete them anyway?" || fail "Stopped; nothing else removed."
  fi
  remove "$config"
  if [[ -d $(dirname "$config") ]]; then
    rmdir "$(dirname "$config")" 2>/dev/null || true
  fi
  remove "$data"
  local bin
  bin=$(command -v amem || true)
  if [[ -n $bin ]]; then
    if [[ $bin == "${CARGO_HOME:-$HOME/.cargo}/bin/amem" ]] && command -v cargo >/dev/null 2>&1; then
      run cargo uninstall -q memory-client
      [[ $DRY_RUN == true ]] || ok "Removed $bin"
    else
      remove "$bin"
    fi
  fi
}

uninstall_client() {
  bold "Removing the Agent Memory client from this machine"
  confirm "Remove the Claude Code / Codex / Antigravity / Hermes integrations and amem?" || fail "Cancelled."
  uninstall_claude_code
  uninstall_codex
  uninstall_antigravity
  uninstall_hermes
  uninstall_amem
  echo
  if [[ $DRY_RUN == true ]]; then info "Dry run: nothing was changed."; return; fi
  ok "Client removed. The server and its memories are untouched."
}

# ── server ───────────────────────────────────────────────────────────

uninstall_server() {
  [[ -f $ENV_FILE ]] || fail "No $ENV_FILE here. Run this in the server's install directory."
  require_docker
  read_env_file "$ENV_FILE"
  local tag=${MEMORY_IMAGE_TAG:-local}
  bold "Removing the Agent Memory server in $PWD"
  info "Containers and the images memory-core:$tag / memory-panel:$tag are removed."
  if [[ $PURGE_DATA == true ]]; then
    warn "--purge-data: every memory, user, key and CLI login of this install is deleted for good."
    info "To keep a copy, back up first as in docs/UPGRADE.md, step B1."
    if [[ $ASSUME_YES == false && $DRY_RUN == false ]]; then
      local project answer
      project=$(basename "$PWD")
      read -r -p "  Type the directory name '$project' to confirm: " answer
      [[ $answer == "$project" ]] || fail "Cancelled; nothing removed."
    fi
  else
    info "Data volumes are kept: running ./install.sh here again brings everything back."
    confirm "Continue?" || fail "Cancelled."
  fi

  # Every profile, so optional services (postgres, redis) go too.
  local args=(--profile postgres --profile redis down --remove-orphans)
  [[ $PURGE_DATA == true ]] && args+=(--volumes)
  run docker compose "${args[@]}"
  local image
  for image in "memory-core:$tag" "memory-panel:$tag"; do
    docker image inspect "$image" >/dev/null 2>&1 || continue
    if [[ $DRY_RUN == true ]]; then
      info "would run: docker image rm $image"
    elif docker image rm "$image" >/dev/null 2>&1; then
      ok "Removed image $image"
    else
      warn "Image $image is still used by another container (another deployment with the same MEMORY_IMAGE_TAG?); kept."
    fi
  done
  # Images kept by upgrade.sh for a rollback, which purging rules out.
  if [[ $PURGE_DATA == true ]]; then
    local kept
    while read -r kept; do
      docker image inspect "$kept" >/dev/null 2>&1 || continue
      if [[ $DRY_RUN == true ]]; then
        info "would run: docker image rm $kept"
      elif docker image rm "$kept" >/dev/null 2>&1; then
        ok "Removed image $kept"
      fi
    done < <(cat "$BACKUP_ROOT"/*/images 2>/dev/null)
  fi
  echo
  if [[ $DRY_RUN == true ]]; then
    info "Dry run: nothing was changed."
  elif [[ $PURGE_DATA == true ]]; then
    ok "Server removed with its data. You can now delete this directory (it holds .env with the instance key)."
  else
    ok "Server stopped and removed. Data volumes kept; './uninstall.sh server --purge-data' deletes them."
  fi
}

main_uninstall() {
  local target=${1:-}
  [[ $# -gt 0 ]] && shift
  for arg in "$@"; do
    case $arg in
      --yes) ASSUME_YES=true ;;
      --dry-run) DRY_RUN=true ;;
      --purge-data) PURGE_DATA=true ;;
      -h | --help) usage; exit 0 ;;
      *) fail "Unknown option: $arg (see --help)" ;;
    esac
  done
  case $target in
    client) uninstall_client ;;
    server) uninstall_server ;;
    -h | --help | "") usage ;;
    *) fail "Unknown target: $target (client or server)" ;;
  esac
}

main_uninstall "$@"
