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

Each log also records the model. For a CLI provider that is the command plus the model its arguments select, for example `cli:agy:gemini-3.8-flash-high`.

A failed job is retried after 5, 15 and 45 seconds (`worker.maxRetries`, default 3). If every attempt fails, it becomes a **dead letter**, kept with its last error. Dead letters are not retried on their own; see section 5.

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

The commands call the running server's API, so they work the same on every storage and coordination backend. `--instance` selects an instance other than `default`. The API endpoints behind them are in [API.md](../API.md#health-and-monitoring).

## 3. Common failures

| Error (in `error_message` or the dead letter) | Likely cause | What to do |
| --- | --- | --- |
| `RESOURCE_EXHAUSTED`, `quota`, `429` | The LLM account ran out of quota | Wait for the reset, or switch to a model with its own quota (section 4). Then redrive |
| `` `agy` produced no output `` | The CLI exited cleanly but printed nothing | Usually transient and retried. If one batch keeps failing, try another model |
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

Once the cause is fixed, put dead letters back in the queue:

```bash
docker compose exec memory-core memory-server dead-letters redrive --limit 10
docker compose exec memory-core memory-server dead-letters redrive --kind skill --limit 20
```

The Panel's **Re-run** button does the same, 10 at a time. A redriven job starts again as a first attempt, oldest first. With several replicas, each dead letter is redriven by one of them only.

Redrive in small batches and watch the failures between them: a backlog sent at once can use up an LLM quota again. Redriving is safe to repeat:
- An L1 job continues from its session's cursor, so it never processes the same messages twice.
- L2, L3 and skill jobs work on the current state of their agent or task.
