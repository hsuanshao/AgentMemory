# MemoryCore（Rust）HTTP API

[English](API.md)

RPC 風格的 v3 API。本文件列出所有已實作的 endpoint 與回應慣例。各元件如何組合（信任區、授權流程、蒸餾管線）請見 [ARCHITECTURE.zh-tw.md](ARCHITECTURE.zh-tw.md)。

## 慣例

* 埠號 `8420`。所有 `/v3/*` endpoint 都是 `POST`，body 為 JSON。
* 回應外層（envelope）：`{ "code": 0, "message": "ok", "request_id": "req-…", "data": … }`。4xx/5xx 時 HTTP 狀態碼等於 `code`；skill 錯誤使用五位數代碼（例如 `40901`），HTTP 狀態碼為對應的三位數。
* 兩種信任區：
  * **使用者範圍**：資料平面（L0–L3、`recall`、`session/add`）與 `/v3/skill/*`。只用 `x-memory-user-key` 授權，不需要 Bearer key，因此遠端 client 永遠不會拿到 伺服器 API key。
  * **Bearer 範圍**：其餘所有路由（pipeline/stats、logs、prompts、knowledge、`meta/*`、`internal/*`、`department/destroy`）。有設定 API key 時需要 `Authorization: Bearer <AGENT_MEMORY_API_KEY>`。`/v3/meta/*` 另外還需要 `x-memory-user-key`（`/v3/meta/auth/verify` 除外）。
* `x-memory-department-id: <department id>`：除了 `/v3/department/destroy`，每個 `/v3/*` 路由都必須帶。安裝程式預設使用 `default`。
* 隔離範圍：body 中的 `team_id`/`agent_id`/`user_id`/`task_id`/`session_id`，或 `x-memory-team-id` / `x-memory-agent-id` / … header。兩者都有時以 body 為準。
* 資料平面授權（`crates/memory-domain/src/meta/memory_access.rs`）：
  * `team_id` 和 `agent_id` 必填（已沒有 `default` 桶），且 agent 必須屬於該 team。缺少時回 400，不相符時回 403。
  * `user_id` 預設為呼叫者本人。一般成員只能讀寫自己的記憶；只有系統管理者能指定其他使用者。
  * 呼叫者必須是該 team 的有效成員（系統管理者除外）。
  * 授權後的範圍會在 handler 執行前寫回 body 與 header，所以 handler 只會看到已授權的範圍。
* 分頁：`limit`（預設 20，最大 100）與 `offset`。

### 錯誤

HTTP 狀態碼等於 envelope 中的 `code`，`message` 說明原因。

| 狀態碼 | 情況 |
| --- | --- |
| 400 | JSON 或輸入不合法；資料平面缺少 `team_id` / `agent_id` |
| 401 | 缺少或錯誤的 Bearer key；user key 不存在、已撤銷或已過期 |
| 403 | 不是該 team 的有效成員；agent 屬於別的 team；非系統管理者卻指定其他 `user_id`；不允許的 metadata 操作 |
| 404 | 路由或資料不存在 |
| 409 | 衝突，例如 `already_initialized`、`target_has_profile` |
| 413 | Body 超過 `max_body_bytes`（`session/add` 為 `max_session_body_bytes`） |
| 429 | 超過配額 |
| 503 | 相依服務（LLM、資料庫）無法使用 |

Skill 錯誤使用五位數代碼，HTTP 狀態碼為對應的三位數，例如 `40301` 非擁有者、`40401` 不存在、`40901` 版本過舊、`41301` 太大。

## Client 快速上手

以下是 `amem` 實際送出的請求，改用一般 HTTP 表示。每個資料平面呼叫都要帶 user key、部門 id，以及 team 與 agent（放在 body，或用 `x-memory-team-id` / `x-memory-agent-id` header）：

```bash
H=(-H "x-memory-user-key: sk-mem-…" -H "x-memory-department-id: default" -H "content-type: application/json")

# 回合開始前召回
curl -sS -X POST https://memory.example.com/v3/recall "${H[@]}" \
  -d '{"team_id":"team-…","agent_id":"agt-…","query":"release plan","max_results":8}'

# 回合結束後寫入文字（→ L0，背景再蒸餾成 L1–L3）
curl -sS -X POST https://memory.example.com/v3/conversation/add "${H[@]}" \
  -d '{"team_id":"team-…","agent_id":"agt-…","session_id":"s-1",
       "messages":[{"role":"user","content":"…"},{"role":"assistant","content":"…"}]}'

# 同一回合連同 tool call（→ skill buffer，蒸餾成 skill）
curl -sS -X POST https://memory.example.com/v3/skill/conversation/add "${H[@]}" \
  -d '{"team_id":"team-…","agent_id":"agt-…","session_id":"s-1",
       "messages":[{"role":"user","content":"build it"},
                   {"role":"tool_call","content":"{\"command\":\"cargo build\"}","tool_name":"Bash","tool_call_id":"t1"},
                   {"role":"tool_result","content":"Finished","tool_name":"Bash","tool_call_id":"t1"},
                   {"role":"assistant","content":"Done."}]}'
```

