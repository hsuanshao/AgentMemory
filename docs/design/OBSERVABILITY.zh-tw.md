# Token 用量與效率可觀測性（設計）

[English](OBSERVABILITY.md)

狀態：**提案**。除了標明「已經存在」的部分，這份文件裡的東西都還沒有實作。

## 目錄

- [1. 目標](#1-目標)
- [2. 目前已有的資料](#2-目前已有的資料)
- [3. 三種 token](#3-三種-token)
- [4. 指標定義](#4-指標定義)
- [5. 資料模型](#5-資料模型)
- [6. 收集點](#6-收集點)
- [7. Dashboard](#7-dashboard)
- [8. 離線評估](#8-離線評估)
- [9. 分階段](#9-分階段)
- [10. 待決問題](#10-待決問題)

## 1. 目標

讓維運者和使用者能回答這些問題：

- AgentMemory 本身花了多少 token？依 team、user、agent、layer 分別是多少？
- recall 在每一輪 agent 對話中加了多少 context？其中有多少真的有用？
- 整體來說，記憶有沒有讓 agent 更省或更快？對誰有效？在哪些情境有效？

答案要能依產品現有的範圍拆開：instance → team → user → agent → session，再加上記憶 layer 和情境（L2 scene）。

## 2. 目前已有的資料

| 訊號 | 位置 | 缺口 |
| --- | --- | --- |
| 每次 LLM 呼叫的 generation log：layer、team/agent/user/session、model、輸入和輸出的參照、延遲、錯誤 | `generation_logs` 資料表、`GenerationLogService` | **沒有 token 數**。`LlmProvider::complete()` 只回傳 `String`；OpenAI 相容 provider 丟掉了 `usage`，CLI provider 則完全沒有 |
| Pipeline worker 計數：enqueued、completed、failed、last error | `/health` → `services.pipelineWorker.metrics` | 只有整個程序的總數，不分範圍，重啟後歸零 |
| Recall | `RecallService::recall`（分數門檻 0.3，最多 50 筆 atom，另外加上 persona 和 scene 導覽） | **完全沒有紀錄**：沒有次數、延遲、大小、命中率 |
| Agent 端的成本 | host 的 transcript（Claude Code 每則 assistant 訊息都有 `usage`） | server 看不到。`amem` 每次 Stop 都會讀 transcript，但只送出訊息內容 |
| Panel dashboard | 健康狀態、各 layer 數量、pipeline 面板 | 沒有 token 或成本的畫面 |

量測時發現兩個問題，也納入這份設計：

- **不相關的 recall 照樣被注入。** recall 可能回傳分數過了門檻、但跟 prompt 毫無關係的記憶。它們每一輪都在花 token，而且沒有人會發現。
- **召回的記憶以蒸餾時間標日期**（`atom.updated_at`），而不是事件發生的時間。匯入舊 session 之後，每則記憶看起來都像是「今天」。

## 3. 三種 token

只有省下來的比花掉的多，記憶才划算。成本和節省要分開量：

| 種類 | 誰付錢 | 例子 |
| --- | --- | --- |
| **花費**：記憶服務自己用掉的 | 維運者（memory-core 的 LLM provider） | L1 擷取、dedup、L2 scene、L3 persona、skill 擷取、embedding |
| **注入**：recall 加進 agent prompt 的 | 使用者（agent 的 LLM） | 每一輪的 `<relevant-memories>`、persona、scene 導覽 |
| **節省**：agent 不必再花的 | 使用者 | agent 開始動手前，少掉的探索工具呼叫和 token |

**淨效益 = 節省 − 注入 − 花費。** 花費和注入可以精確量測或接近地估算。節省需要基準線，所以用估算（第 4 節），再用離線評估校正（第 8 節）。

## 4. 指標定義

Dashboard 和離線評估使用同一套定義，數字才能互相比較。

**Token 種類。** 分別記錄 `input`、`cached_input`（cache 讀取）、`cache_write`、`output`。報表使用**加權總數**，權重是各種 token 相對於一般 input 的價格。以 Anthropic 模型來說，cache 讀取 ×0.1、cache 寫入 ×1.25、output ×5。權重依模型設定，可以調整。

**估算值**帶有 `estimated` 標記，畫面上也會標示是估算。沒有 usage 輸出的 CLI provider，用字元數依文字種類換算（CJK 和拉丁文字的比例不同）。

**Agent 一輪的指標。** 一輪從真正的使用者 prompt 開始，到下一個 prompt 之前結束；背景任務通知和指令輸出不算 prompt。

| 指標 | 定義 |
| --- | --- |
| `turn_tokens` | 這一輪所有模型呼叫的加權 token。串流時每個 content block 都會重複帶 `usage`，所以每個 message id 只算一次 |
| `exploration_calls` | 這一輪第一次修改（Edit/Write）之前的唯讀工具呼叫。唯讀包括：Read、Grep、Glob、網頁擷取與搜尋，以及全部由讀取指令組成的 Bash（`ls`、`cat`、`grep`、`git log/show/diff/status` 等） |
| `exploration_tokens` | 到第一次修改為止的模型呼叫加權 token |
| `clarifying_questions` | 這一輪的 AskUserQuestion 次數 |
| `cold_start_*` | 同樣的指標，只算每個 session 的第一輪 |

**Recall 指標。**

| 指標 | 定義 |
| --- | --- |
| `recall_injected_tokens` | 回傳給 host 的 context（記憶、persona、scene 導覽）的估算 token |
| `recall_items` | 各 layer 回傳的筆數 |
| `recall_empty` | 沒有任何記憶超過門檻 |
| `recall_used` | 抽樣判斷：這一輪的回答有沒有用到召回的內容。只在離線評估判斷（第 8 節），不在即時路徑上做 |

## 5. 資料模型

一張只增不改的事件表加上每小時彙總，兩種 storage backend 都要有。

`usage_events`：

| 欄位 | 說明 |
| --- | --- |
| `event_id`、`ts` | |
| `service_id`、`team_id`、`agent_id`、`user_id`、`session_id` | 範圍。agent 層級的工作（L2、L3）`user_id` 留空 |
| `kind` | `distill_l1`、`dedup`、`distill_l2`、`distill_l3`、`skill_extract`、`embed`、`recall`、`agent_turn` |
| `model` | provider 回報的模型，或 CLI 指令 |
| `input_tokens`、`cached_input_tokens`、`cache_write_tokens`、`output_tokens` | 可為空 |
| `estimated` | 數字是估算時為 `true` |
| `latency_ms`、`chars` | |
| `scene_id` | 工作和某個 L2 scene 相關時填寫（scene 更新、recall 命中） |
| `detail` | 小型 JSON：recall 各 layer 筆數、`exploration_calls`、generation log id |

`usage_rollup_hourly` 以小時、範圍、kind、model 為鍵，存加總和延遲分布。Dashboard 讀彙總表；原始事件用來往下鑽，保留天數可以設定。

Generation log 加上同樣的 token 欄位。`distill_*` 事件用 id 指向它的 log，不重複存一份。

## 6. 收集點

| 內容 | 位置 | 做法 |
| --- | --- | --- |
| 花費 | `LlmProvider` | `complete()` 改回傳 `Completion { text, usage: Option<Usage> }`。OpenAI 相容 provider 讀取 `usage`。CLI provider 在設定的工具支援時讀 JSON 輸出的 usage（`claude -p --output-format json`、`codex exec --json`），否則用估算。每個 runner 每次呼叫記一筆事件，和它的 generation log 一起寫 |
| Embedding | `EmbeddingProvider::embed` | provider 有回傳 usage 就用，否則估算 |
| 注入 | `RecallService::recall` | 每次呼叫記一筆 `recall` 事件：query 長度、各 layer 筆數、回傳字元數和估算 token、延遲、是否為空 |
| Agent 輪次 | `amem` 的 Stop | `amem` 本來就會讀 transcript。它在本機算出這一輪的指標（第 4 節），以小型的 `turn_stats` 物件和擷取的訊息一起送出，不會多帶任何 transcript 內容。transcript 裡沒有 usage 的 host 不送 |

`turn_stats` 預設開啟，client 端可以關閉（`AMEM_TURN_STATS=0`），內容永遠只有計數。

## 7. Dashboard

Panel 新增 **Usage** 頁面。可見範圍依照現有角色：一般成員看自己，team admin 看自己的 team，system admin 看全部。

1. **總覽**，可選時間區間：
   - 花費、注入、agent 輪次的 token；
   - 估算的節省和淨效益；
   - 每一項都有趨勢線。
2. **拆解**：依 team → user → agent 的表格，可依任何指標排序，也能往下鑽到單一 session 的事件。
3. **Pipeline**：每個 layer 每次執行的 token、p50 和 p95 延遲、失敗數、佇列深度。取代現在整個程序的總計數。
4. **Recall 品質**：
   - 命中率和空結果率；
   - 每輪注入的 token；
   - 各 layer 筆數；
   - 抽樣中被判定沒用到的比例，用來找出不相關的注入。
5. **情境**：每個 L2 scene 被召回的次數、注入的 token，以及維護它的成本。

預算之後再做：現有的 `instance-quota` 可以擴充成每個 team 的 token 預算，並在達到門檻時發出警示。

## 8. 離線評估

線上數字能看出趨勢，但每週的任務都不一樣。用一個受控的比較來校正「節省」的估算：

1. **基準線**：對啟用記憶之前的 transcript 計算第 4 節的指標。
2. **成對 A/B**：
   - 一組固定的題目：有的需要歷史資訊，有的是不需要的對照題。
   - 每題在全新的 headless session 裡，分別在記憶 plugin 開和關的情況下執行。同一個模型、同一個 commit，每組跑多次。
   - 兩組都關閉 host 自己的記憶功能。
   - 記錄第 4 節的指標，以及成本和耗時。
3. **品質**：由不知道答案來自哪一組的裁判，依關鍵事實評分。只有在品質不下降時，省下的 token 才算數。
4. **報告**：每題、每個指標的成對差異中位數。對照題可以看出注入本身的額外成本。

這套流程的第一次執行，預計在一個真實的部署上進行：匯入了 57 個 Claude Code session，10 道題目，每組跑 3 次。

## 9. 分階段

| 階段 | 範圍 | 修改 |
| --- | --- | --- |
| 0 | 離線評估和指標定義（第 4 節、第 8 節） | 只有腳本 |
| 1 | **花費和注入**：`Completion.usage`、generation log 的 token 欄位、`usage_events` 和彙總、`recall` 事件、Panel Usage 頁面（總覽、拆解、pipeline、recall） | memory-core、Panel |
| 2 | **節省**：`amem` 送出 `turn_stats`、dashboard 上的節省估算、抽樣判斷 recall 是否被使用 | amem、memory-core、Panel |
| 3 | **情境和預算**：依 scene 歸屬、每個 team 的預算與警示 | memory-core、Panel |

適合和第 1 階段一起做的修正：

- **Recall 日期**：顯示來源對話的時間，而不是 `updated_at`。
- **注入預算**：為注入設定 token 預算，並對剛好過門檻的記憶採用更嚴格的相關性標準。

## 10. 待決問題

1. **價格表**：權重由維運者依模型設定，還是自動取得？要換算成金額就需要價格表，這份設計刻意避開了這一點。
2. **沒有 usage 輸出的 CLI provider（目前是 `agy -p`）**：用字元數估算夠不夠？還是 CLI provider 應該優先選用會回報 usage 的工具？
3. 原始 `usage_events` 的**保留期限**，以及刪除 session 時要不要一起刪掉它的事件。
4. **`turn_stats` 的隱私**：雖然只有計數，但工具名稱會透露一個人的工作方式。team admin 應該看到每個人的細節，還是只看總數？
