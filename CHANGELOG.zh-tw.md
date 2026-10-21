# 變更紀錄

[English](CHANGELOG.md)

各版本的重要變更。細節與升級步驟見 [release notes](https://github.com/hsuanshao/AgentMemory/releases)；跨版本升級見 [UPGRADE.zh-tw.md](docs/UPGRADE.zh-tw.md)。格式依照 [Keep a Changelog](https://keepachangelog.com/zh-TW/1.1.0/)，版號依照[語意化版本](https://semver.org/lang/zh-TW/)（1.0 之前，次版本可能改變資料或 API，並會在說明中註明）。

## [未發布]

### 變更
- **授權：從 0.4.0 版開始，server 端由 MIT 改為 Business Source License 1.1。** 評估測試、個人與非營利使用，以及最多 1 個部門、10 位成員的組織可以免費使用；更大規模的正式環境使用需要商業授權。每個版本發布滿四年後轉為 Apache 2.0。`amem` client（`crates/memory-client/`）和 plugin（`clients/`）維持 MIT，0.3.2 以前的版本也維持 MIT。詳見 [COMMERCIAL.zh-tw.md](COMMERCIAL.zh-tw.md)。

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

[未發布]: https://github.com/hsuanshao/AgentMemory/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.2
[0.3.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.1
[0.3.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.0
[0.2.3]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.3
[0.2.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.2
[0.2.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.1
[0.2.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.0
[0.1.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.1.1
