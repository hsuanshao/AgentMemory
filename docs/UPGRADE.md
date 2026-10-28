# Upgrading

[繁體中文](UPGRADE.zh-tw.md)

## Contents

- [To 0.5.0](#to-050)
- [To 0.4.3](#to-043)
- [To 0.4.2](#to-042)
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

## To 0.5.0

```bash
git fetch --tags && git checkout v0.5.0
./upgrade.sh
```

> [!WARNING]
> **Going back to 0.4.3 means restoring the backup** ([E. Roll back](#e-roll-back)). When it starts, memory-core runs schema migration 7. Nothing is converted, but 0.4.3 refuses data at schema version 7: it would apply a team's or a user's redaction rules to every team, and drop their level when it saved one.

**Check before upgrading.** Each row links to its section below.

| If … | Then … |
| --- | --- |
| `redaction.patterns` (`AGENT_MEMORY_REDACTION_PATTERNS`) holds patterns that should stay with administrators | Every member can now read them, with the rules for all teams ([who reads the rules](#who-reads-the-rules)) |
| A script calls `/v3/meta/redaction-rule/*` | Nothing to change: without `team_id` or `user_id` it works on the rules for all teams, as before; `list` answers with two more fields ([rules for a team or a user](#rules-for-a-team-or-a-user)) |
| People open Redaction under Metadata in the Panel | It has its own entry in the menu now |

### Rules for a team or a user

On top of the rules for all teams (the department's, added in 0.4.2), a team's admins add rules for their team, which apply to its members' writes and its shared scenes and persona, and each user adds their own, which apply to their writes in every team. A level only adds redaction. [DISTILLATION §7](ops/DISTILLATION.md#7-secrets-and-redaction) has the table of who changes and reads each level.

- `redaction-rule/list`, `create` and `test` take an optional `team_id` or `user_id`; `test` takes both for a user's writes in that team. `list` also returns `team` or `user` (the level asked for) and `can_edit`.
- Only the kinds of the rules for all teams are unique: a team or a user may use the same kind as another, so `[REDACTED:phone]` may come from any level.
- Existing rules stay rules for all teams.

### Who reads the rules

In 0.4.3 only system admins could list redaction rules. Now every member of the department reads the rules for all teams and the deployment's (`redaction.patterns`), a team's members its rules, and a user their own. Changing them stays with system admins (all teams), the team's admins (a team) and the user (their own). The Panel no longer stops other users before Core: Core checks each level.

### Also in this release

These need no action. The [CHANGELOG](../CHANGELOG.md#050---2026-10-28) has the details.

- **Panel languages:** English, Traditional Chinese, Simplified Chinese, French, Vietnamese, Spanish and German, from the browser's language or the menu ([Panel README](../panel/README.md#languages)).
- **`AGENT_MEMORY_DISTILLATION_LLM_TIMEOUT_MS`** sets `llm.timeoutMs` without editing the config file.
- **Worker `last_error`** shows only the error of a job that was dead-lettered.

## To 0.4.3

```bash
git fetch --tags && git checkout v0.4.3
./upgrade.sh
```

> [!WARNING]
> **Going back to 0.4.2 means restoring the backup** ([E. Roll back](#e-roll-back)). When it starts, memory-core runs schema migration 6: every stored user key is replaced by its SHA-256 digest. Existing keys keep working, but 0.4.2 refuses data at schema version 6 and could not check a digest anyway.

**Check before upgrading.** Each row links to its section below.

| If … | Then … |
| --- | --- |
| Clients on other machines reach memory-core or the Panel | Nothing changes on upgrade (`BIND_ADDR=0.0.0.0` is added to `.env`); consider HTTPS ([network](#where-the-ports-listen)) |
| memory-core sits behind a reverse proxy other than the bundled Panel and Caddy | Name it in `AGENT_MEMORY_TRUSTED_PROXIES`, or every client counts as the proxy's address ([refused keys](#refused-keys-are-throttled-and-audited)) |
| Many people share one address (NAT, a VPN gateway), or Docker shows every client with one address (rootless Docker) | 20 refused keys in 15 minutes lock that address, so everyone behind it, out for 15 minutes: check the addresses after upgrading, and raise or turn off the lock (`AGENT_MEMORY_THROTTLE_LOCK_AFTER`) ([refused keys](#refused-keys-are-throttled-and-audited)); for rootless Docker, decide before upgrading ([rootless Docker](#rootless-docker-every-client-shows-one-address)) |
| A script calls `internal/upgrade` with `issue_key` to get a fresh admin key | It is refused unless there is legacy data to move or `AGENT_MEMORY_ALLOW_KEY_RECOVERY=true` ([keys](#user-keys-are-stored-as-digests)) |
| A script creates users or the first admin with its own key (`user_key`) | Use `sk-mem-` and at least 32 letters, digits, `-` or `_` ([keys](#user-keys-are-stored-as-digests)) |
| A department has custom memory prompts (Panel → Prompts) that mention the input's old labels, such as `[New messages to extract]` | Refer to the JSON fields instead ([distillation input](#the-distillation-model-gets-its-input-as-json-data)) |

### User keys are stored as digests

Migration 6 replaces each stored user key by its SHA-256 digest and keeps its last four characters to show in the Panel. Nobody needs a new key: a key is checked by its digest. What changes:

- **A lost key cannot be read back** from the database (it never should have been). A system admin issues a new one in the Panel (Metadata → Users → Keys). If the only system admin lost theirs, the operator can set `AGENT_MEMORY_ALLOW_KEY_RECOVERY=true` in `.env`, restart, run `internal/upgrade` with `issue_key` ([B5](#b5-move-the-data-and-get-a-key)), and remove the setting again.
- **`internal/upgrade` issues a key only along with legacy data to move** (an install from before 0.1.1), or with `AGENT_MEMORY_ALLOW_KEY_RECOVERY=true`. Otherwise the server API key alone could mint a system admin key for any department. Every key issued this way, and every first admin `init-admin` creates, is recorded in the audit log.
- **A key chosen by the caller** (`init-admin` or `user/create-with-key` with `user_key`) must be `sk-mem-` followed by at least 32 letters, digits, `-` or `_`, as hard to guess as a generated one. Existing keys are not affected.

### Refused keys are throttled and audited

memory-core counts refused credentials (a wrong or revoked user key, a wrong server API key, a `meta/auth/verify` that answers `valid: false`) per client address. From the 5th in 15 minutes each refusal is answered later (250 ms, doubling up to 5 s); the 20th locks the address out for 15 minutes: every request from it gets 429 `too_many_failures` with `Retry-After`. Requests that succeed are not slowed. The limits are `server.throttle` in the config file ([memory-core.example.yaml](../config/memory-core.example.yaml)) or `AGENT_MEMORY_THROTTLE_DELAY_AFTER`, `AGENT_MEMORY_THROTTLE_LOCK_AFTER` and `AGENT_MEMORY_THROTTLE_LOCK_MINUTES`; 0 turns delays or locks off.

> [!WARNING]
> **A lockout refuses valid keys too.** It locks the address, not the key, so while it lasts everyone behind that address is refused, with the right key as well. Where many people share one address (an office NAT, a VPN gateway), anyone there can lock all of them out for 15 minutes by sending 20 wrong keys. That is the trade-off for stopping guesses at the address: if a shared address is more likely to meet a careless or hostile colleague than a key guesser, raise `AGENT_MEMORY_THROTTLE_LOCK_AFTER` or set it to 0, which keeps the delays and the audit log.

**Check that clients are told apart.** After upgrading, look at the addresses on Panel → Metadata → Audit (a wrong key typed once into the Panel puts one there). If every client shows the same internal address, such as the Docker network's gateway `172.x.0.1`, memory-core cannot tell them apart: rootless Docker's default port forwarding hides the client's address, and so does a NAT in front. Then one person's typos lock everyone out. Either let a proxy that sees the real address forward it (a reverse proxy trusted in `AGENT_MEMORY_TRUSTED_PROXIES`), or set `AGENT_MEMORY_THROTTLE_LOCK_AFTER=0` and rely on the delays and the audit log; for rootless Docker, see [below](#rootless-docker-every-client-shows-one-address). With Redis coordination the counts are shared by every replica, and deleting `<key_prefix>:auth:locked:<address>` (and `:auth:failures:<address>`) lifts a lock early; with local coordination they are kept in memory, and restarting memory-core lifts every lock.

The address is the connection's, or, when the connection comes from a trusted proxy, the client that proxy names in `X-Forwarded-For`. Compose trusts the Panel and Caddy (`AGENT_MEMORY_TRUSTED_PROXIES=memory-panel,caddy`), so a wrong key typed into the Panel counts against the browser's address, not the Panel's. **Behind another reverse proxy** (nginx, a load balancer), add its address or range, e.g. `AGENT_MEMORY_TRUSTED_PROXIES=memory-panel,caddy,10.0.0.0/8`; otherwise all clients share the proxy's address and one person's typos lock everyone out. Never list an address clients can connect from directly: whoever connects from it can name any client. memory-core reads every `X-Forwarded-For` header (a proxy may add its own instead of appending to the one it received) and drops a port a load balancer adds to a hop.

Every refusal, every lockout and every key issued with the server API key goes to the audit log: time, address, the `X-Forwarded-For` chain, user agent, department, path, and for a user key the first 8 characters of its digest (never the key). It lives beside the departments (`audit.db` in the data directory on SQLite, `agent_memory.audit_events` on Postgres), is kept 90 days (`AGENT_MEMORY_AUDIT_RETENTION_DAYS`), and survives `department/destroy`. A department's system admins see its events on Panel → Metadata → Audit (`/v3/audit/list`); the operator sees every department's, including names that do not exist, with `/v3/internal/audit/list` and the server API key ([API](API.md#credential-audit-log)).

#### Rootless Docker: every client shows one address

> [!IMPORTANT]
> **If Docker runs rootless, decide this before upgrading (or right after installing).** With rootless Docker's default port driver, memory-core sees every client, on this machine or another, as one internal address: the gateway of the rootless network, as Panel → Metadata → Audit shows it. The Compose `tls` profile does not change this, because Caddy runs inside the same Docker and sees that address too. With the default limits, 20 wrong keys from anyone in 15 minutes lock **everyone** out for 15 minutes.

Do one of these:

1. **Turn off the lock; keep the delays and the audit log.** Add `AGENT_MEMORY_THROTTLE_LOCK_AFTER=0` to `.env` and run `docker compose up -d`. Refusals are still answered later from the 5th on and still recorded; only refused requests wait, so other people are not slowed, and nobody is locked out.
2. **Put a reverse proxy on the host that sees the real address.** nginx or Caddy installed on the host itself (not in Docker) listens on the address clients use, forwards `/v3/` and `/health` to `127.0.0.1:8420` and the rest to `127.0.0.1:8125`, and sets `X-Forwarded-For` to the client's address (nginx: `proxy_set_header X-Forwarded-For $remote_addr;`). Then, in `.env`:
   - keep `BIND_ADDR=127.0.0.1`. Every connection through the published ports shows the same gateway address, so anything that reached them directly could name any client in `X-Forwarded-For`; only processes on this machine, the proxy among them, may reach them;
   - add that gateway address to the trusted proxies, e.g. `AGENT_MEMORY_TRUSTED_PROXIES=memory-panel,caddy,172.18.0.1` (use the address the audit log shows);
   - run `docker compose up -d`, type a wrong key into the Panel once from another machine, and check that the audit log shows that machine's address.

Switching rootless Docker to the `slirp4netns` port driver also keeps client addresses, but it changes every container on the host and is slower.

### Where the ports listen

`install.sh` now asks who reaches memory-core and the Panel: this machine only (ports on `127.0.0.1`, the default), other machines over plain HTTP (`0.0.0.0`), or other machines over HTTPS with Caddy (profile `tls`, [TLS](ops/TLS.md)). `docker-compose.yml` reads it from `BIND_ADDR`. **`upgrade.sh` adds `BIND_ADDR=0.0.0.0` to an existing `.env`, so the ports keep listening where they did.** User keys cross the network in plain text over HTTP; to stop that, set `BIND_ADDR=127.0.0.1` and follow [TLS](ops/TLS.md), then point the clients at the `https://` address with `amem login`.

### The distillation model gets its input as JSON data

L1 extraction and deduplication, L2 scenes and L3 persona now give the model one JSON object, as skill extraction did in 0.4.2: stored text is a JSON string, so a conversation cannot end the data or pose as the instructions. Each built-in prompt describes the fields (`new_messages`, `session_memories`, `candidate_pool`, `new_memories`, `existing_scenes`, `changed_scenes`, `existing_persona`, …) and says everything in them is data. A custom memory prompt that mentions the old text labels (`[Previous scene]`, `[New messages to extract]`, `### 1. New memories`, `## Changed scenes`) should name the JSON fields instead; one that only says what to remember needs nothing.

### Also in this release

These need no action. The [CHANGELOG](../CHANGELOG.md#043---2026-10-24) has the details.

- **Panel:** the session key is kept per browser tab (closing the tab signs out; a key saved by an earlier version is removed), a revoked key stops working in the Panel within 10 s, and the Panel sends a Content-Security-Policy.
- **`amem`** does not follow redirects, so a key never goes to another address: when a server moves to HTTPS, run `amem login` with the new address. Files it creates are readable by the owner only. A query after `--` is never read as an option.
- **Errors:** an internal or unavailable error answers with its code and the request id; the detail (paths, hosts) is only in memory-core's log.
- An agent's owner who left its team can no longer change the agent; a binding records who set it; `init-admin` creates one first admin even when called twice at once; name filters match `%` and `_` literally; tasks whose skill proposals all expired are closed hourly.

## To 0.4.2

```bash
git fetch --tags && git checkout v0.4.2
./upgrade.sh
```

`upgrade.sh` takes a backup, checks the configuration (`check-config`) before it replaces anything, and creates an admin if the install never had one.

> [!WARNING]
> **Going back to 0.4.1 means restoring the backup** ([E. Roll back](#e-roll-back)). When it starts, memory-core runs schema migration 4 (an index of the atoms without an embedding) and 5 (the table of department redaction rules). 0.4.1 refuses data at a newer schema version, and what this version writes would not be carried back.

**Check before upgrading.** This release tightens access, and some setups need a step. Each row links to its section below.

| If … | Then … |
| --- | --- |
| A Panel, script or client sends a department id that never ran `init-admin` | Run `init-admin` for it after upgrading ([departments](#departments-are-created-only-by-init-admin)) |
| Scripts or monitoring call `meta/*`, `pipeline/*`, `stats/overview`, … with only the server API key | Give them a system admin's user key, or use the `internal/…` routes ([server API key](#the-server-api-key-no-longer-opens-department-routes)) |
| The Panel runs outside Compose, or an override sets `CORE_API_KEY` for it | Remove `CORE_API_KEY` ([server API key](#the-server-api-key-no-longer-opens-department-routes)) |
| The server API key was handed to department admins | Change it after the upgrade ([server API key](#the-server-api-key-no-longer-opens-department-routes)) |
| The distillation CLI goes through a cloud provider (Bedrock, Vertex AI, Azure) or a wrapper script | List the variables it needs in `AGENT_MEMORY_LLM_CLI_PASS_ENV` ([CLI](#the-distillation-cli-no-longer-inherits-memory-cores-environment)) |
| You rely on extracted skills appearing by themselves | Approve them on Panel → Skills, or set `AGENT_MEMORY_SKILL_REQUIRE_APPROVAL=false` ([skills](#extracted-skills-wait-for-approval)) |
| Each person has their own agent, but the agents are team-visible (the default) | Set each to `"visibility": "private"` with `/v3/meta/agent/update`, so only its owner and the team's admins use it ([COLLABORATION §9](COLLABORATION.md#9-authorization-of-memory-requests)) |
| `.env` has a numeric `AGENT_MEMORY_*` value that does not parse | Fix it: memory-core now stops on it, and `upgrade.sh` reports it before replacing anything |

### Departments are created only by init-admin

A request that names a new department no longer creates it. It is refused:

- the data plane and department routes answer 401, the same as a wrong user key;
- Panel sign-in answers `valid: false`;
- operator routes answer 404 `unknown_department`.

Departments that already exist are not affected. If a Panel, script or client sends a department id that was never initialized, run `init-admin` for it after upgrading ([COLLABORATION §13](COLLABORATION.md#13-departments-and-storage)):

```bash
curl -sS -X POST "$CORE/v3/internal/meta/user/init-admin" -H "Authorization: Bearer $AGENT_MEMORY_API_KEY" \
  -H "x-memory-department-id: <department>" -H "content-type: application/json" -d '{"username":"admin"}'
```

### The server API key no longer opens department routes

**Calls that used only `Authorization: Bearer $AGENT_MEMORY_API_KEY` on `meta/*`, `pipeline/*`, `stats/overview`, `memory-generation-log/*`, `memory-prompt/*`, `knowledge/*` or `chat-memory/clear` get 401 after the upgrade.** These routes now take the user key of a user of the department, and the server API key does not stand in for one. This keeps each department's administration inside that department ([ARCHITECTURE](ARCHITECTURE.md#three-trust-zones)). Clients (`amem` and the plugins) are not affected: they already use user keys only.

| Routes | Before | From this version |
| --- | --- | --- |
| `meta/*`, `pipeline/status`, `pipeline/dead-letter/*`, `stats/overview`, `memory-generation-log/*` | Server API key (plus a user key on `meta/*`) | A user key of the department; the caller's role applies. The server API key is ignored |
| `memory-prompt/*`, `knowledge/*`, `chat-memory/clear` | Server API key | A user key of a **system admin** of the department |
| `internal/*`, `department/destroy` | Server API key | Server API key (unchanged) |
| `internal/pipeline/dead-letter/{list,redrive}`, `internal/memory-generation-log/{list,summary}` | (new) | Server API key: the operator's whole-department view |

What to do, by setup:

1. **Panel with Docker Compose (`install.sh` / `upgrade.sh`).** Nothing: `docker-compose.yml` no longer gives the Panel `CORE_API_KEY`, and the Panel forwards the signed-in user's key. If a `docker-compose.override.yml` or another env file sets `CORE_API_KEY` for `memory-panel`, remove it; the Panel logs a warning while it is set.
2. **Panel run on its own** (`docker run`, `npm start`). Remove `CORE_API_KEY` from its environment. `PANEL_REQUIRE_USER_KEY=false` no longer gives anonymous access: Core refuses calls without a user key.
3. **Your own scripts or monitoring** that call the routes above with the server API key only. Send `x-memory-user-key` with the key of a system admin instead; a dedicated user for automation (Panel → Metadata → Users) keeps it separate from people's keys. For the whole department's dead letters and generation logs, call the `internal/…` routes above with the server API key, or use `memory-server dead-letters` / `memory-server generations`.
4. **`memory-server generations` / `dead-letters`.** Nothing: they call the `internal/…` routes. Run them from the upgraded image; an older memory-core does not have those routes.
5. **Department admins who were given the server API key** to administer their department no longer need it, and should not keep it: it is a root credential. Whoever holds it can create the first admin of a new department (`init-admin`), issue a new key for any department's system admin (`internal/upgrade` with `issue_key`), list a department's users, read every department's dead letters and generation logs, and destroy departments. Change `AGENT_MEMORY_API_KEY` in `.env` after the upgrade and restart (`docker compose up -d`), so only the operator holds it. `upgrade.sh` and `install.sh` read it from `.env`.

Before upgrading, look for such calls in your scripts:

```bash
grep -rn "Authorization: Bearer" --include='*.sh' --include='*.py' . | grep -v "/v3/internal/\|department/destroy"
```

After upgrading, check both sides (`H_ADMIN` carries a system admin's user key, `x-memory-department-id` and `content-type`):

```bash
curl -sS -X POST "$CORE/v3/pipeline/status" -H "Authorization: Bearer $AGENT_MEMORY_API_KEY" \
  -H "x-memory-department-id: default" -d '{}'        # 401: the server key alone opens nothing here
curl -sS -X POST "$CORE/v3/pipeline/status" "${H_ADMIN[@]}" -d '{}'   # "code":0
```

This change stores nothing new, so going back to the previous version needs no restore on its account.

### The distillation CLI no longer inherits memory-core's environment

It gets `PATH`, `HOME`, locale, certificate and proxy variables and its own settings (`ANTHROPIC_*`/`CLAUDE_*` for claude, `OPENAI_*`/`CODEX_*` for codex, `GEMINI_*`/`GOOGLE_*` for agy). claude, codex and agy logged in as the install describes keep working. A CLI that read some other variable from the container's environment needs it listed in `AGENT_MEMORY_LLM_CLI_PASS_ENV` ([DISTILLATION §6](ops/DISTILLATION.md#what-the-cli-inherits)). **Check this before upgrading** if the CLI goes through a cloud provider (claude on Bedrock: `AWS_*`; claude on Vertex AI: `CLOUD_ML_REGION`, `GOOGLE_APPLICATION_CREDENTIALS`; codex on Azure: `AZURE_OPENAI_*`) or if `llm.cli.command` is a wrapper script: otherwise distillation fails after the upgrade.

### Extracted skills wait for approval

Skill extraction no longer saves skills by itself. What it creates or changes waits as a proposal on the Panel's **Skills** page (and `skill/proposal/*`) until the conversation's user (on an agent they may use), an admin of the team, or a system admin approves it. Nothing is lost: after the upgrade, new extractions simply show up there instead of in the skill list. Someone has to look at them, or no new skills appear. Proposals left undecided expire after 30 days (`AGENT_MEMORY_SKILL_PROPOSAL_TTL_DAYS`, 0 for never). A deployment that wants the old behaviour sets `AGENT_MEMORY_SKILL_REQUIRE_APPROVAL=false` (`skill.extraction.requireApproval: false`). The model also gets the conversation as JSON data and may change only the agent's skills it was shown, with or without approval ([ARCHITECTURE](ARCHITECTURE.md#4-memory-model-and-pipeline)).

### Also in this release

These need no action. The [CHANGELOG](../CHANGELOG.md#042---2026-10-24) has the details.

- **Secrets are redacted on the server**, on every write path, in many languages and by shape. Data stored before the upgrade is not rewritten. Department system admins can add their own rules (Panel → Metadata → Redaction).
- **Missing embeddings are filled in later.** On Postgres, migration 4 builds its index on each department's first use after the upgrade, and with a large `atoms` table that first request waits for it.
- **Recall output quotes stored text**, and a private agent is usable only by its owner, the team's admins and system admins.

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

Note `team_id`, `agent_id` and `user_key` from the answer. The key is shown only once, and it also logs you in to the Panel. Running the command again is harmless: there is nothing left to move, and the existing team and agent are reused. From 0.4.3, `issue_key` is refused once nothing is left to move, unless `AGENT_MEMORY_ALLOW_KEY_RECOVERY=true` is set ([To 0.4.3](#user-keys-are-stored-as-digests)); spare keys can be revoked in the Panel.

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
