# MemoryCore (Rust) HTTP API

[繁體中文](API.zh-tw.md)

RPC-style v3 API. This file lists every implemented endpoint and the
response conventions. For how the pieces fit together (trust zones,
authorization flow, pipeline) see [ARCHITECTURE.md](ARCHITECTURE.md).

## Conventions

* Port `8420`. All `/v3/*` endpoints are `POST` with a JSON body.
* Envelope: `{ "code": 0, "message": "ok", "request_id": "req-…", "data": … }`.
  HTTP status equals `code` for 4xx/5xx; skill errors use five-digit codes
  (e.g. `40901`) with the matching HTTP status.
* Two trust zones:
  * **User-scoped** — the data plane (L0–L3, `recall`, `session/add`) and
    `/v3/skill/*`. Authorized by `x-memory-user-key` alone; no Bearer key is
    needed, so remote clients never hold the server API key.
  * **Bearer-scoped** — everything else (pipeline/stats, logs, prompts,
    knowledge, `meta/*`, `internal/*`, `department/destroy`):
    `Authorization: Bearer <AGENT_MEMORY_API_KEY>` when an API key is
    configured. `/v3/meta/*` additionally needs `x-memory-user-key`
    (except `/v3/meta/auth/verify`).
* `x-memory-department-id: <department id>` — required on every `/v3/*` route
  except `/v3/department/destroy`. Installs use `default`.
* Isolation: `team_id`/`agent_id`/`user_id`/`task_id`/`session_id` in the
  body, or `x-memory-team-id` / `x-memory-agent-id` / … headers. Body wins.
* Data-plane authorization (`crates/memory-domain/src/meta/memory_access.rs`):
  * `team_id` and `agent_id` are required (there is no `default` bucket);
    the agent must belong to the team. Missing ⇒ 400, mismatch ⇒ 403.
  * `user_id` defaults to the caller. Members may read and write only their
    own memory; only system admins may name another user.
  * The caller must be an active member of the team (system admins excepted).
  * The resolved scope is written back into the body and headers before the
    handler runs, so handlers only ever see an authorized scope.
* Pagination: `limit` (default 20, max 100) and `offset`.

### Errors

The HTTP status equals `code` in the envelope; `message` explains.

| Status | When |
| --- | --- |
| 400 | Invalid JSON or input; `team_id` / `agent_id` missing on the data plane |
| 401 | Missing or wrong Bearer key; unknown, revoked or expired user key |
| 403 | Not an active member of the team; agent of another team; naming another `user_id` without being a system admin; metadata action not allowed |
| 404 | Unknown route or entity |
| 409 | Conflict, e.g. `already_initialized`, `target_has_profile` |
| 413 | Body over `max_body_bytes` (`max_session_body_bytes` for `session/add`) |
| 429 | Quota exceeded |
| 503 | A dependency (LLM, database) is unavailable |

Skill errors use five-digit codes with the matching HTTP status, e.g. `40301` not owner, `40401` not found, `40901` stale version, `41301` too large.

## Client quick start

What `amem` sends, as plain HTTP. Every data-plane call carries the user key, the department id, and the team and agent (in the body or as `x-memory-team-id` / `x-memory-agent-id` headers):

```bash
H=(-H "x-memory-user-key: sk-mem-…" -H "x-memory-department-id: default" -H "content-type: application/json")

# recall before a turn
curl -sS -X POST https://memory.example.com/v3/recall "${H[@]}" \
  -d '{"team_id":"team-…","agent_id":"agt-…","query":"release plan","max_results":8}'

# capture the text of a finished turn (→ L0, then L1–L3 in the background)
curl -sS -X POST https://memory.example.com/v3/conversation/add "${H[@]}" \
  -d '{"team_id":"team-…","agent_id":"agt-…","session_id":"s-1",
       "messages":[{"role":"user","content":"…"},{"role":"assistant","content":"…"}]}'

# the same turn with tool calls (→ skill buffer, distilled into skills)
curl -sS -X POST https://memory.example.com/v3/skill/conversation/add "${H[@]}" \
  -d '{"team_id":"team-…","agent_id":"agt-…","session_id":"s-1",
       "messages":[{"role":"user","content":"build it"},
                   {"role":"tool_call","content":"{\"command\":\"cargo build\"}","tool_name":"Bash","tool_call_id":"t1"},
                   {"role":"tool_result","content":"Finished","tool_name":"Bash","tool_call_id":"t1"},
                   {"role":"assistant","content":"Done."}]}'
```

