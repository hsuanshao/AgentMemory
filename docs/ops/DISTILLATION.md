# Watching and recovering distillation

[繁體中文](DISTILLATION.zh-tw.md)

memory-core distils conversations in the background: L1 extracts memories, L2 folds them into scenes, L3 rewrites the persona. Each step calls the configured LLM. This page covers how to see whether that works, and what to do when it does not.

## Contents

- [1. What is recorded](#1-what-is-recorded)
- [2. Where to look](#2-where-to-look)
- [3. Common failures](#3-common-failures)
- [4. Changing the model](#4-changing-the-model)
- [5. Dead letters and redrive](#5-dead-letters-and-redrive)

## 1. What is recorded

Every L1, L2 and L3 run that reaches the LLM leaves a generation log:

| Status | Meaning |
| --- | --- |
| `succeeded` | It wrote memories, scenes or the persona |
| `empty` | The LLM answered but nothing was written: nothing worth keeping, or an answer that changed nothing. Many `empty` runs in a row can mean a model that answers badly |
| `failed` | The call failed (quota, timeout, no output) or its answer could not be used. `error_message` says why |
| `dropped` | Like `failed`, on the **last** attempt of an L1 or L2 run: the input was skipped so the session or agent could go on (below) |

Each log also records the model. For a CLI provider that is the command plus the model its arguments select, for example `cli:agy:gemini-3.8-flash-high`.

A failed job is retried after 5, 15 and 45 seconds (`worker.maxRetries`, default 3). A failure that would happen again is not retried: the model tried to use a tool (distillation never allows one, see #37), or its answer was not the JSON the step asks for. If every attempt fails, or the failure would repeat, the job becomes a **dead letter**, kept with its last error and the model it failed with. Section 5 explains what happens to dead letters.

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
| failed for a passing reason (quota, timeout, outage) | It is **redriven** on its own 1 h after it died, then 2 h, then 4 h. At most 10 per sweep |
| failed in a way that would repeat (tool refused, invalid answer) | It waits until the configured model changes, then is redriven like the others; with the same model it would only fail again. A letter written before this was recorded is taken to have failed with the current model |

Each redrive, by hand or automatic, counts towards the 3. Settings (`memory-core.yaml`, section `worker`): `deadLetterTtlHours` (24), `deadLetterMaxRedrives` (3), `deadLetterRedriveAfterMinutes` (60), `deadLetterSweepSeconds` (300), `deadLetterRedriveBatch` (10). The Panel shows each letter's re-runs; `/health` counts `dead_letters_redriven` and `dead_letters_expired`.

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
