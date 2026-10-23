# Upgrading

[繁體中文](UPGRADE.zh-tw.md)

## Contents

- [To 0.4.1](#to-041)
- [To 0.4.0](#to-040)
- [To 0.3.0](#to-030)
- [Within 0.1.1 – 0.2.x](#within-011--02x)
- [From a version before 0.1.1](#from-a-version-before-011)
  - [What changes](#what-changes)
  - [Before you start](#before-you-start)
  - [A. Automatic: `upgrade.sh`](#a-automatic-upgradesh)
  - [B. Manual: the same steps by hand](#b-manual-the-same-steps-by-hand)
  - [C. Update the clients](#c-update-the-clients)
  - [D. Check the result](#d-check-the-result)
  - [E. Roll back](#e-roll-back)
  - [F. Troubleshooting](#f-troubleshooting)

## To 0.4.1

```bash
git fetch --tags && git checkout v0.4.1
./upgrade.sh
```

**Scenes and personas move to new tables.** Schema migration 3 runs when memory-core starts. It copies every version in `profile_files` into `scenes`, `scene_versions` and `persona_versions`, which are append-only from now on. `profile_files` stays, but nothing writes to it anymore. Clients see the same scenes and persona, and the API paths are unchanged.

> [!WARNING]
> **Going back to 0.4.0 means restoring the backup** ([E. Roll back](#e-roll-back)). 0.4.0 still reads `profile_files`, so on upgraded data it would show the scenes and persona from before the upgrade, and what it wrote would not be carried over by a later upgrade, whose migration is already recorded. Restoring the backup also undoes every other change made after the upgrade, as with any rollback.

**From this version on, memory-core refuses data written by a newer version.** It checks SQLite `user_version`, Postgres `schema_migrations` and Redis `schema_version`; if the data is newer, it stops with an error that says to run the newer version or restore the backup. 0.4.0 and earlier do not have this check, so take care when going back to them.

## To 0.4.0

```bash
git fetch --tags && git checkout v0.4.0
./upgrade.sh
```

**License:** from 0.4.0 the server is under the Business Source License 1.1; the `amem` client and the plugins stay MIT. Check whether your use is within the free tiers in [COMMERCIAL.md](../COMMERCIAL.md). No data migration is needed.

> [!WARNING]
> **memory-core no longer starts when `AGENT_MEMORY_LLM_CLI_ARGS` (`llm.cli.args`) gives the distillation CLI more capability than its defaults.** Check your `.env` before you upgrade.

Distillation reads untrusted conversation text, so its model never gets a tool (#37). These arguments are now refused, and the error names the one it found:

- `--dangerously-skip-permissions`, `--allow-dangerously-skip-permissions`, `--dangerously-bypass-approvals-and-sandbox` and any other `--dangerously-…`;
- `--yolo`, `--full-auto`, `--allowedTools` / `--allowed-tools`, `--mcp-config`, `--permission-prompt-tool`;
- `--permission-mode` other than `default` or `plan`;
- claude: `--tools` with a list of tools;
- codex: arguments without `exec`; `--enable …`, `--search`, `-p` / `--profile`, `-s` / `--sandbox` other than `read-only`, and these `-c` settings: `mcp_servers.…`, `features.…=true`, `web_search` other than `disabled`, `sandbox_mode` other than `read-only`, `sandbox_permissions`, `approval_policy`, `profile`. Short options count with their value attached too (`-cKEY=VALUE`, `-sMODE`, `-pNAME`). Settings such as `-c model_reasoning_effort=high` or `-c service_tier=fast` are fine.

Remove them. Distillation does not need any of them. A CLI that kept failing because a tool was refused now drops that input instead (0.3.2).

**`upgrade.sh` checks this for you.** After the backup it builds the new version and runs `memory-server check-config` with your settings, before it starts anything new. If the configuration is refused, or anything else fails before the new version answers `/health` (the build, or a new version that does not start), it prints the error, puts `.env` back as it was, runs the previous version again on its previous image, and stops. If the new version had already migrated stored data before failing, also restore the data from the backup ([E. Roll back](#e-roll-back)). To check by hand after `git checkout` of the new version:

```bash
docker compose build memory-core
docker compose run --rm --no-deps -T memory-core check-config
```

**Also new:** with `claude` or `codex`, memory-core now turns off the CLI's own tools before distilling (`AGENT_MEMORY_LLM_CLI_HARDENING=auto`, the default). **The distillation codex no longer reads `config.toml`:** move settings it needs from there into `AGENT_MEMORY_LLM_CLI_ARGS` (`-c key=value`). See [DISTILLATION §6](ops/DISTILLATION.md#6-tools-and-distillation).

## To 0.3.0

0.3.0 renames the **instance** to the **department**: one server holds departments, a department holds teams, and a team holds members and agents. The new name reaches the API, the settings and the stored data:

| | Before 0.3.0 | 0.3.0 |
| --- | --- | --- |
| Request header | `x-memory-service-id` | `x-memory-department-id` |
| `amem` | `service_id`, `AMEM_SERVICE_ID`, `--service-id` | `department_id`, `AMEM_DEPARTMENT_ID`, `--department-id`. `client.json` is rewritten on first use |
| Claude Code plugin option | Service ID | Department ID |
| `.env` | `CORE_INSTANCE_ID` | `CORE_DEPARTMENT_ID` |
| `memory-core.yaml` | `maxOpenInstances`, `maxUsersPerInstance`, `maxTeamsPerInstance` | `maxOpenDepartments`, `maxUsersPerDepartment`, `maxTeamsPerDepartment`. memory-core refuses to start with an old name |
| API | `/v3/instance/destroy` `{instance_id}`, `/v3/meta/instance-quota/get`, `/v3/internal/meta/user/list-by-instance` | `/v3/department/destroy` `{department_id}`, `/v3/meta/department-quota/get`, `/v3/internal/meta/user/list-by-department` |
| `memory-server` commands | `--instance` | `--department` |
| Team roles | `admin`, `member`, `reviewer` | `admin`, `leader`, `member`. Reviewers become members; a leader sees the whole team's pipeline state, generation logs and dead letters ([COLLABORATION §12](COLLABORATION.md#12-operational-state-by-role)) |
| Operational state with a user key | The whole department | What the user's role allows. Redriving dead letters needs a system admin |
| Stored data | `<data>/instances`, Postgres `agent_memory.instances`, Redis `…:pipe:inst:…` | Migrated by memory-core when it starts ([what changes](ops/SCHEMA-MIGRATIONS.md#3-migrations-so-far)) |

**Update the server and every client together.** A client older than 0.3.0 gets `401` with "x-memory-service-id was renamed in 0.3.0; update the client".

1. Stop the agents that write to the server. If you run several memory-core replicas, stop all of them: an older replica cannot read migrated data.
2. Update the code and run the upgrade script from the install directory:

   ```bash
   cd /path/to/agent-memory
   git fetch --tags && git checkout v0.4.0
   ./upgrade.sh
   ```

   `upgrade.sh` backs up the data, renames `CORE_INSTANCE_ID` in `.env`, then rebuilds and starts the services. memory-core migrates the stored data as it starts.

   To do it by hand instead:
   1. Back up as in [B1](#b1-back-up).
   2. Rename `CORE_INSTANCE_ID` to `CORE_DEPARTMENT_ID` in `.env`, and the three settings above if your `memory-core.yaml` sets them.
   3. Run:

      ```bash
      docker compose build
      docker compose run --rm -T memory-core migrate   # optional: migrate and report before starting
      docker compose up -d
      ```
3. Update `amem` on every client machine ([REMOTE-CLIENTS §4](REMOTE-CLIENTS.md#4-install-amem-on-each-client-machine)). In Claude Code, if you had set **Service ID**, set **Department ID** to the same value.
4. Check:
   - `curl -s http://127.0.0.1:8420/health` shows the new `version`;
   - `docker compose logs memory-core | grep -i migrat` lists the steps that ran;
   - on a client, `amem status` shows the expected `department_id` and `amem recall test` answers without an error.

The migration only goes forward. To return to 0.2.x, restore the backup as in [E](#e-roll-back).

## Within 0.1.1 – 0.2.x

Releases from 0.1.1 to 0.2.x change neither the data format nor how clients authenticate. In the install directory (the one holding `.env`):

```bash
cd /path/to/agent-memory
git fetch --tags && git checkout v0.2.3   # or: git pull
docker compose up -d --build
```

`docker compose` reads `COMPOSE_PROFILES` and the rest of your settings from `.env`, so the same services come back. Check `curl -s http://127.0.0.1:8420/health`: `version` shows the new release. Clients keep working; update `amem` on each machine when convenient ([REMOTE-CLIENTS §4](REMOTE-CLIENTS.md#4-install-amem-on-each-client-machine)).

To go back, check out the previous tag and run the same `docker compose up -d --build`.

## From a version before 0.1.1

This part is for people running a server older than 0.1.1. It also applies when going straight to 0.3.0: `upgrade.sh` does both upgrades in one run. Every step is a command you run yourself, and you don't need an AI agent for any of it. You can let `./upgrade.sh` do the whole thing ([A](#a-automatic-upgradesh)), or run the same steps by hand ([B](#b-manual-the-same-steps-by-hand)). Either way, finish with the clients ([C](#c-update-the-clients)).

## What changes

| | Before 0.1.1 | 0.1.1 |
| --- | --- | --- |
| Client authentication | The server API key (`Authorization: Bearer …`) | A per-user key (`x-memory-user-key: sk-mem-…`). The server API key is only for operations and admin tasks |
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
  git fetch --tags && git checkout v0.4.0  # or: git pull
  ```

## A. Automatic: `upgrade.sh`

```bash
./upgrade.sh
```

The script runs the following steps. Each one is safe to repeat.

1. **Backup** to `backups/<timestamp>/`. This includes `.env`, the commit you had before the checkout (`previous-commit`), and an archive of memory-core's data volume. With Postgres it also takes a `pg_dump`; for your own server it uses the `pg_dump` of that server's major version (`postgres:<major>-alpine`), since an older one refuses a newer server; with the Redis profile, an archive of Redis's data. The running images are kept as `memory-core:pre-upgrade-<timestamp>` and `memory-panel:pre-upgrade-<timestamp>`. memory-core is stopped first, so the copy is consistent.
2. **Settings**: renames settings that changed name (0.3.0: `CORE_INSTANCE_ID` → `CORE_DEPARTMENT_ID`) and adds the ones that are new (`MEMORY_IMAGE_TAG=local`) to `.env`. Your other settings are not touched.
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
H=(-H "Authorization: Bearer $AGENT_MEMORY_API_KEY" -H "x-memory-department-id: ${CORE_DEPARTMENT_ID:-default}" -H "content-type: application/json")
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

> If the answer says `no active system admin` or `unknown_department`, the install never created one. Create it first:
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

Each agent keeps its own scenes and persona; nothing is shared between agents. People who use the same agent share its scenes and persona, while their conversations and facts stay private. Give each client its own agent unless you want them to share one. See [COLLABORATION.md](COLLABORATION.md#2-what-is-private-and-what-is-shared).

## D. Check the result

```bash
amem status                      # configuration, and a queue size of 0
amem recall "something you told an agent before the upgrade"
```

In the Panel (`http://SERVER:8125`, log in with the user key), pick the team and agent. The old conversations and memories are listed there. On the server, `docker compose logs -f memory-core` should show no authorization errors once the clients are back.

## E. Roll back

Going back always means restoring the data too: a migrated store cannot be read correctly by an earlier version, and versions after 0.4.0 refuse to start on data newer than they know. Everything written after the upgrade is lost.

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
| `no active system admin` or `unknown_department` | The install never created an admin. `upgrade.sh` creates one. By hand, use the `init-admin` call in B4 |
| Clients get `401` | They still send only the Bearer key. See C |
| Clients get `403 not a member` | The user is not in the team. Add them in the Panel, under Teams → members |
| `upgrade.sh`: `No .env here` | Run it from the existing install directory, not from a fresh clone |
| Lost the key the upgrade printed | It is in `upgrade-client-settings.txt`. Or run `./upgrade.sh` again: nothing moves twice, and a new key is issued |
| The Claude/Antigravity/Codex distillation login is gone | It is stored in the `claude-config` / `antigravity-config` / `codex-config` volumes, which the upgrade keeps. If you ran `docker compose down -v`, log in again: `docker compose exec -it memory-core agy` (or `claude`, or `codex login --device-auth`) |
