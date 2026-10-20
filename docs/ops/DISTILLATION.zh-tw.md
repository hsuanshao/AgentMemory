# 監看與修復蒸餾

[English](DISTILLATION.md)

memory-core 會在背景蒸餾對話：L1 擷取記憶，L2 整理成 scene，L3 改寫 persona。每一步都會呼叫設定好的 LLM。這份文件說明如何確認蒸餾是否正常運作，以及出問題時怎麼處理。

## 目錄

- [1. 會記錄哪些資訊](#1-會記錄哪些資訊)
- [2. 在哪裡查看](#2-在哪裡查看)
- [3. 常見的失敗原因](#3-常見的失敗原因)
- [4. 更換模型](#4-更換模型)
- [5. Dead letter 與重跑](#5-dead-letter-與重跑)

## 1. 會記錄哪些資訊

每一次呼叫到 LLM 的 L1、L2、L3 執行，都會留下一筆生成紀錄：

| 狀態 | 意思 |
| --- | --- |
| `succeeded` | 寫出了記憶、scene 或 persona |
| `empty` | LLM 有回應，但沒有寫出任何東西：沒有值得保留的內容，或回應沒有改變任何東西。連續出現大量 `empty`，可能代表模型回答得不好 |
| `failed` | 呼叫失敗（額度用完、逾時、沒有輸出），或回應無法使用。`error_message` 會說明原因 |

每筆紀錄也會記下使用的模型。CLI provider 會記成指令加上參數選用的模型，例如 `cli:agy:gemini-3.8-flash-high`。

失敗的 job 會在 5、15、45 秒後重試（`worker.maxRetries`，預設 3 次）。每次嘗試都失敗，就會成為 **dead letter**，並保留最後一次的錯誤。Dead letter 不會自動重試，處理方式見第 5 節。

| Coordination backend | Dead letter 存放位置 |
| --- | --- |
| `redis` | 存在 Redis，保留最新的 10,000 筆，所有 replica 共用 |
| `local` | 存在記憶體，保留最新的 1,000 筆，重新啟動後就會消失 |

## 2. 在哪裡查看

**Panel → Dashboard → Distillation**：
- 目前使用的 LLM；
- 過去 24 小時各層 succeeded、empty、failed 的次數與失敗率；
- 最近幾筆失敗和它們的錯誤；
- 各類 job 的 dead letter 數量。

System admin 還會看到重跑 dead letter 的按鈕。**Panel → Generation logs** 列出每一次執行；可以依狀態篩選（`#/logs?status=failed`），點開單筆就能看到錯誤、模型、輸入和輸出。

**在伺服器上**，於安裝目錄執行：

```bash
# 過去 24 小時各層、各狀態的次數，以及最近幾筆失敗
docker compose exec memory-core memory-server generations
docker compose exec memory-core memory-server generations --status empty --layer l1 --hours 6

# Dead letter，新的在前
docker compose exec memory-core memory-server dead-letters
docker compose exec memory-core memory-server dead-letters --kind l1 --json
```

這些指令會呼叫正在執行的伺服器 API，所以不論使用哪種 storage 或 coordination backend，行為都一樣。`--department` 可以選擇 `default` 以外的部門。背後用到的 API 端點見 [API.zh-tw.md](../API.zh-tw.md#健康檢查與監控)。

## 3. 常見的失敗原因

| 錯誤（在 `error_message` 或 dead letter 中） | 可能原因 | 處理方式 |
| --- | --- | --- |
| `RESOURCE_EXHAUSTED`、`quota`、`429` | LLM 帳號的額度用完了 | 等額度重置，或換一個有獨立額度的模型（第 4 節），之後再重跑 |
| `` `agy` produced no output; stderr: … `` | CLI 正常結束但沒有輸出回應，後面附上它寫到 stderr 的內容 | 通常是暫時性的，會自動重試。如果同一批一直失敗，先看 stderr 的部分，或換個模型試試 |
| `` `…` timed out after … `` | 回應時間超過 `llm.timeoutMs` | 調高逾時，或換一個比較快的模型 |
| `llm_output_invalid` | 回應不是該步驟要求的 JSON | 模型不太遵守指示，換一個模型 |
| `LLM returned 5xx` | OpenAI 相容端點出錯 | 檢查端點；短暫的中斷通常靠重試就能撐過 |

## 4. 更換模型

CLI provider 的模型寫在 `.env` 的參數裡：

```bash
AGENT_MEMORY_LLM_CLI_ARGS=["-p","{prompt}","--model","gemini-3.8-flash-high"]
```

OpenAI 相容 provider 則是 `AGENT_MEMORY_LLM_MODEL`。換之前先確認模型能回應、也還有額度，例如 `docker compose exec memory-core agy -p "Reply OK" --model gemini-3.8-flash-high`。確認後執行 `docker compose up -d memory-core` 套用。重新啟動前還在執行的 job，重啟後會再派送一次。

## 5. Dead letter 與重跑

修好原因之後，把 dead letter 重新排回佇列：

```bash
docker compose exec memory-core memory-server dead-letters redrive --limit 10
docker compose exec memory-core memory-server dead-letters redrive --kind skill --limit 20
```

Panel 上的 **Re-run** 按鈕效果相同，每次 10 筆。重跑的 job 會從第一次嘗試重新開始，最舊的先跑。有多個 replica 時，每筆 dead letter 只會由其中一個重跑。

請小批重跑，並在每批之間觀察失敗情況：一次把積壓全部送出，可能又把 LLM 的額度用完。重跑可以安全地重複執行：
- L1 job 會從該 session 的游標接著處理，不會重複處理同樣的訊息。
- L2、L3 和 skill job 處理的是 agent 或 task 當下的狀態。