`user_id` is omitted: it defaults to the key's owner. Tool messages need a `tool_call_id` that pairs the call with its result.

## Endpoints

### Health and monitoring

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/health` | `{status, version, uptime, stores{vectorStore,embeddingService,ftsAvailable,llmConfigured}, services{pipelineWorker, stateBackend}}` |
| POST | `/v3/pipeline/status` | `{limit?}` (default 50, at most 500) → `{worker: WorkerMetrics, queue: {pending, running, dead_letter}, scopes: ScopeState[]}`. Each scope row has a unique `key`, the ids, `team_name` / `agent_name` / `username`, and `can_read_memory` |
| POST | `/v3/stats/overview` | Layer counts for the department: `{l0, l1: {total, episodic, persona, instruction}, l2, l3, skills, generation_logs}` |
| POST | `/v3/memory-generation-log/summary` | `{hours?}` (default 24, at most 720) → `{hours, from_ms, llm: {configured, model}, items: [{layer, status, count}]}`: distillation runs per layer and status |
| POST | `/v3/pipeline/dead-letter/list` | `{kind?, limit?}` (`l1`, `l2`, `l3`, `skill`; default 50, at most 500) → `{items: [{kind, job, attempt, error, dead_at_ms}]}`, newest first, this department only |
| POST | `/v3/pipeline/dead-letter/redrive` | `{kind?, limit?}` (default 10, at most 1000) → `{requeued}`: queues the oldest matching dead letters again as first attempts. With a user key, system admins only |

`WorkerMetrics`: `{tasks_enqueued, tasks_completed, tasks_failed, tasks_retried, tasks_dead_lettered, last_task_at, last_error}`.
`ScopeState`: `{scope, team_id, agent_id, user_id, session_id, conversation_count, warmup_threshold, l1_cursor, l2_pending, last_l1_at, last_l2_at, last_l3_at, memories_since_persona}`.

Every L1, L2 and L3 run that reaches the LLM leaves a generation log with a `status`:
- `succeeded`: it wrote memories, scenes or the persona;
- `empty`: the LLM answered but nothing was written;
- `failed`: the call or its answer failed; `error_message` says why.
- `dropped`: failed on the last attempt of an L1 or L2 run; the input was skipped so later input could go on, and the dead letter's job (with `range`) redoes it.

`model` names the LLM, including the model a CLI provider selects (`cli:agy:gemini-3.8-flash-high`). A job whose every attempt fails becomes a **dead letter**, kept with its last error until it is redriven. With `coordination.backend: local`, dead letters live in memory (at most 1,000) and are lost on restart. The same views are in the Panel dashboard and in `memory-server generations` / `memory-server dead-letters` ([docs/ops/DISTILLATION.md](ops/DISTILLATION.md)).

These routes take an optional `x-memory-user-key`. Without one (the operator, `memory-server` commands) they cover the whole department. With one, scopes, generation logs and dead letters are limited to what that user's role allows: a team admin or leader sees their whole team, a member sees the team's shared L2/L3 work and their own ([COLLABORATION.md §12](COLLABORATION.md#12-operational-state-by-role)). The Panel always sends the signed-in user's key.

### Recall

| Path | Notes |
| --- | --- |
| `/v3/recall` | Body `{query, max_results?, strategy?}` + scope. Returns `{strategy, memories: [{id, type, content, score}], prepend_context, append_system_context}`. Integrations inject `prepend_context` and `append_system_context` (persona, scene navigation) as they are |

### Data plane (L0–L3)

`/v3/conversation/{add,query,search,delete,count}`,
`/v3/atomic/{update,query,search,delete,count}`,
`/v3/scenario/{ls,read,write,rm,count}`,
`/v3/core/{read,write,count}`, `/v3/session/add`.

* `conversation/add` `{session_id, messages: [{role: user|assistant, content, timestamp?}]}` stores L0 and drives the pipeline.
* `session/add` stores a raw session turn object (needs `x-memory-user-id` and `x-memory-session-id`, which the middleware sets from the resolved scope).
* L0/L1 are scoped by team + agent + user; L2 (`scenario/*`) and L3 (`core/*`) by team + agent only.

### Skills

`/v3/skill/{create,update,patch,delete,get,get-by-name,list,search,versions,listing,extract}`,
`/v3/skill/files/{write,remove,read}`,
`/v3/skill/conversation/{add,force-archive}`.
`/v3/skill/export` is not implemented (returns 501).

### Knowledge metadata, chat memory, prompts, generation logs

`/v3/knowledge/{create,get,update,delete,list}`,
`/v3/chat-memory/clear`,
`/v3/memory-prompt/{create,get,update,delete,set,setting/list,log}`,
`/v3/memory-generation-log/{list,get,summary}` (`list` takes `{layer?, status?, start_time?, end_time?, limit?, cursor?}`).

### Metadata plane (`x-memory-user-key` required)

`/v3/meta/user/{create,create-with-key,get,delete,list}`,
`/v3/meta/user-key/{create,list,get,revoke,update}`,
`/v3/meta/team/{create,get,update,delete,list}` (`list` with `"all": true` lists every team; system admins only),
`/v3/meta/team-member/{add,remove,list,get}`,
`/v3/meta/agent/{create,get,update,delete,list,archive}`,
`/v3/meta/task/{create,get,update,delete,list,archive}`,
`/v3/meta/task-agent/{link,unlink,list}`,
`/v3/meta/participation-log/{append,list}`,
`/v3/meta/asset/{create,get,update,delete,list,list-accessible,touch-usage}`,
`/v3/meta/agent-fixed-asset/{set,list,list-with-detail,summary-by-agents}`,
`/v3/meta/acl/{grant,revoke,list,check}`,
`/v3/meta/auth/verify`, `/v3/meta/department-quota/get`,
`/v3/meta/config/user/{get,set}`.

Meta list responses: `{items, total, limit, offset}`.

### Internal / operations (Bearer only)

`/v3/internal/meta/user/init-admin` `{username, user_key?}` → `{user_id, user_key, team_id, agent_id}`
— first-run bootstrap: a system admin plus `default-team` and `default-agent-<username>`
(409 `already_initialized` once any user exists),
`/v3/internal/meta/user/list-by-department`,
`/v3/internal/migrate/default-scope` `{team_id, agent_id, user_id, from_team_id?, from_agent_id?, from_user_id?, dry_run?}` →
`{dry_run, to, moved: {conversations, atoms, profile_files, skill_versions}, session_states, profile_states}`
— upgrade helper that moves data written to the legacy `default` buckets
(L0/L1 of `default/default/default`, L2/L3 and skills of `default/default`)
into a real scope, pipeline cursors included. A pre-0.1.1 client that named
its agent or user wrote to `default/<agent>/<user>` instead; select that
bucket with `from_team_id` / `from_agent_id` / `from_user_id` (each
defaults to `default`). The agent must belong to the
team, the user must be an active member, and the agent must have no L2/L3
yet (409 `target_has_profile`, checked only when legacy data remains, so a
re-run after a successful move is a no-op). `dry_run: true` rolls back and
only counts.
`/v3/internal/upgrade` `{admin_username?, team_name?, agent_name?, issue_key?, dry_run?}` →
`{dry_run, user_id, username, team_id, team_name, team_created, agent_id, agent_name, agent_created, moved, session_states, profile_states, user_key?}`
— what `upgrade.sh` calls: picks the system admin (default: the oldest),
finds the team (default `default-team`) and agent (default
`default-agent-<admin>`) by exact name or creates them, runs the
default-scope migration into them for that admin and, with `issue_key`,
returns a new user key once. A dry run creates nothing (`team_id`/`agent_id`
are `null` when they would be created). Repeatable.
`/v3/department/destroy` `{department_id}`.

## Design decisions

* No vendor-specific integrations: analytics, external task sources and
  upstream model-group routes are not provided.
* Correctness guarantees: `atomic/update` looks up by id;
  recall and search always apply scope isolation; `time_start`/`time_end`
  are honoured by search; `scenario/ls` validates `path_prefix`; new L1
  records start at version 1.
* L2 scene and L3 persona generation use structured JSON operations
  instead of an agentic tool loop; prompts follow the strategy
  (UPDATE > MERGE > CREATE, heat, scene cap, META header, Scene Navigation).