範例中省略了 `user_id`，預設就是 key 的擁有者。Tool 訊息需要 `tool_call_id`，用來配對 call 與 result。

## Endpoint 一覽

### 健康檢查與監控

| Method | 路徑 | 說明 |
| --- | --- | --- |
| GET | `/health` | `{status, version, uptime, stores{vectorStore,embeddingService,ftsAvailable,llmConfigured}, services{pipelineWorker, stateBackend}}` |
| POST | `/v3/pipeline/status` | `{limit?}`（預設 50，最多 500）→ `{worker: WorkerMetrics, queue: {pending, running, dead_letter}, scopes: ScopeState[]}`。每一列範圍都有唯一的 `key`、各個 id、`team_name` / `agent_name` / `username`，以及 `can_read_memory` |
| POST | `/v3/stats/overview` | 此部門各層的筆數：`{l0, l1: {total, episodic, persona, instruction}, l2, l3, skills, generation_logs}` |
| POST | `/v3/memory-generation-log/summary` | `{hours?}`（預設 24，最多 720）→ `{hours, from_ms, llm: {configured, model}, items: [{layer, status, count}]}`：各層、各狀態的蒸餾次數 |
| POST | `/v3/pipeline/dead-letter/list` | `{kind?, limit?}`（`l1`、`l2`、`l3`、`skill`；預設 50，最多 500）→ `{items: [{kind, job, attempt, error, dead_at_ms}]}`，新的在前，只列此部門 |
| POST | `/v3/pipeline/dead-letter/redrive` | `{kind?, limit?}`（預設 10，最多 1000）→ `{requeued}`：把最舊的符合條件 dead letter 重新排回佇列，從第一次嘗試開始。帶 user key 時只有系統管理者可以執行 |

`WorkerMetrics`：`{tasks_enqueued, tasks_completed, tasks_failed, tasks_retried, tasks_dead_lettered, last_task_at, last_error}`。
`ScopeState`：`{scope, team_id, agent_id, user_id, session_id, conversation_count, warmup_threshold, l1_cursor, l2_pending, last_l1_at, last_l2_at, last_l3_at, memories_since_persona}`。

每一次呼叫到 LLM 的 L1、L2、L3 執行，都會留下一筆生成紀錄，帶有 `status`：
- `succeeded`：寫出了記憶、scene 或 persona；
- `empty`：LLM 有回應，但沒有寫出任何東西；
- `failed`：呼叫或回應失敗，`error_message` 說明原因。
- `dropped`：L1 或 L2 在最後一次嘗試仍失敗；這批輸入被略過，讓後面的內容可以繼續，dead letter 裡的 job（帶 `range`）可以重做它。

`model` 是使用的 LLM，包含 CLI provider 選用的模型（例如 `cli:agy:gemini-3.8-flash-high`）。每次嘗試都失敗的 job 會成為 **dead letter**，連同最後一次的錯誤保留下來，直到被重跑為止。`coordination.backend: local` 時，dead letter 存在記憶體中（最多 1,000 筆），重新啟動後就會消失。同樣的資訊也可以在 Panel 的 dashboard，以及 `memory-server generations`、`memory-server dead-letters` 查看（[docs/ops/DISTILLATION.zh-tw.md](ops/DISTILLATION.zh-tw.md)）。

