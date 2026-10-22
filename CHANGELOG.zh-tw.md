# 變更紀錄

[English](CHANGELOG.md)

各版本的重要變更。細節與升級步驟見 [release notes](https://github.com/hsuanshao/AgentMemory/releases)；跨版本升級見 [UPGRADE.zh-tw.md](docs/UPGRADE.zh-tw.md)。格式依照 [Keep a Changelog](https://keepachangelog.com/zh-TW/1.1.0/)，版號依照[語意化版本](https://semver.org/lang/zh-TW/)（1.0 之前，次版本可能改變資料或 API，並會在說明中註明）。

## [未發布]

### 新增
- memory-core 遇到由更新版本寫入的資料（SQLite `user_version`、Postgres `schema_migrations`、Redis `schema_version`）時拒絕啟動，錯誤訊息會提示改跑新版本或還原備份。這次升級後要退回 0.4.0，必須還原備份（[UPGRADE](docs/UPGRADE.zh-tw.md#040-之後未發布)）。

### 變更
- **場景（L2）和 persona（L3）改成只追加的版本儲存**（`scenes`、`scene_versions`、`persona_versions`），不再覆寫檔案。L2 更新、合併或 `/v3/scenario/rm` 都會新增一個版本，不會刪除任何版本。每個場景有固定的 id；合併會記錄它併入了哪些場景，並把那些場景標為退役，所以被合併或移除的場景仍可以用 `version` 讀取。關鍵欄位（名稱、摘要、熱度、版本的由來、作者）是資料表欄位，內文維持 markdown，其他屬性原樣保留。schema 遷移 3 會在啟動時匯入 `profile_files`，並保留這張表。
- persona 不再因為場景導覽改變就多一個版本：導覽改在讀取 persona 時，依目前的場景產生。
- `/v3/scenario/write` 和 `/v3/core/write` 會把呼叫的使用者記為新版本的作者。內容與最新版本完全相同的寫入不會新增版本，直接回傳最新版本（作者仍是原本的作者）。清除 chat memory 仍是唯一會刪除場景與 persona 歷史的操作。
- 場景在 `/v3/scenario/{ls,read,write}` 中有固定的 `scene_id`；`/v3/scenario/read` 接受 `scene_id`，可以讀到名稱已被其他場景用掉的舊場景。`/v3/core/read` 帶 `version` 時只回傳該版本的內文，不附場景導覽。
- L2 執行期間，如果 API 寫入先用掉了 L2 要建立的場景名稱，L2 會寫進那個既有場景，不會讓整批失敗。

## [0.4.0] - 2026-10-22

### 新增
- memory-core 在 `claude`、`codex` 蒸餾前先關掉它們自帶的工具，設定為 `llm.cli.hardening`（`AGENT_MEMORY_LLM_CLI_HARDENING`，預設 `auto`）；每個 CLI 也會在 prompt 裡被告知沒有工具（#37）。
- 自架的 OpenAI 相容模型回傳工具呼叫（`tool_calls`、`function_call` 或工具呼叫格式的文字，且沒有答案）時，以新的錯誤代碼 `LLM_TOOL_REQUEST` 失敗，直接 drop、不重試，和 CLI 的工具被拒絕一樣；若同時有答案，就拿掉工具呼叫的部分後使用（#37）。
- `memory-server check-config` 只檢查設定、不啟動；`upgrade.sh` 會在啟動新版本前用它檢查；設定被拒絕，或新版本通過 `/health` 之前任何一步失敗時，會還原 `.env`，並用原本的 image 重新執行舊版本。
- 蒸餾用的 codex 以 `--ignore-user-config` 和唯讀 sandbox 執行：不再讀 `config.toml`，所以裡面的 MCP server 和 profile 都不會載入。需要的設定請改放到 `AGENT_MEMORY_LLM_CLI_ARGS`（#37）。

### 變更
- **授權：從 0.4.0 版開始，server 端由 MIT 改為 Business Source License 1.1。** 評估測試、個人與非營利使用，以及最多 1 個部門、10 位成員的組織可以免費使用；更大規模的正式環境使用需要商業授權。每個版本發布滿四年後轉為 Apache 2.0。`amem` client（`crates/memory-client/`）和 plugin（`clients/`）維持 MIT，0.3.2 以前的版本也維持 MIT。詳見 [COMMERCIAL.zh-tw.md](COMMERCIAL.zh-tw.md)。
- **`llm.cli.args` 讓蒸餾用的 CLI 拿到比預設更多的能力時，memory-core 不會啟動，例如 `--dangerously-skip-permissions`、`--allowedTools` 或可寫入的 codex sandbox。錯誤訊息會指出是哪個參數（#37）。** 包括 codex 的 `--search`、`--profile`，以及會加入 MCP server、網路搜尋、feature 或可寫入 sandbox 的 `-c` 設定。 升級前請先檢查 `AGENT_MEMORY_LLM_CLI_ARGS`（[UPGRADE](docs/UPGRADE.zh-tw.md#升級到-040)）。
- L1 抽取時會看到這個 session 最新的 10 筆 episodic 記憶（最多 3,000 字元）。新記憶如果讓其中某筆過時，會在 `supersedes` 列出它，那筆記憶會被就地改寫，不再和新消息並存（#43）。
- L2 把 scene 的摘要和內文寫成目前狀態，不再寫成這一批的變更紀錄，並移除已解決的待解問題（#43）。
- CLI 的工具被拒絕時，改回報為 `LLM_TOOL_REQUEST`，不再是 `LLM_UNAVAILABLE`；之前寫入的 dead letter 仍然視為不可重試。

### 修正
- 原始碼來自壓縮檔（git archive、release tarball）且檔案時間早於快取的建置時，Docker 建置可能沿用舊的 `memory-server` 執行檔；現在只要原始碼有變動，建置一定會重新編譯 workspace。

## [0.3.2] - 2026-10-21

### 新增
- `amem` 會擷取 Claude Code 工作期間輸入的訊息，當成目前這一輪的一部分（#40）。
- Dead letter 一天後或重跑 3 次後到期，並在 1、2、4 小時後自動重跑；設定為 `worker.deadLetter*`（#41）。

### 變更
- 蒸餾時工具被拒絕不再重試；L1/L2 直接 drop 該批輸入（#41）。

### 修正
- 重跑時寫入佇列失敗，同一批其餘的 dead letter 可能遺失（#41）。

## [0.3.1] - 2026-10-20

### 變更
- L1/L2 會 drop 每次嘗試都失敗的那一批，session 或 agent 不再被卡住；dead letter 可以只重做那一段（#38）。

### 修正
- `upgrade.sh` 用版本相符的 `pg_dump` 備份外部 PostgreSQL 17 以上（#35）。

## [0.3.0] - 2026-10-20

### 變更
- **不相容變更**：「instance」改名為「部門（department）」：header `x-memory-department-id`、`amem` 的 `department_id`、`.env` 的 `CORE_DEPARTMENT_ID`，以及 API 路由。client 和伺服器必須一起更新（#32）。
- Team 角色改為 `admin`、`leader`、`member`；管線狀態、生成紀錄和 dead letter 依角色篩選（#33）。

### 新增
- 有版本的儲存遷移，啟動時自動執行，也可用 `memory-server migrate`（#32）。

## [0.2.3] - 2026-10-20

### 新增
- 記錄失敗和空的蒸餾執行；dead letter 保留錯誤原因，可以從 CLI 和 Panel 重跑（#28）。
- `amem` 的 `skip_prompt_prefixes`（#27）。

### 修正
- `migrate-to-postgres.sh` 會採用指令上傳入的 `AGENT_MEMORY_POSTGRES_URL`（#25）。

## [0.2.2] - 2026-10-20

### 修正
- PostgreSQL 可以儲存含 U+0000 的訊息（#23）。
- `amem` 會切分過長的回覆和 session 以符合 Core 的限制（#22），並送出每則訊息自己的時間（#21）。

## [0.2.1] - 2026-10-20

### 修正
- CLI LLM 的失敗原因更清楚，且不會把被蒸餾的對話寫進記錄（#17、#18、#19）。

### 變更
- 專案改用 MIT 授權。

## [0.2.0] - 2026-10-20

### 新增
- 從 SQLite 搬移到 PostgreSQL（#12）。
- 預先編譯好的 Linux 和 macOS 版 `amem`，以及 GHCR 上的容器映像（#15）。
- 每個 pull request 都會跑 CI（#14）。

### 修正
- 模型漏掉結尾括號時修復 LLM 的 JSON（#3）。
- 升級 Panel 相依套件，清除所有 audit 警告（#13）。

## [0.1.1] - 2026-10-19

第一個公開版本：memory-core、L0→L3 蒸餾管線、混合召回、metadata 平面、Monitor Panel、`amem` 以及 Claude Code、Codex、Antigravity、Hermes 的 plugin，還有 `install.sh`。client 使用每位使用者各自的 key 驗證。

[未發布]: https://github.com/hsuanshao/AgentMemory/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.4.0
[0.3.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.2
[0.3.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.1
[0.3.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.0
[0.2.3]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.3
[0.2.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.2
[0.2.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.1
[0.2.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.0
[0.1.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.1.1
