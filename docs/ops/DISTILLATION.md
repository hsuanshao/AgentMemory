# Watching and recovering distillation

[繁體中文](DISTILLATION.zh-tw.md)

memory-core distils conversations in the background: L1 extracts memories, L2 folds them into scenes, L3 rewrites the persona. Each step calls the configured LLM. This page covers how to see whether that works, and what to do when it does not.

## Contents

- [1. What is recorded](#1-what-is-recorded)
- [2. Where to look](#2-where-to-look)
- [3. Common failures](#3-common-failures)
- [4. Changing the model](#4-changing-the-model)
- [5. Dead letters and redrive](#5-dead-letters-and-redrive)
- [6. Tools and distillation](#6-tools-and-distillation)
- [7. Secrets and redaction](#7-secrets-and-redaction)
- [8. Embeddings and load](#8-embeddings-and-load)
- [9. Scenes and their atoms](#9-scenes-and-their-atoms)

## 1. What is recorded

Every L1, L2 and L3 run that reaches the LLM leaves a generation log:

| Status | Meaning |
| --- | --- |
| `succeeded` | It wrote memories, scenes or the persona |
| `empty` | The LLM answered but nothing was written: nothing worth keeping, or an answer that changed nothing. Many `empty` runs in a row can mean a model that answers badly |
| `failed` | The call failed (quota, timeout, no output) or its answer could not be used. `error_message` says why |
| `dropped` | Like `failed`, on the **last** attempt of an L1 or L2 run: the input was skipped so the session or agent could go on (below) |

Each log also records the model. For a CLI provider that is the command plus the model its arguments select, for example `cli:agy:gemini-3.8-flash-high`.

A failed job is retried after 5, 15 and 45 seconds (`worker.maxRetries`, default 3). A failure that would happen again is not retried: the model tried to use a tool, which distillation never allows (see #37). An answer that is not the step's JSON is retried, because the next one may be fine. If every attempt fails, or the failure would repeat, the job becomes a **dead letter**, kept with its last error and the model it failed with. Section 5 explains what happens to dead letters.

**L1 and L2 drop what they cannot distil.** L1 works through a session and L2 through an agent's atoms, each from a cursor that moves only on success. A batch that fails every attempt would otherwise block everything after it, because each later run starts with the same batch again. So on the last attempt:
- the cursor moves past the batch, and later turns or atoms are distilled as usual;
- the run is logged as `dropped`, with the error and the ids of its input (`input_refs`);
- the dead letter keeps a job limited to exactly that input (`range: {first, last}`, storage sequence numbers). Redriving it redoes only that batch and leaves the cursor alone.

Nothing is deleted: the conversations stay in L0 and the atoms in L1. L3 and skill jobs have no cursor, so a failure there blocks nothing and is not dropped.

| Coordination backend | Where dead letters live |
| --- | --- |
| `redis` | In Redis, the newest 10,000, shared by every replica |
| `local` | In memory, the newest 1,000. They are lost on restart |

## 2. Where to look

**Panel → Dashboard → Distillation**:
- the LLM in use;
- succeeded, empty and failed runs per layer over the last 24 hours, with the failure rate;
- the latest failures with their errors;
- the dead letters per job kind.

System admins also get a button to re-run dead letters. **Panel → Generation logs** lists every run; filter by status (`#/logs?status=failed`) and open one for its error, model, inputs and outputs.

**On the server**, from the install directory:

```bash
# Runs per layer and status in the last 24 h, then the latest failed runs
docker compose exec memory-core memory-server generations
docker compose exec memory-core memory-server generations --status empty --layer l1 --hours 6

# Dead letters, newest first
docker compose exec memory-core memory-server dead-letters
docker compose exec memory-core memory-server dead-letters --kind l1 --json
```

The commands call the running server's API, so they work the same on every storage and coordination backend. `--department` selects a department other than `default`. The API endpoints behind them are in [API.md](../API.md#health-and-monitoring).

## 3. Common failures

| Error (in `error_message` or the dead letter) | Likely cause | What to do |
| --- | --- | --- |
| `RESOURCE_EXHAUSTED`, `quota`, `429` | The LLM account ran out of quota | Wait for the reset, or switch to a model with its own quota (section 4). Then redrive |
| `` `agy` produced no output; stderr: … `` | The CLI exited cleanly but printed no answer; what it wrote to stderr follows | Often transient and retried. If one batch keeps failing, read the stderr part, or try another model |
| `` `…` timed out after … `` | The answer took longer than `llm.timeoutMs` | Raise the timeout, or use a faster model |
| `llm_output_invalid` | The answer was not the JSON the step asks for | A model that follows instructions poorly; switch models |
| `LLM returned 5xx` | The OpenAI-compatible endpoint failed | Check the endpoint; retries usually cover short outages |
| `LLM_TOOL_REQUEST: …` (older letters: `… auto-denied …`) | The model tried to use a tool instead of answering (section 6) | Nothing; the input is dropped and not retried. If it happens often, use a CLI whose tools memory-core turns off, or another model |
| `Unknown feature flag: …` | A newer `codex` renamed or removed a feature that hardening turns off (section 6) | Update memory-core, or set `AGENT_MEMORY_LLM_CLI_HARDENING=off` until it is updated |

## 4. Changing the model

For a CLI provider, the model is part of the arguments in `.env`:

```bash
AGENT_MEMORY_LLM_CLI_ARGS=["-p","{prompt}","--model","gemini-3.8-flash-high"]
```

For an OpenAI-compatible provider it is `AGENT_MEMORY_LLM_MODEL`. Before switching, check that the model answers and has quota, for example `docker compose exec memory-core agy -p "Reply OK" --model gemini-3.8-flash-high`. Then apply it with `docker compose up -d memory-core`. Jobs that were running are delivered again after the restart.

## 5. Dead letters and redrive

**Dead letters look after themselves.** Every 5 minutes a sweeper goes through them:

| A dead letter… | What happens |
| --- | --- |
| first failed more than a day ago, or was already redriven 3 times | It is **removed**. Its failures stay in the generation logs (`failed` / `dropped`, with the error and the input ids), so nothing about it is lost but the redo handle |
| failed for a passing reason (quota, timeout, outage, an invalid answer) | It is **redriven** on its own 1 h after it died, then 2 h, then 4 h. At most 10 per sweep |
| failed in a way that would repeat (tool refused) | It waits until the configured model changes, then is redriven like the others; with the same model it would only fail again. A letter written before this was recorded is taken to have failed with the current model |

Each redrive, by hand or automatic, counts towards the 3. Settings (`memory-core.yaml`, section `worker`): `deadLetterTtlHours` (24), `deadLetterMaxRedrives` (3), `deadLetterRedriveAfterMinutes` (60), `deadLetterSweepSeconds` (300), `deadLetterRedriveBatch` (10). The Panel shows each letter's re-runs; `/health` counts `dead_letters_redriven` and `dead_letters_expired`.

- **Letters from before 0.2.3** carry no time, so they count as older than a day and are removed by the first sweep after an upgrade. Redrive any you still want before upgrading.
- **A redrive by hand is an admin's override:** it is allowed even for a letter already redriven 3 times. That counts as one more redrive, so if it fails again the next sweep removes it.

Once the cause is fixed, you can redrive sooner by hand:

```bash
docker compose exec memory-core memory-server dead-letters redrive --limit 10
docker compose exec memory-core memory-server dead-letters redrive --kind skill --limit 20
```

The Panel's **Re-run** button does the same, 10 at a time. A redriven job starts again as a first attempt, oldest first. With several replicas, each dead letter is redriven by one of them only.

Redrive in small batches and watch the failures between them: a backlog sent at once can use up an LLM quota again. Redriving is safe to repeat:
- An L1 job continues from its session's cursor, so it never processes the same messages twice.
- A job with a `range` (dropped input) redoes only that batch. If it fails again it comes back as the same dead letter and is not dropped again.
- L2, L3 and skill jobs work on the current state of their agent or task.

Find what was dropped and why with **Panel → Generation logs** (`#/logs?status=dropped`) or `memory-server generations --status dropped`. The dashboard's Distillation card counts dropped runs per layer.

## 6. Tools and distillation

> [!WARNING]
> **memory-core does not start when `AGENT_MEMORY_LLM_CLI_ARGS` gives the CLI more capability than its defaults**, whatever `AGENT_MEMORY_LLM_CLI_HARDENING` says. The error names the argument; remove it. See [Arguments that are refused](#arguments-that-are-refused) and [UPGRADE](../UPGRADE.md#to-040).

Distillation reads conversation text, which is untrusted: anyone in a conversation can write instructions into it. So the distillation model never gets a tool or a skipped permission check (#37). Instead:

- a known agent CLI has its own tools turned off before it reads a prompt;
- every CLI is told in the prompt that it has no tools;
- a model that still tries to use a tool fails with `LLM_TOOL_REQUEST`. That failure is not retried: L1/L2 drop the input, and its dead letter waits until the model changes ([section 5](#5-dead-letters-and-redrive));
- skill extraction gets the conversation as JSON data, may change only the skills it was shown, and only proposes: its changes wait for the conversation's user (on an agent they may use), an admin of the team, or a system admin to approve them (Panel → Skills, or `skill/proposal/*`). `skill.extraction.requireApproval: false` (`AGENT_MEMORY_SKILL_REQUIRE_APPROVAL=false`) applies them at once ([ARCHITECTURE](../ARCHITECTURE.md#4-memory-model-and-pipeline)).

### Agent CLIs

`llm.cli.hardening: auto` is the default (`AGENT_MEMORY_LLM_CLI_HARDENING`). memory-core puts these arguments in front of `AGENT_MEMORY_LLM_CLI_ARGS`. They only remove capability:

| CLI | Added arguments | Effect |
| --- | --- | --- |
| `claude` | `--tools "" --strict-mcp-config --disable-slash-commands` | No built-in tools, no MCP servers from settings, no skills or slash commands |
| `codex` | `--disable` for `apps`, `plugins`, `remote_plugin`, `shell_tool`, `unified_exec`, `code_mode_host`, `multi_agent`, `goals`, `skill_search`, `tool_suggest`, `browser_use`, `browser_use_external`, `computer_use`, `in_app_browser`, `image_generation`; `-c web_search=disabled`; `-c sandbox_mode="read-only"`; and `--ignore-user-config` right after `exec` | No connectors, plugins, shell or code execution, sub-agents, browsing or web search. `config.toml` is not read, so its MCP servers and profiles are not loaded |
| `agy` and others | nothing | No known way to turn the tools off. A tool call is refused in headless mode, and it fails with `LLM_TOOL_REQUEST` |

At startup memory-core logs which arguments it added, or warns that it could not add any.

**Check it.** Ask the CLI in the container how many tools it has, with the same arguments:

```bash
docker compose exec -T memory-core sh -c 'echo "How many tools do you have? Reply with just the number." | claude --tools "" --strict-mcp-config --disable-slash-commands -p'
```

The answer should be `0`. For `codex`, a refused attempt shows up in its output as `code-mode host is disabled`, and the run still answers.

**codex and `config.toml`.** The distillation codex does not read `$CODEX_HOME/config.toml`; it still signs in from `CODEX_HOME`. Put the settings it needs in `AGENT_MEMORY_LLM_CLI_ARGS`, e.g. `-c model=…` or `-c model_reasoning_effort=high`. On codex 0.163 `unified_exec` cannot be turned off; `shell_tool`, which carries the command tools, is off. With hardening on, codex arguments without `exec` are refused, since only `exec` can skip `config.toml`.

**`hardening: off`** passes the arguments as given and logs a warning. Use it only to get past a CLI version whose flag names changed, until memory-core is updated. Refused arguments stay refused.

### What the CLI inherits

The CLI reads untrusted conversation text, so it does not get memory-core's environment. It inherits only:

- what any program needs: `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LANGUAGE`, `LC_*`, `TZ`, `TMPDIR`, `TERM`, `XDG_*_HOME`, `XDG_RUNTIME_DIR`, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `NODE_EXTRA_CA_CERTS` and the proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, `ALL_PROXY`, either case);
- the settings of the CLI in use: `ANTHROPIC_*` and `CLAUDE_*` for `claude`, `OPENAI_*` and `CODEX_*` for `codex`, `GEMINI_*` and `GOOGLE_*` for `agy` and `gemini`;
- what `llm.cli.passEnv` (`AGENT_MEMORY_LLM_CLI_PASS_ENV`, comma- or space-separated) names: exact names, or `PREFIX_*`.

`llm.cli.env` sets variables on top. `AGENT_MEMORY_*` (the API key, database and Redis URLs, the LLM and embedding keys) never reaches the CLI; a `passEnv` entry that would match it stops memory-core at startup. Another CLI that needs a key, say `MY_LLM_TOKEN`, gets it with `AGENT_MEMORY_LLM_CLI_PASS_ENV=MY_LLM_TOKEN`. With Docker Compose the variable must also reach the memory-core container: `docker-compose.yml` lists what it passes, so add it in a `docker-compose.override.yml`.

**A CLI that goes through a cloud provider** needs that provider's variables, which are not among its own prefixes. Without them distillation fails after the upgrade. Common cases:

| CLI | Provider | `AGENT_MEMORY_LLM_CLI_PASS_ENV` |
| --- | --- | --- |
| claude | Amazon Bedrock (`CLAUDE_CODE_USE_BEDROCK`, passed as `CLAUDE_*`) | `AWS_*` (credentials, `AWS_PROFILE`, `AWS_REGION`) |
| claude | Google Vertex AI (`CLAUDE_CODE_USE_VERTEX`) | `CLOUD_ML_REGION`, plus `GOOGLE_APPLICATION_CREDENTIALS` or `GOOGLE_*` if needed (`GOOGLE_*` is passed only to `agy` and `gemini`) |
| codex | Azure OpenAI | `AZURE_OPENAI_*` (or the names your codex provider settings use) |

**A wrapper script** as `llm.cli.command` (say `/opt/bin/claude-wrapper`) is not recognized as `claude`, so it gets only the basics; list what the real CLI needs in `passEnv`. Hardening (section 6, Agent CLIs) also goes by the executable name, so the wrapper must turn off the CLI's tools itself.

**memory-core's own secrets are kept back by name.** Only `AGENT_MEMORY_*` is never passed. `memory-core.yaml` expands `${VAR}`, so a setting such as `embedding.apiKey: ${OPENAI_API_KEY}` reads a variable that codex also inherits (`OPENAI_*`). Name the variables that hold memory-core's own secrets `AGENT_MEMORY_*`, as `.env.example` does, or at least not with a prefix the CLI inherits. `llm.cli.env` cannot set `AGENT_MEMORY_*` either: memory-core refuses to start.

### Arguments that are refused

memory-core checks `AGENT_MEMORY_LLM_CLI_ARGS` at startup and on `memory-server check-config`, and refuses:

- `--dangerously-skip-permissions`, `--allow-dangerously-skip-permissions`, `--dangerously-bypass-approvals-and-sandbox` and any other `--dangerously-…`;
- `--yolo`, `--full-auto`, `--allowedTools` / `--allowed-tools`, `--mcp-config`, `--permission-prompt-tool`;
- `--permission-mode` other than `default` or `plan`;
- claude: `--tools` with a list of tools;
- codex: arguments without `exec`; `--enable …`, `--search`, `-p` / `--profile`, `-s` / `--sandbox` other than `read-only`, and these `-c` settings: `mcp_servers.…`, `features.…=true`, `web_search` other than `disabled`, `sandbox_mode` other than `read-only`, `sandbox_permissions`, `approval_policy`, `profile`. Short options count with their value attached too (`-cKEY=VALUE`, `-sMODE`, `-pNAME`). Settings such as `-c model_reasoning_effort=high` or `-c service_tier=fast` are fine.

### Self-hosted models (OpenAI-compatible)

memory-core never sends `tools` in its request. A reply that is only a tool call therefore means the model tried to act. memory-core treats any of these, with no other text, as `LLM_TOOL_REQUEST`:
- `finish_reason: "tool_calls"` or `"function_call"`;
- `tool_calls` or `function_call` in the message;
- tool-call markup such as `<tool_call>…</tool_call>`.

A reply that also contains an answer is used without the tool-call markup, so a model that adds a stray tool call is not dropped for it. An empty reply without a tool call is still retried as a transient failure.

## 7. Secrets and redaction

Distillation turns whatever it reads into lasting memories, so secrets are removed before anything is stored. Each match becomes `[REDACTED:<kind>]`.

**Where.** The `amem` client redacts a turn before it is queued or sent. memory-core redacts again on every write path, so a client that calls the HTTP API directly is covered too:

- conversations (`/v3/conversation/add`) and raw session turns (`/v3/session/add`). In a raw turn, a value under a secret field name (`password`, `pass`, `pin`, `otp`, `dbPassword`, `access_token`, `SECRET_KEY_BASE`, `credentials`, …) is replaced whole as `[REDACTED:secret-field]`, since the text rules cannot see the name;
- the skill-extraction buffer (`/v3/skill/conversation/add`), skill content, and skill files that are UTF-8 text (`.sh`, `.env.example`, …);
- atoms written through the API and atoms the L1 model writes (a model can spell out a secret its input did not contain);
- scenes and the persona, from L2/L3 and from the API.

The L1 and skill prompts also tell the model never to record a credential's value.

**Rules** (`crates/memory-client/redact`, shared by `amem` and memory-core):

| Kind | Catches |
| --- | --- |
| `private-key`, `jwt`, `openai-key`, `github-token`, … | Known token and key formats |
| `secret-assignment` | `password=…`, `api_key: …`: an English key name containing the secret word (`DB_PASSWORD=`, `SECRET_KEY_BASE=`, `GITHUB_TOKEN_RO=`); a key that ends in a setting word (`count`, `limit`, `length`, `type`, …) or counts `tokens` is a setting, so `max_tokens=` and `token_count:` stay, while `RATE_LIMIT_TOKEN=` and `MAX_API_KEY=` are redacted |
| `secret-phrase` | A password named in natural language, then its value. Words, with common abbreviations: English (`password`, `pw`, `pwd`, `pass`, `psw`, …), Chinese (`密碼`, `密码`, `口令`, `金鑰`, …), Japanese (`パスワード`, `パス`, …), Korean (`비밀번호`, `비번`, …), Vietnamese (`mật khẩu`, `mk`), Thai, Indonesian/Malay, Hindi, Arabic, Spanish, Portuguese, French, German, Turkish and Russian. After `:`, `=` or `：` any value is redacted; after an "is" word (`是`, `là`, `は`, `는`, `es`, …) or a space, only one that looks like a secret (a digit, a symbol, or mixed case). Words with an everyday meaning (`token`, `pass`, `mk`, `clave`, `パス`, `トークン`, `รหัส`, …) need `:`, `=` or an "is" word, so `token 4096` and `tests pass 100ms` stay |
| `password-shape` | No word needed: 8+ ASCII characters with a letter, a digit and one of `!@#$%^&+=?~\|` inside. Left alone: URLs, paths, `x=1` settings, email addresses, query strings (`?page=2&sort=asc1`), code (identifiers or numbers joined by `&&`, `\|\|`, `!=`, `==`, such as `cmd1&&cmd2`) and a whole model reference (`4@qwen3-8b`, `Claude@Opus-5.5`). A model name inside a string is not enough: `Falcon#2024x` is redacted |

**Settings.**

```yaml
redaction:
  passwordShape: true          # AGENT_MEMORY_REDACTION_PASSWORD_SHAPE
  patterns:                    # AGENT_MEMORY_REDACTION_PATTERNS (a JSON list)
    - kind: tw-national-id
      pattern: '\b[A-Z][12]\d{8}\b'
    - kind: phone
      pattern: '(?P<keep>\btel:\s*)\+?\d[\d -]{7,}\d'
```

- A `keep` group keeps that part of the match (here the `tel:` label).
- memory-core refuses to start when a pattern does not compile, or when `AGENT_MEMORY_REDACTION_PATTERNS` is not a JSON list. `memory-server check-config` reports the same.
- In `.env`, Docker Compose expands `$`. Write `$$` for a literal `$`, or put patterns in the config file.
- Personal data such as ids and phone numbers is not redacted by default. Which formats matter depends on the country and the deployment, so add them here.
- These settings apply to memory-core only. `amem` always uses the built-in rules, shape rule included, so turning `passwordShape` off or adding patterns does not change what the client redacts before sending.

**Rules for all teams, a team or a user.** On top of the built-in and deployment rules, people add their own, in the Panel (Redaction) or through `/v3/meta/redaction-rule/*` ([API](../API.md)). Each level only adds redaction; none can turn off a level above it. MCP and the distillation model cannot see or change them.

| Level | Applies to | Changed by | Read by |
| --- | --- | --- | --- |
| All teams | every write in the department | system admins | everyone in the department |
| A team (`team_id`) | its members' writes, and its shared scenes and persona | the team's admins, system admins | the team's members |
| A user (`user_id`) | that user's writes, in every team | the user, system admins | the user |

- Scenes and personas belong to a team, not to one user, so they go through the rules for all teams and the team's. Their input, the members' atoms, already went through each member's rules.
- Each rule is a kind (shown as `[REDACTED:<kind>]`; not a built-in one, nor one of `redaction.patterns`) and a Rust regex, at most 500 characters. A pattern that does not compile, matches the empty string or is too large is refused. So is one too broad to be meant: it matches a single letter, digit or CJK character on its own (`.`, `\w`, `e`), or more than 20% of a set of everyday sentences (`\S+`, `\w+`). Redaction happens before anything is stored, so such a rule would erase every write. At most 50 enabled rules per level (for all teams, per team, per user).
- Only the kinds of the rules for all teams are unique. A team's or a user's rule may not take one of them, but teams and users may use the same kind as each other, so nobody can take a name first or probe for rules they cannot read. So `[REDACTED:phone]` may come from any level. A rule for all teams added later may take a kind a team or a user already has; theirs keeps working and can still be changed.
- Rules never touch text already redacted (`[REDACTED:…]` from the client or an earlier rule), so markers stay readable and say which rule caught what. Only a marker of a known kind counts: `[REDACTED:hunter2xyz99]` is not one, and the secret in it is redacted like any other text.
- A kind is 1-40 characters of a-z, 0-9 and `-` in every layer; a `redaction.patterns` kind that is not stops memory-core at startup.
- **Try it** (`test`) runs sample text through the rules of a level and the levels above it (at a user's level, optionally with one team's too, as that user's writes there get), plus a pattern not saved yet, and shows the result, what each kind caught and how much of the sample the pattern matches; a pattern too broad to save is still previewed, with the reason. The text is not stored.
- A new or changed rule applies to the next write on that replica; other replicas pick it up within 30 seconds. Stored data is not rewritten.
- Removing a rule disables it. The record of who added and changed it, and when, stays; it can be enabled again.
- If the rules cannot be read, writes to the department fail rather than going through without them.

**Limits.**

- Rules are heuristics: a safety net, not a guarantee.
- Data stored before an upgrade, or before a rule was added, is not rewritten.
- Binary skill resource files (not UTF-8) are not scanned.

## 8. Embeddings and load

With embeddings on, every atom is embedded when it is stored. When that call fails or times out, the atom is still stored: recall finds it by keyword, not by meaning, until it gets an embedding.

**Backfill.** memory-core embeds atoms stored without one every `backfillIntervalSecs` (default 600; the first run waits one interval after start), up to `backfillBatch` atoms per department per run.

- Each run goes on where the last one stopped, in the order the atoms were written, and starts over at the end.
- An atom whose input the service refuses (HTTP 400, 413 or 422: too long for the model, say) is skipped, however many there are in a row, so they do not hold up the rest; the next pass tries them again.
- When the service fails three atoms in a row for its own reasons (unreachable, timeouts, 5xx, 429), it is taken as down: the department is left as it is until the next run.
- When it refuses 32 inputs in a row, its settings are taken as wrong (a misspelt `model`, or `dimensions` the model does not support): the run stops and logs `embedding service refuses every input; check memory.embedding model and dimensions`, and `reembed` says the same.
- The atom is not rewritten, so L2 does not treat it as new.

To run one pass now and see the result:

```bash
docker compose exec memory-core memory-server reembed
docker compose exec memory-core memory-server reembed --department default --limit 500 --json
```

**Settings.**

| Setting | Env | Default | |
| --- | --- | --- | --- |
| `memory.embedding.timeoutMs` | `AGENT_MEMORY_EMBEDDING_TIMEOUT_MS` | 10000 | Per request to the embedding service |
| `memory.embedding.maxConcurrency` | `AGENT_MEMORY_EMBEDDING_MAX_CONCURRENCY` | 4 | Requests in flight at once; more wait their turn |
| `memory.embedding.backfillIntervalSecs` | `AGENT_MEMORY_EMBEDDING_BACKFILL_INTERVAL_SECS` | 600 | `0` turns the background run off; `reembed` still works |
| `memory.embedding.backfillBatch` | | 200 | Atoms per department per background run |
| `llm.timeoutMs` | `AGENT_MEMORY_DISTILLATION_LLM_TIMEOUT_MS` | 180000 | Per call to the distillation LLM (L1/L2/L3, skill extraction), CLI provider included |
| `worker.concurrency` | `AGENT_MEMORY_WORKER_CONCURRENCY` | 8 | Pipeline jobs (L1/L2/L3/skills) at once |
| `llm.cli.maxConcurrency` | `AGENT_MEMORY_LLM_CLI_MAX_CONCURRENCY` | 2 | CLI model processes at once |

The backfill shares `maxConcurrency` with L1 and `atomic/update`: while it catches up after an outage, their embedding calls may wait for a free slot. It sends 32 atoms per request.

A small or shared embedding server that times out under load: lower `maxConcurrency` first, then raise `timeoutMs`. A self-hosted distillation model that needs minutes for a long answer (skill extraction writes the most) fails with `LLM_UNAVAILABLE` at the timeout and is retried: raise `AGENT_MEMORY_DISTILLATION_LLM_TIMEOUT_MS`, and keep any proxy in front of the model at least as patient. Many embeddings missing after an outage: run `reembed` once the service is back.

A numeric variable that does not parse (`AGENT_MEMORY_EMBEDDING_MAX_CONCURRENCY=abc`) stops memory-core at startup with its name; `memory-server check-config` reports it too.

## 9. Scenes and their atoms

L2 records which atoms each scene was built from, and at which version. An atom belongs to one scene at most, or to none (a preference may only belong in the persona). When an atom changes after that, the scene may still say the old thing, so it is marked **stale**, with a reason:

| Reason | When |
| --- | --- |
| `rewritten` | The atom was rewritten: L1 dedup or a correction, or `atomic/update` |
| `deleted` | The atom was deleted: merged away by L1, or `atomic/delete` |
| `moved_out` | L2 placed the atom in another scene |
| `changed_during_run` | The atom was rewritten while the L2 run that placed it waited for the model |

Each mark keeps the atom's text from before the change, so L2 can find it in the scene. A deleted atom's text stays in the mark until the mark is settled, as it does in the scene's body.

**L2 settles them.** The next L2 run of the agent is given up to `memory.l2.staleScenesPerRun` stale scenes with what they may still say and what is true now, even when there are no new atoms (a quiet topic gets none). For each one the model rewrites the scene or answers that it already agrees; either settles the marks it was given. Rewriting or deleting atoms through the API schedules that run as L1 does. Both texts pass the atom owner's redaction rules as they are at that moment, so a rule added after the atom was written also covers them. A mark is given to L2 at most `memory.l2.staleMaxAttempts` times, counting runs whose model call failed or answered something unusable; after that it stays until a person deals with it. Saving the scene (`scenario/write`, the Panel's L2 tab) settles all its marks, and removing it settles them too.

**Where to see them.** `scenario/ls` and `scenario/read` say whether a scene is `stale` and how many reasons it has; `scenario/atoms` lists the caller's atoms in it with their reasons, and only counts other members' (`hidden`). The Panel's L2 tab shows the same. Each L2 generation log lists the marks it was given as `stale` input refs.

| Setting | Default | |
| --- | --- | --- |
| `memory.l2.staleScenesPerRun` | 10 | Stale scenes given to one L2 run (not a limit on scenes) |
| `memory.l2.staleMaxAttempts` | 3 | Runs a mark is given to, failed ones included, before L2 stops being given it |

Scenes written before schema migration 8 have no recorded atoms; they get them as L2 places atoms in them again. Nothing is filled in for the past.