這些路由可以另外帶 `x-memory-user-key`。沒有帶時（維運人員、`memory-server` 指令）涵蓋整個部門；有帶時，範圍、生成紀錄和 dead letter 只會列出該使用者角色允許的部分：team admin 或 leader 看得到整個 team，member 看得到 team 共用的 L2/L3 工作和自己的工作（[COLLABORATION.zh-tw.md 第 12 節](COLLABORATION.zh-tw.md#12-依角色檢視運作狀態)）。Panel 一律會送出登入使用者的 key。

### 召回

| 路徑 | 說明 |
| --- | --- |
| `/v3/recall` | Body `{query, max_results?, strategy?}` 加上範圍。回傳 `{strategy, memories: [{id, type, content, score}], prepend_context, append_system_context}`。各整合直接注入 `prepend_context` 與 `append_system_context`（persona、場景導覽） |

### 資料平面（L0–L3）

`/v3/conversation/{add,query,search,delete,count}`、
`/v3/atomic/{update,query,search,delete,count}`、
`/v3/scenario/{ls,read,write,rm,count}`、
`/v3/core/{read,write,count}`、`/v3/session/add`。

* `conversation/add` `{session_id, messages: [{role: user|assistant, content, timestamp?}]}`：寫入 L0 並驅動蒸餾管線。
* `session/add`：儲存原始的 session 回合物件（需要 `x-memory-user-id` 與 `x-memory-session-id`，由 middleware 依授權後的範圍設定）。
* L0/L1 以 team + agent + user 劃分範圍；L2（`scenario/*`）與 L3（`core/*`）只以 team + agent 劃分。

### Skills

`/v3/skill/{create,update,patch,delete,get,get-by-name,list,search,versions,listing,extract}`、
`/v3/skill/files/{write,remove,read}`、
`/v3/skill/conversation/{add,force-archive}`。
`/v3/skill/export` 尚未實作（回傳 501）。

### Knowledge metadata、chat memory、prompts、生成紀錄

`/v3/knowledge/{create,get,update,delete,list}`、
`/v3/chat-memory/clear`、
`/v3/memory-prompt/{create,get,update,delete,set,setting/list,log}`、
`/v3/memory-generation-log/{list,get,summary}`（`list` 接受 `{layer?, status?, start_time?, end_time?, limit?, cursor?}`）。

### Metadata 平面（需要 `x-memory-user-key`）

`/v3/meta/user/{create,create-with-key,get,delete,list}`、
`/v3/meta/user-key/{create,list,get,revoke,update}`、
`/v3/meta/team/{create,get,update,delete,list}`（`list` 帶 `"all": true` 會列出所有 team，僅限系統管理者）、
`/v3/meta/team-member/{add,remove,list,get}`、
`/v3/meta/agent/{create,get,update,delete,list,archive}`、
`/v3/meta/task/{create,get,update,delete,list,archive}`、
`/v3/meta/task-agent/{link,unlink,list}`、
`/v3/meta/participation-log/{append,list}`、
`/v3/meta/asset/{create,get,update,delete,list,list-accessible,touch-usage}`、
`/v3/meta/agent-fixed-asset/{set,list,list-with-detail,summary-by-agents}`、
`/v3/meta/acl/{grant,revoke,list,check}`、
`/v3/meta/auth/verify`、`/v3/meta/department-quota/get`、
`/v3/meta/config/user/{get,set}`。

Meta 的列表回應格式：`{items, total, limit, offset}`。

### 內部與維運（僅需 Bearer）

* `/v3/internal/meta/user/init-admin` `{username, user_key?}` → `{user_id, user_key, team_id, agent_id}`：第一次啟動時的初始化，建立一位系統管理者，以及 `default-team` 與 `default-agent-<username>`。只要已有任何使用者，就回 409 `already_initialized`。
* `/v3/internal/meta/user/list-by-department`。
* `/v3/internal/migrate/default-scope` `{team_id, agent_id, user_id, from_team_id?, from_agent_id?, from_user_id?, dry_run?}` → `{dry_run, to, moved: {conversations, atoms, profile_files, skill_versions}, session_states, profile_states}`：升級輔助工具，把寫在舊 `default` 桶的資料（`default/default/default` 的 L0/L1，以及 `default/default` 的 L2/L3 和 skills）連同管線游標搬到真正的範圍。0.1.1 之前若 client 有指定 agent 或使用者，資料會寫在 `default/<agent>/<user>`；這時用 `from_team_id`／`from_agent_id`／`from_user_id` 指定來源桶（未填的部分都是 `default`）。條件：agent 必須屬於該 team、使用者必須是有效成員，而且 agent 還不能有 L2/L3（否則回 409 `target_has_profile`）。這項檢查只在仍有舊資料時進行，所以搬移成功後重跑不會有任何動作。`dry_run: true` 會 rollback，只回報筆數。
* `/v3/internal/upgrade` `{admin_username?, team_name?, agent_name?, issue_key?, dry_run?}` → `{dry_run, user_id, username, team_id, team_name, team_created, agent_id, agent_name, agent_created, moved, session_states, profile_states, user_key?}`：`upgrade.sh` 呼叫的就是這個。它會：
  * 選定系統管理者（預設是最早建立的那位）；
  * 以完全相同的名稱找 team（預設 `default-team`）和 agent（預設 `default-agent-<admin>`），找不到就建立；
  * 以該管理者身分執行 default-scope 遷移；
  * 帶 `issue_key` 時，另外發一把新的 user key，只回傳這一次。

  Dry run 不會建立任何東西；將會新建的 team 或 agent，其 `team_id`/`agent_id` 回傳 `null`。可以安全地重複執行。
* `/v3/department/destroy` `{department_id}`。

## 設計決策

* 不提供任何廠商專屬的整合：沒有 analytics、外部任務來源、上游模型群組路由。
* 正確性保證：`atomic/update` 以 id 查找；召回與搜尋一律套用範圍隔離；搜尋會遵守 `time_start`/`time_end`；`scenario/ls` 會驗證 `path_prefix`；新的 L1 紀錄從版本 1 開始。
* L2 場景與 L3 persona 的生成採用結構化 JSON 操作，而不是 agentic tool loop。Prompt 依循既定策略：UPDATE > MERGE > CREATE、熱度、場景上限、META 標頭、場景導覽。
