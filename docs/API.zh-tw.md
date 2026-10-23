# MemoryCore（Rust）HTTP API

[English](API.md)

RPC 風格的 v3 API。本文件列出所有已實作的 endpoint 與回應慣例。各元件如何組合（信任區、授權流程、蒸餾管線）請見 [ARCHITECTURE.zh-tw.md](ARCHITECTURE.zh-tw.md)。

## 慣例

* 埠號 `8420`。所有 `/v3/*` endpoint 都是 `POST`，body 為 JSON。
* 回應外層（envelope）：`{ "code": 0, "message": "ok", "request_id": "req-…", "data": … }`。4xx/5xx 時 HTTP 狀態碼等於 `code`；skill 錯誤使用五位數代碼（例如 `40901`），HTTP 狀態碼為對應的三位數。
* 三個信任區，任何一區都無法跨部門：
  * **使用者範圍**：資料平面（L0–L3、`recall`、`session/add`）與 `/v3/skill/*`。依 `x-memory-user-key` 與請求的範圍授權。
  * **部門**：`meta/*`、`pipeline/*`、`stats/overview`、`memory-generation-log/*`，以及只限系統管理員的 `memory-prompt/*`、`knowledge/*`、`chat-memory/clear`。需要該部門使用者的 `x-memory-user-key`（`/v3/meta/auth/verify` 除外），由呼叫者的角色決定能做什麼、看得到什麼。伺服器 API key 不能代替使用者。
  * **營運者**：`internal/*` 與 `department/destroy`。只有 `Authorization: Bearer <AGENT_MEMORY_API_KEY>` 能使用（有設定 API key 時）。

  因此部門的管理員永遠不需要伺服器 API key，Panel 也不持有它。
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
| 401 | 營運者路由缺少或錯誤的 Bearer key；其他路由缺少 user key，或 user key 不存在、已撤銷、已過期（部門不存在時也是） |
| 403 | 不是該 team 的有效成員；agent 屬於別的 team；非系統管理者卻指定其他 `user_id`；不允許的 metadata 操作；非系統管理者呼叫 `memory-prompt/*`、`knowledge/*` 或 `chat-memory/clear` |
| 404 | 路由或資料不存在；營運者路由上部門不存在時回 `unknown_department`（先對它執行 `init-admin`） |
| 409 | 衝突，例如 `already_initialized`、`target_has_profile` |
| 413 | Body 超過 `max_body_bytes`（`session/add` 為 `max_session_body_bytes`） |
| 429 | 超過配額；client 位址因太多次被拒絕的憑證而被擋時，回 `too_many_failures` 並附 `Retry-After`（[憑證稽核紀錄](#憑證稽核紀錄)） |
| 500 | 伺服器內部錯誤；`message` 只有錯誤代碼與 request id，細節在 memory-core 的 log |
| 503 | 相依服務（LLM、資料庫）無法使用；`message` 只有代碼（例如 `LLM_UNAVAILABLE`）與 request id，細節在 log |

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
| POST | `/v3/pipeline/dead-letter/list` | `{kind?, limit?}`（`l1`、`l2`、`l3`、`skill`；預設 50，最多 500）→ `{items: [{kind, job, attempt, error, dead_at_ms, redrives, first_dead_at_ms, model}]}`，新的在前，只列此部門 |
| POST | `/v3/pipeline/dead-letter/redrive` | `{kind?, limit?}`（預設 10，最多 1000）→ `{requeued}`：把最舊的符合條件 dead letter 重新排回佇列，從第一次嘗試開始。帶 user key 時只有系統管理者可以執行 |

`WorkerMetrics`：`{tasks_enqueued, tasks_completed, tasks_failed, tasks_retried, tasks_dead_lettered, last_task_at, last_error}`。
`ScopeState`：`{scope, team_id, agent_id, user_id, session_id, conversation_count, warmup_threshold, l1_cursor, l2_pending, last_l1_at, last_l2_at, last_l3_at, memories_since_persona}`。

每一次呼叫到 LLM 的 L1、L2、L3 執行，都會留下一筆生成紀錄，帶有 `status`：
- `succeeded`：寫出了記憶、scene 或 persona；
- `empty`：LLM 有回應，但沒有寫出任何東西；
- `failed`：呼叫或回應失敗，`error_message` 說明原因。
- `dropped`：L1 或 L2 在最後一次嘗試仍失敗；這批輸入被略過，讓後面的內容可以繼續，dead letter 裡的 job（帶 `range`）可以重做它。

`model` 是使用的 LLM，包含 CLI provider 選用的模型（例如 `cli:agy:gemini-3.8-flash-high`）。每次嘗試都失敗的 job 會成為 **dead letter**，連同最後一次的錯誤保留下來，直到被重跑為止。`coordination.backend: local` 時，dead letter 存在記憶體中（最多 1,000 筆），重新啟動後就會消失。同樣的資訊也可以在 Panel 的 dashboard，以及 `memory-server generations`、`memory-server dead-letters` 查看（[docs/ops/DISTILLATION.zh-tw.md](ops/DISTILLATION.zh-tw.md)）。

這些路由需要 `x-memory-user-key`。範圍、生成紀錄和 dead letter 只會列出該使用者角色允許的部分：系統管理員看得到整個部門，team admin 或 leader 看得到整個 team，member 看得到 team 共用的 L2/L3 工作和自己的工作（[COLLABORATION.zh-tw.md 第 12 節](COLLABORATION.zh-tw.md#12-依角色檢視運作狀態)）。`stats/overview` 對任何成員都統計整個部門。營運者看整個部門的檢視（`memory-server generations` 與 `memory-server dead-letters` 使用）在 `/v3/internal/` 底下（見下方）。

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
* `session/add`：把原始的 session 回合物件存到 `sessions/<source>/<team>/<agent>/<user>/<session>/` 底下，並在物件裡記錄 `agent`、`user`、`session`。client 送 `x-memory-session-id`；team、agent 與使用者取自授權後的範圍（`team_id`／`agent_id` 和其他路由一樣用來指定範圍），所以私人 agent 的回合只屬於它的擁有者（COLLABORATION §9）。Body：`source`（`online` 或 `offline`）、`started_at`、`ended_at`（`YYYY-MM-DDTHH:MM:SS.sssZ`）、`user_query`、`model`、`messages`（不可為空）、`output`（物件），以及選填的 `subagent`；其他欄位（包括 `agent`、`user`、`session`）一律回 422。每筆回合最大 `max_session_body_bytes`；總量目前沒有配額或保留期限。
* L0/L1 以 team + agent + user 劃分範圍；L2（`scenario/*`）與 L3（`core/*`）只以 team + agent 劃分。
* 場景和 persona 只追加版本。`scenario/write`、`core/write` 會新增一個以呼叫者為作者的版本；內容與最新版本完全相同時不會新增，直接回傳最新版本。`scenario/rm` 會讓場景退役：它不再出現在 `ls`、`count` 和不帶 `version` 的 `read`，但帶 `version` 的 `read` 仍能取得它的任何版本；被合併掉的場景也一樣。
* 每個場景都有固定的 `scene_id`（出現在 `ls`、`read`、`write` 的回應中）。用 `path` 的 `read` 找的是「現在」叫這個名稱的場景；名稱被另一個場景用掉之後，要用 `scenario/read {scene_id, version?}` 讀舊場景，這個方式也會回傳已退役的場景。`read` 另外回傳該版本的 `op`（`create`、`update`、`merge`、`retire`、`manual`、`import`）、`parents`（合併時併入的版本）和 `author`。
* `core/read` 不帶 `version` 時，回傳最新的 persona，後面附上依目前場景產生的場景導覽。帶 `version` 時（即使是最新版的號碼），只回傳該版本的內文：導覽不屬於儲存的版本。

### Skills

`/v3/skill/{create,update,patch,delete,get,get-by-name,list,search,versions,listing,extract}`、
`/v3/skill/files/{write,remove,read}`、
`/v3/skill/conversation/{add,force-archive}`、
`/v3/skill/proposal/{list,approve,reject}`。
`/v3/skill/export` 尚未實作（回傳 501）。

Skill 萃取只提案，在有人核准之前什麼都不會存下：

* `skill/proposal/list` `{team_id, agent_id?}` → `{items: [{task_id, team_id, agent_id, user_id, session_id, archived_at_ms, reason, proposals: [{index, change, base_version?, state}]}]}`，最新的在前。`change` 是 `{action: "create", name, content}`、`{action: "update", skill_id, content}` 或 `{action: "patch", skill_id, old_string, new_string}`。列出呼叫者可以審核的待核准提案，依記憶的規則判斷（提案是從某人的對話萃取出來的）：自己能使用的 agent 上、自己對話的提案（私人 agent 只屬於它的擁有者），自己擔任 admin 的 team 裡的所有提案，系統管理員則是全部。team leader 能看營運狀態，但不是審核者。已封存的 agent 一律不列出、也不能決定。提案存下前已經過遮罩；未決定的提案會在 `skill.extraction.proposalTtlDays`（預設 30 天）之後過期，不再列出（`state: "expired"`；決定它會得到 404 `skill_proposal_expired`）。
* `skill/proposal/approve` `{team_id, agent_id, task_id, index}` → `state: "applied"` 的提案，附 `result {action, skill_id, version}`。修改以該對話的 agent 與使用者身分進行。規則依 agent 現在的狀態判斷（之後被封存或改成私人的也算）。skill 在 `base_version` 之後被改過時回 409 `skill_changed`；其他人回 403；已決定過的回 404。同一個 task 的決定一次只處理一個。
* `skill/proposal/reject` `{team_id, agent_id, task_id, index}` → `state: "rejected"` 的提案。

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
`/v3/meta/config/user/{get,set}`、
`/v3/meta/redaction-rule/{list,create,update,disable,enable,test}`（僅限系統管理者；見 [DISTILLATION §7](ops/DISTILLATION.zh-tw.md#7-機密與遮罩)）。

Meta 的列表回應格式：`{items, total, limit, offset}`；`redaction-rule/list` 例外，回傳 `{builtin, deployment_password_shape, deployment, department}`。

呼叫端自己指定的 user key（`user/create-with-key`、`init-admin` 的 `user_key`）必須是 `sk-mem-` 加上至少 32 個英數字、`-` 或 `_`。`agent-fixed-asset/set` 以呼叫者作為每個 binding 的 `created_by`。Agent 的擁有者只有在仍是該 team 的有效成員時，才能管理它（`agent/update`、`agent/delete`、`agent-fixed-asset/set`）。

### 憑證稽核紀錄

memory-core 依 client 位址計算被拒絕的憑證：錯誤、已撤銷或已過期的 user key、錯誤的伺服器 API key，以及回答 `valid: false` 的 `meta/auth/verify`。15 分鐘內從第 5 次起，每次拒絕都會晚一點回應（250 ms，倍增到最多 5 秒）；第 20 次會讓這個位址被擋 15 分鐘，期間它的每個請求都回 429 `too_many_failures` 並附 `Retry-After`（秒）。位址是連線本身的位址，或是 `AGENT_MEMORY_TRUSTED_PROXIES` 列出的代理在 `X-Forwarded-For` 指出的 client。

`/v3/audit/list` `{kind?, since_ms?, limit?, offset?}` → `{items}`（部門的系統管理者；只有自己部門的事件，新的在前；`limit` 預設 50，最大 500）。`kind` 為 `credential_rejected`、`source_locked` 或 `key_issued`。每一筆：`{event_id, at_ms, kind, department, source, forwarded_for, user_agent, path, credential, request_id, detail}`；`credential` 是 `user key sha256:<雜湊前 8 個 hex>` 或 `server API key`，絕不是 key 本身。

### 內部與維運（只接受伺服器 API key）

* `/v3/internal/pipeline/dead-letter/{list,redrive}`、`/v3/internal/memory-generation-log/{list,summary}`：與去掉 `internal/` 的路由相同，涵蓋整個部門；`memory-server dead-letters` 與 `memory-server generations` 呼叫的就是這些。
* `/v3/internal/audit/list` `{department_id?, kind?, since_ms?, limit?, offset?}`：所有部門的憑證稽核紀錄，或只看 `department_id` 的（不存在的部門也可以）；不需要 `x-memory-department-id`。

* `/v3/internal/meta/user/init-admin` `{username, user_key?}` → `{user_id, user_key, team_id, agent_id}`：第一次啟動時的初始化，建立一位系統管理者，以及 `default-team` 與 `default-agent-<username>`。只要已有任何使用者，就回 409 `already_initialized`；同時呼叫也只會建立一位管理者；會寫進稽核紀錄。這是唯一會建立部門的路由：對不存在的部門，其他路由一律拒絕（資料面與部門路由回與 user key 錯誤相同的 401，`meta/auth/verify` 回 `valid: false`，營運者路由回 404 `unknown_department`）。
* `/v3/internal/meta/user/list-by-department`。
* `/v3/internal/migrate/default-scope` `{team_id, agent_id, user_id, from_team_id?, from_agent_id?, from_user_id?, dry_run?}` → `{dry_run, to, moved: {conversations, atoms, profile_files, skill_versions}, session_states, profile_states}`：升級輔助工具，把寫在舊 `default` 桶的資料（`default/default/default` 的 L0/L1，以及 `default/default` 的 L2/L3 和 skills）連同管線游標搬到真正的範圍。0.1.1 之前若 client 有指定 agent 或使用者，資料會寫在 `default/<agent>/<user>`；這時用 `from_team_id`／`from_agent_id`／`from_user_id` 指定來源桶（未填的部分都是 `default`）。條件：agent 必須屬於該 team、使用者必須是有效成員，而且 agent 還不能有 L2/L3（否則回 409 `target_has_profile`）。這項檢查只在仍有舊資料時進行，所以搬移成功後重跑不會有任何動作。`dry_run: true` 會 rollback，只回報筆數。
* `/v3/internal/upgrade` `{admin_username?, team_name?, agent_name?, issue_key?, dry_run?}` → `{dry_run, user_id, username, team_id, team_name, team_created, agent_id, agent_name, agent_created, moved, session_states, profile_states, user_key?}`：`upgrade.sh` 呼叫的就是這個。它會：
  * 選定系統管理者（預設是最早建立的那位）；
  * 以完全相同的名稱找 team（預設 `default-team`）和 agent（預設 `default-agent-<admin>`），找不到就建立；
  * 以該管理者身分執行 default-scope 遷移；
  * 帶 `issue_key` 時，另外發一把新的 user key，只回傳這一次，並寫進稽核紀錄。沒有 legacy 資料要搬時 `issue_key` 會被拒絕（403），除非設了 `AGENT_MEMORY_ALLOW_KEY_RECOVERY=true`。

  Dry run 不會建立任何東西；將會新建的 team 或 agent，其 `team_id`/`agent_id` 回傳 `null`。可以安全地重複執行。
* `/v3/department/destroy` `{department_id}` → `{department_id, cleaned: {storage, state, dead_letters}}`；不需要 `x-memory-department-id`。

## 設計決策

* 不提供任何廠商專屬的整合：沒有 analytics、外部任務來源、上游模型群組路由。
* 正確性保證：`atomic/update` 以 id 查找；召回與搜尋一律套用範圍隔離；搜尋會遵守 `time_start`/`time_end`；`scenario/ls` 會驗證 `path_prefix`；新的 L1 紀錄從版本 1 開始。
* L2 場景與 L3 persona 的生成採用結構化 JSON 操作，而不是 agentic tool loop。Prompt 依循既定策略：UPDATE > MERGE > CREATE、熱度、場景上限、META 標頭、場景導覽。
