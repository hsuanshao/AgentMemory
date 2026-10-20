# Token usage and efficiency observability (design)

[繁體中文](OBSERVABILITY.zh-tw.md)

Status: **proposal**. Nothing here is implemented yet, except where a section says it already exists.

## Contents

- [1. Goal](#1-goal)
- [2. What exists today](#2-what-exists-today)
- [3. Three kinds of tokens](#3-three-kinds-of-tokens)
- [4. Metric definitions](#4-metric-definitions)
- [5. Data model](#5-data-model)
- [6. Collection points](#6-collection-points)
- [7. Dashboard](#7-dashboard)
- [8. Offline evaluation](#8-offline-evaluation)
- [9. Phases](#9-phases)
- [10. Open questions](#10-open-questions)

## 1. Goal

Operators and users should be able to answer:

- How many tokens does AgentMemory itself spend, per team, user, agent and layer?
- How much context does recall add to each agent turn, and how much of it is useful?
- Does memory make agents cheaper or faster overall, for whom, and in which scenarios?

The answer has to be broken down by the same scopes the product already uses (instance → team → user → agent → session), plus the memory layer and the scenario (L2 scene).

## 2. What exists today

| Signal | Where | Gap |
| --- | --- | --- |
| Generation log per LLM call: layer, team/agent/user/session, model, input and output refs, latency, error | `generation_logs` table, `GenerationLogService` | **No token counts.** `LlmProvider::complete()` returns only `String`; the OpenAI-compatible provider drops `usage`, and the CLI provider has none |
| Pipeline worker counters: enqueued, completed, failed, last error | `/health` → `services.pipelineWorker.metrics` | Process-wide totals only, not per scope, reset on restart |
| Recall | `RecallService::recall` (score threshold 0.3, at most 50 atoms, plus persona and scene navigation) | **Nothing is recorded**: no count, latency, size, or hit rate |
| Agent-side cost | The host's transcript (Claude Code writes `usage` on every assistant message) | The server never sees it. `amem` reads the transcript on every Stop but sends only the messages |
| Panel dashboard | Health, layer counts, pipeline panel | No token or cost view |

Two problems found while measuring, both in scope here:

- **Irrelevant recall is injected anyway.** A recall can return memories that clear the score threshold but have nothing to do with the prompt. They cost tokens on every turn and nobody notices.
- **Recalled memories are dated by distillation time** (`atom.updated_at`), not by when the event happened. After importing old sessions, every memory reads as "today".

## 3. Three kinds of tokens

Memory is worth it when what it saves exceeds what it costs. The costs and the saving are measured separately:

| Kind | Who pays | Examples |
| --- | --- | --- |
| **Spend**: what the memory service consumes | The operator (LLM provider of memory-core) | L1 extraction, dedup, L2 scenes, L3 persona, skill extraction, embeddings |
| **Injection**: what recall adds to agent prompts | The user (the agent's LLM) | `<relevant-memories>`, persona and scene navigation, on every turn |
| **Saving**: what the agent no longer spends | The user | Fewer exploration tool calls and tokens before the agent can act |

**Net effect = saving − injection − spend.** Spend and injection can be measured exactly or estimated closely. Saving needs a baseline, so it is estimated (section 4) and calibrated offline (section 8).

## 4. Metric definitions

Use the same definitions in the dashboard and in offline evaluation, so their numbers can be compared.

**Token kinds.** Record `input`, `cached_input` (cache reads), `cache_write` and `output` separately. Report a **weighted total** using each kind's price relative to plain input. For Anthropic models that is cache read ×0.1, cache write ×1.25, output ×5. Weights are per model and configurable.

**Estimated values** carry an `estimated` flag and are shown as such. A CLI provider without usage output is estimated from the character count with a per-script ratio (CJK and Latin differ).

**Agent turn metrics.** A turn starts at a real user prompt and ends before the next one; task notifications and command output are not prompts.

| Metric | Definition |
| --- | --- |
| `turn_tokens` | Weighted tokens of all model calls in the turn. Streaming repeats `usage` per content block, so count it once per message id |
| `exploration_calls` | Read-only tool calls before the turn's first change (Edit/Write). Read-only: Read, Grep, Glob, web fetch and search, and Bash commands made only of read commands (`ls`, `cat`, `grep`, `git log/show/diff/status`, …) |
| `exploration_tokens` | Weighted tokens of the model calls up to the first change |
| `clarifying_questions` | AskUserQuestion calls in the turn |
| `cold_start_*` | The same metrics, for the first turn of a session only |

**Recall metrics.**

| Metric | Definition |
| --- | --- |
| `recall_injected_tokens` | Estimated tokens of the context returned to the host (memories, persona, scene navigation) |
| `recall_items` | Items returned, per layer |
| `recall_empty` | No memory cleared the threshold |
| `recall_used` | Sampled: did the turn's answer use the recalled content? Judged offline (section 8), never on the hot path |

## 5. Data model

One append-only table plus hourly rollups, in both storage backends.

`usage_events`:

| Column | Notes |
| --- | --- |
| `event_id`, `ts` | |
| `service_id`, `team_id`, `agent_id`, `user_id`, `session_id` | The scope. `user_id` is empty for agent-level work (L2, L3) |
| `kind` | `distill_l1`, `dedup`, `distill_l2`, `distill_l3`, `skill_extract`, `embed`, `recall`, `agent_turn` |
| `model` | As reported by the provider, or the CLI command |
| `input_tokens`, `cached_input_tokens`, `cache_write_tokens`, `output_tokens` | Nullable |
| `estimated` | `true` when the counts are estimates |
| `latency_ms`, `chars` | |
| `scene_id` | When the work is tied to one L2 scene (scene update, recall hit) |
| `detail` | Small JSON: per-layer recall counts, `exploration_calls`, the generation log id |

`usage_rollup_hourly` is keyed by hour, scope, kind and model, and holds sums plus a latency histogram. The dashboard reads rollups; the raw events serve drill-down and are kept for a configurable number of days.

Generation logs gain the same token columns. A `distill_*` event refers to its log by id instead of duplicating it.

## 6. Collection points

| What | Where | How |
| --- | --- | --- |
| Spend | `LlmProvider` | `complete()` returns `Completion { text, usage: Option<Usage> }`. The OpenAI-compatible provider reads `usage`. The CLI provider reads usage from JSON output when the configured tool supports it (`claude -p --output-format json`, `codex exec --json`); otherwise it estimates. Each runner records one event per call, next to its generation log |
| Embeddings | `EmbeddingProvider::embed` | Provider usage when returned; otherwise estimated |
| Injection | `RecallService::recall` | One `recall` event per call: query length, items per layer, returned characters and estimated tokens, latency, empty or not |
| Agent turns | `amem` on Stop | `amem` already reads the transcript. It also sends the turn's metrics (section 4), computed locally, as a small `turn_stats` object next to the captured messages. No transcript content is added. Hosts without usage in their transcript send none |

`turn_stats` is opt-out on the client (`AMEM_TURN_STATS=0`) and only ever contains counts.

## 7. Dashboard

The Panel gains a **Usage** page. Visibility follows the existing roles: a member sees their own usage, a team admin sees their team, a system admin sees everything.

1. **Overview** for a time range:
   - spend, injection, agent turn tokens;
   - the estimated saving and the net effect;
   - each as a trend line.
2. **Breakdown:** a table by team → user → agent, sortable by any metric, with drill-down to a session's events.
3. **Pipeline:** per layer, tokens per run, p50 and p95 latency, failures and queue depth. This replaces the process-wide counters.
4. **Recall quality:**
   - hit rate and empty rate;
   - injected tokens per turn;
   - items per layer;
   - the share of sampled recalls judged unused, which flags irrelevant injection.
5. **Scenarios:** per L2 scene, how often it is recalled, the tokens it injects, and what keeping it up to date costs.

Budgets come later: the existing `instance-quota` can grow a per-team token budget with an alert at a threshold.

## 8. Offline evaluation

Online numbers show trends, but tasks differ from week to week. A controlled comparison calibrates the "saving" estimate:

1. **Baseline:** compute the section 4 metrics over existing transcripts recorded before memory was enabled.
2. **Paired A/B:**
   - A fixed set of questions: some need history, some are controls that don't.
   - Each question runs in a fresh headless session with the memory plugin on and with it off. Same model, same commit, several runs each.
   - The host's own memory features are off in both groups.
   - Record the section 4 metrics, plus cost and duration.
3. **Quality:** a judge that doesn't know which group an answer came from scores each answer against key facts. A token saving only counts if quality holds.
4. **Report:** the median paired difference per question and metric. The control questions show the pure overhead of injection.

The first run of this protocol is planned on a real deployment: 57 imported Claude Code sessions, 10 questions, 3 runs per group.

## 9. Phases

| Phase | Scope | Changes |
| --- | --- | --- |
| 0 | Offline evaluation and metric definitions (section 4 and section 8) | Scripts only |
| 1 | **Spend and injection:** `Completion.usage`, token columns on generation logs, `usage_events` and rollups, `recall` events, Panel Usage page (overview, breakdown, pipeline, recall) | memory-core, Panel |
| 2 | **Saving:** `turn_stats` from `amem`, the saving estimate on the dashboard, sampled recall-use judging | amem, memory-core, Panel |
| 3 | **Scenarios and budgets:** per-scene attribution, per-team budgets and alerts | memory-core, Panel |

Fixes worth doing alongside phase 1:

- **Recall dates:** show the time of the source conversation, not `updated_at`.
- **Injection budget:** a token budget for injection, and a stricter relevance cutoff for memories that clear the threshold only barely.

## 10. Open questions

1. **Price tables:** are weights configured per model by the operator, or fetched? Cost in currency would need a price list, which this design avoids.
2. **CLI providers without usage output (currently `agy -p`):** is a character-based estimate good enough, or should the CLI provider prefer tools that report usage?
3. **Retention** of raw `usage_events`, and whether sessions can be deleted together with their events.
4. **Privacy of `turn_stats`:** counts only, but tool names reveal how someone works. Should team admins see per-user detail or only totals?
