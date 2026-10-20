# Upgrading

[繁體中文](UPGRADE.zh-tw.md)

## Contents

- [From 0.1.1 or later](#from-011-or-later)
- [From a version before 0.1.1](#from-a-version-before-011)
  - [What changes](#what-changes)
  - [Before you start](#before-you-start)
  - [A. Automatic: `upgrade.sh`](#a-automatic-upgradesh)
  - [B. Manual: the same steps by hand](#b-manual-the-same-steps-by-hand)
  - [C. Update the clients](#c-update-the-clients)
  - [D. Check the result](#d-check-the-result)
  - [E. Roll back](#e-roll-back)
  - [F. Troubleshooting](#f-troubleshooting)

## From 0.1.1 or later

Releases after 0.1.1 change neither the data format nor how clients authenticate. In the install directory (the one holding `.env`):

```bash
cd /path/to/agent-memory
git fetch --tags && git checkout v0.2.3   # or: git pull
docker compose up -d --build
```

`docker compose` reads `COMPOSE_PROFILES` and the rest of your settings from `.env`, so the same services come back. Check `curl -s http://127.0.0.1:8420/health`: `version` shows the new release. Clients keep working; update `amem` on each machine when convenient ([REMOTE-CLIENTS §4](REMOTE-CLIENTS.md#4-install-amem-on-each-client-machine)).

To go back, check out the previous tag and run the same `docker compose up -d --build`.

## From a version before 0.1.1

This part is for people running a server older than 0.1.1. Every step is a command you run yourself, and you don't need an AI agent for any of it. You can let `./upgrade.sh` do the whole thing ([A](#a-automatic-upgradesh)), or run the same steps by hand ([B](#b-manual-the-same-steps-by-hand)). Either way, finish with the clients ([C](#c-update-the-clients)).

## What changes

| | Before 0.1.1 | 0.1.1 |
| --- | --- | --- |
| Client authentication | The instance key (`Authorization: Bearer …`) | A per-user key (`x-memory-user-key: sk-mem-…`). The instance key is only for operations and admin tasks |
| Team / agent on writes | Optional. When left out, data went to a shared `default` bucket | Required. The user must be a member of the team, and the agent must belong to it |
| Old data | — | The `default` bucket is no longer served. Move its contents once with the upgrade |
| Clients | Your own scripts and plugins | The `amem` client and plugins for Claude Code, Codex, Antigravity and Hermes |

**This is a breaking change.** A client that sends only the Bearer key gets `401` after the upgrade, until you switch it over as described in section C.

## Before you start

- You need a shell on the server, inside the directory you installed into (the one holding `.env`). Docker Compose v2 and `curl` must be available there.
- Expect a few minutes of downtime: the server is stopped for the backup and then rebuilt.
- Stop or close the agents that write to the memory server (Hermes, Claude Code, Antigravity, scripts). Anything they send during the upgrade would land in the old format.
- Update the code:

  ```bash
  cd /path/to/agent-memory        # the directory with .env
  git fetch --tags && git checkout v0.2.3  # or: git pull
  ```

## A. Automatic: `upgrade.sh`

```bash
./upgrade.sh
```

The script runs the following steps. Each one is safe to repeat.

1. **Backup** to `backups/<timestamp>/`. This includes `.env`, the commit you had before the checkout (`previous-commit`), and an archive of memory-core's data volume. With Postgres it also takes a `pg_dump`; with the Redis profile, an archive of Redis's data. The running images are kept as `memory-core:pre-upgrade-<timestamp>` and `memory-panel:pre-upgrade-<timestamp>`. memory-core is stopped first, so the copy is consistent.
2. **Settings**: adds the settings that are new in this version to `.env`. Today that is `MEMORY_IMAGE_TAG=local`. Your other settings are not touched.
3. **Rebuild and start**: runs `docker compose up -d --build` and waits for `/health`.
4. **Move the old memories.** First it shows what it found and asks two questions:
   - *Team*: the default is `default-team`, the team the first install created.
   - *Agent*: the default is `default-agent-<admin>`. Give the name of the client you will mostly use, for example `hermes`. A missing team or agent is created.

   After you confirm, everything in the `default` bucket is moved into that team and agent, owned by the system admin. That covers conversations, memories, L2 scenes, the L3 persona and skills, plus the pipeline position, so nothing is distilled twice.
5. **Client settings**: issues a new user key for the admin, so it works even if you lost the one the first install printed. It prints the `amem login` command and writes everything to `upgrade-client-settings.txt` (mode 600).

To run without prompts, set the variables and pass `--non-interactive`:

```bash
UPGRADE_AGENT_NAME=hermes ./upgrade.sh --non-interactive
```

| Variable | Default | Meaning |
| --- | --- | --- |
| `UPGRADE_ADMIN_NAME` | the oldest system admin | Who owns the moved data and receives the new key |
| `UPGRADE_TEAM_NAME` | `default-team` | Team that receives the data; created if missing |
| `UPGRADE_AGENT_NAME` | `default-agent-<admin>` | Agent that receives the data; created if missing |
| `UPGRADE_SKIP_BACKUP` | `0` | `1` skips the backup. Only do this if you made your own backup |

Then continue with [C](#c-update-the-clients).

## B. Manual: the same steps by hand

All commands run in the install directory. Load the settings into your shell first:

```bash
source .env   # without set -a: docker compose must keep reading .env itself
CORE=http://127.0.0.1:${CORE_PORT:-8420}
H=(-H "Authorization: Bearer $AGENT_MEMORY_API_KEY" -H "x-memory-service-id: ${CORE_INSTANCE_ID:-default}" -H "content-type: application/json")
```

### B1. Back up

```bash
B=backups/$(date +%Y%m%d-%H%M%S); mkdir -p -m 700 "$B"
cp .env "$B/env"; git rev-parse 'HEAD@{1}' > "$B/previous-commit"   # the commit before your checkout
# Keep the running images, so a rollback needs no rebuild:
docker image tag memory-core:local "memory-core:pre-upgrade-${B##*/}"
docker image tag memory-panel:local "memory-panel:pre-upgrade-${B##*/}"
# Postgres in Docker (COMPOSE_PROFILES contains postgres):
docker compose exec -T postgres pg_dump -U "${POSTGRES_USER:-memory}" "${POSTGRES_DB:-memory}" > "$B/postgres.sql"
# Every backend: memory-core's data volume
docker compose stop memory-core
docker compose run --rm --no-deps -T --entrypoint tar memory-core czf - -C /data . > "$B/memory-core-data.tgz"
# Redis in Docker (COMPOSE_PROFILES contains redis):
docker compose stop redis
docker compose run --rm --no-deps -T --entrypoint tar redis czf - -C /data . > "$B/redis-data.tgz"
```

If you use your own Postgres server (`AGENT_MEMORY_POSTGRES_URL`), back it up with your usual tool, for example `pg_dump "$AGENT_MEMORY_POSTGRES_URL" > "$B/postgres.sql"`.

### B2. Add the new settings

Open `.env` and add any line from this list that is missing:

```bash
MEMORY_IMAGE_TAG=local   # tag of the images built by docker compose
```

If you run a second deployment from the same machine, give it a different tag so the two builds don't overwrite each other's images.

### B3. Rebuild and start

```bash
docker compose up -d --build
curl -fsS "$CORE/health"   # repeat until it answers
```

### B4. Look before moving (dry run)

```bash
curl -sS -X POST "$CORE/v3/internal/upgrade" "${H[@]}" \
  -d '{"dry_run": true, "agent_name": "hermes"}'
```

The answer shows the admin (`username`), `team_name`/`team_id` and `agent_name`/`agent_id`. An id of `null` means that team or agent will be created. `moved` lists how many rows are still in the old bucket. Nothing changes yet.

All body fields are optional: `admin_username`, `team_name` (default `default-team`), `agent_name` (default `default-agent-<admin>`), `issue_key`, `dry_run`.

> If the answer says `no active system admin`, the install never created one. Create it first:
> `curl -sS -X POST "$CORE/v3/internal/meta/user/init-admin" "${H[@]}" -d '{"username":"admin"}'`

### B5. Move the data and get a key

```bash
curl -sS -X POST "$CORE/v3/internal/upgrade" "${H[@]}" \
  -d '{"agent_name": "hermes", "issue_key": true}'
```

Note `team_id`, `agent_id` and `user_key` from the answer. The key is shown only once, and it also logs you in to the Panel. Running the command again is harmless: there is nothing left to move, and the existing team and agent are reused. Each run with `issue_key` adds another key, and you can revoke spare keys in the Panel.

You can also choose the exact target yourself. Create the team, user and agent in the Panel, then call:

```bash
curl -sS -X POST "$CORE/v3/internal/migrate/default-scope" "${H[@]}" \
  -d '{"team_id":"team-…","agent_id":"agt-…","user_id":"usr-…","dry_run":true}'   # then again with false
```

## C. Update the clients

Every client needs four values: the server URL, a **user key**, a **team id** and an **agent id**. The upgrade printed them, and `upgrade-client-settings.txt` has them too. To give other people their own keys, create users in the Panel and add them to the team. See [REMOTE-CLIENTS.md §3](REMOTE-CLIENTS.md#3-create-users-teams-and-agents-admin).

1. **Install `amem` and log in** on each machine that runs an agent, as in [REMOTE-CLIENTS.md §4](REMOTE-CLIENTS.md#4-install-amem-on-each-client-machine):

   ```bash
   cargo install --git <this repository> memory-client      # or from a checkout: cargo install --path crates/memory-client
   amem login --server-url http://SERVER:8420 --team-id team-… --agent-id agt-… --user-key -
   # paste the sk-mem-… key, press Enter
   ```

2. **Per client:**

   | Client | What to do |
   | --- | --- |
   | Hermes, with the old `agentmemory` provider | The old copy at `<hermes source>/plugins/memory/agentmemory` takes precedence over user plugins, so remove it first. Then install the new provider: [REMOTE-CLIENTS.md §8](REMOTE-CLIENTS.md#8-hermes-agent). Use the agent that received the old data so the old memories are recalled |
   | Claude Code | Install the plugin: [REMOTE-CLIENTS.md §5](REMOTE-CLIENTS.md#5-claude-code) |
   | Antigravity | Install the plugin: [REMOTE-CLIENTS.md §6](REMOTE-CLIENTS.md#6-antigravity) |
   | Codex CLI | Install the plugin and trust its hooks with `/hooks`: [REMOTE-CLIENTS.md §7](REMOTE-CLIENTS.md#7-codex-cli) |
   | Your own scripts | Keep the Bearer key for admin calls only. On data calls (`/v3/recall`, `/v3/conversation/add`, `/v3/session/add`, …), send `x-memory-user-key: sk-mem-…` plus `team_id` and `agent_id` in the body, or the `x-memory-team-id` / `x-memory-agent-id` headers. See [API.md](API.md) |

Each agent keeps its own scenes and persona; nothing is shared between agents. People who use the same agent share its scenes and persona, while their conversations and facts stay private. Give each client its own agent unless you want them to share one. See [MULTI-TENANCY.md](MULTI-TENANCY.md#2-what-is-private-and-what-is-shared).

## D. Check the result

```bash
amem status                      # configuration, and a queue size of 0
amem recall "something you told an agent before the upgrade"
```

In the Panel (`http://SERVER:8125`, log in with the user key), pick the team and agent. The old conversations and memories are listed there. On the server, `docker compose logs -f memory-core` should show no authorization errors once the clients are back.

## E. Roll back

Use the backup from step 1, for example `B=backups/20261019-101500`:

```bash
docker compose down                                   # keeps the volumes
git checkout "$(cat "$B/previous-commit")"
cp "$B/env" .env
# Bring back the previous images (listed in $B/images). Without them, the final
# step needs `docker compose up -d --build` to rebuild the old version.
docker image tag "memory-core:pre-upgrade-${B##*/}" memory-core:local
docker image tag "memory-panel:pre-upgrade-${B##*/}" memory-panel:local
docker compose run --rm --no-deps -T --entrypoint sh memory-core \
  -c 'find /data -mindepth 1 -delete && tar xzf - -C /data' < "$B/memory-core-data.tgz"
# Redis profile:
docker compose run --rm --no-deps -T --entrypoint sh redis \
  -c 'find /data -mindepth 1 -delete && tar xzf - -C /data' < "$B/redis-data.tgz"
# Postgres profile:
docker compose up -d postgres
docker compose exec -T postgres dropdb -U "${POSTGRES_USER:-memory}" --force "${POSTGRES_DB:-memory}"
docker compose exec -T postgres createdb -U "${POSTGRES_USER:-memory}" "${POSTGRES_DB:-memory}"
docker compose exec -T postgres psql -q -U "${POSTGRES_USER:-memory}" "${POSTGRES_DB:-memory}" < "$B/postgres.sql"
# then
docker compose up -d
```

Once you are sure you won't roll back, remove the kept images with `docker image rm memory-core:pre-upgrade-… memory-panel:pre-upgrade-…`.

Point the clients back at their old configuration as well.

## F. Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `Agent '…' already has L2/L3 memories` / `409 target_has_profile` | Old data was found, but the chosen agent already has its own scenes, and the two sets are never merged. Choose a new agent name and run again |
| `no active system admin` | The install never created an admin. `upgrade.sh` creates one. By hand, use the `init-admin` call in B4 |
| Clients get `401` | They still send only the Bearer key. See C |
| Clients get `403 not a member` | The user is not in the team. Add them in the Panel, under Teams → members |
| `upgrade.sh`: `No .env here` | Run it from the existing install directory, not from a fresh clone |
| Lost the key the upgrade printed | It is in `upgrade-client-settings.txt`. Or run `./upgrade.sh` again: nothing moves twice, and a new key is issued |
| The Claude/Antigravity/Codex distillation login is gone | It is stored in the `claude-config` / `antigravity-config` / `codex-config` volumes, which the upgrade keeps. If you ran `docker compose down -v`, log in again: `docker compose exec -it memory-core agy` (or `claude`, or `codex login --device-auth`) |
