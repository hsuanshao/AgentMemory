# Agent Memory 

[English](./README.md) | 繁體中文

![Agent-memory](./docs/imgs/agent-memory.jpg "agent memory")

以 Rust 實作的 Agent 記憶核心服務（**memory-core**），並附一個以 TypeScript 開發的 **Monitor Panel**。

- 遵循 Clean Architecture：`domain ← application ← infrastructure / gateway ← server`
- 特點：L0→L3 分層記憶、非同步蒸餾管線、BM25 + 向量 + RRF 混合召回、團隊/Agent/使用者隔離、Skill 記憶、資產與 ACL 元資料平面、v3 RPC API
- 不依賴任何雲廠商專屬服務：儲存使用 SQLite + 本地檔案，LLM 與 Embedding 使用任何 OpenAI 相容端點（可指向自架模型）

## 快速開始

```bash
./install.sh
```

安裝程式會依序詢問：

1. **儲存後端**：SQLite（預設，免安裝）／既有的 PostgreSQL（輸入 URL，先從容器內測試連線）／幫我用 Docker 啟動 PostgreSQL（pgvector）
2. **協調後端**：單機（預設）／既有的 Redis／幫我用 Docker 啟動 Redis
3. **LLM**：OpenAI 相容 API／Claude Code CLI／Antigravity CLI／Codex CLI
4. CLI 的專案目錄與對外 port

接著它會產生 Gateway API key 與資料庫密碼、寫入權限 600 的 `.env`（含 `COMPOSE_PROFILES`）、啟動服務並等待健康檢查，最後建立第一位管理員並顯示一次登入用的 user key。

- 自動化部署：`INSTALL_STORAGE=postgres-docker INSTALL_COORDINATION=redis-docker ./install.sh --non-interactive`（變數說明見 `./install.sh --help`）。
- 之後重啟只需 `docker compose up -d`，`.env` 會帶上同樣的 profile 與設定。
- 本機上的資料庫請以 `host.docker.internal` 取代 `localhost`。

| 服務 | 位址 | Dockerfile | 何時啟動 |
| --- | --- | --- | --- |
| memory-core | http://localhost:8420 | `docker/core.Dockerfile` | 永遠 |
| memory-panel | http://localhost:8125 | `panel/Dockerfile` | 永遠 |
| postgres（pgvector/pgvector:pg16） | 僅容器網路內 | — | profile `postgres` |
| redis（redis:8.4.0） | 僅容器網路內 | — | profile `redis` |

手動安裝（不使用 `install.sh`）：

```bash
cp .env.example .env        # 至少填入 AGENT_MEMORY_API_KEY
docker compose up -d --build
curl -s -X POST http://localhost:8420/v3/internal/meta/user/init-admin \
  -H "Authorization: Bearer $AGENT_MEMORY_API_KEY" \
  -H "x-memory-service-id: default" -H "content-type: application/json" \
  -d '{"username":"admin"}'
```

也可以分別啟動：

```bash
docker build -f docker/core.Dockerfile -t memory-core:local .
docker run --rm -p 8420:8420 -e AGENT_MEMORY_API_KEY=change-me \
  -e AGENT_MEMORY_LLM_API_KEY=sk-... -v memory-core-data:/data memory-core:local

docker build -t memory-panel:local panel
docker run --rm -p 8125:8125 -e CORE_URL=http://host.docker.internal:8420 \
  -e CORE_API_KEY=change-me memory-panel:local
```

## 使用者、團隊與權限

以 admin 的 user key 登入 Panel。在 **Metadata** 中，系統管理員可以建立使用者（每位都會產生一把 user key，只顯示一次）、建立團隊、把使用者指派到團隊並設定角色，以及建立 Agent。其他使用者看到的是唯讀的 Panel。

Client（任何機器上的 agent）只用自己的 **user key** 驗證，不需要 `AGENT_MEMORY_API_KEY`。每次記憶呼叫都要帶上使用者所屬的 `team_id` 與 `agent_id`；成員只能讀寫自己的對話與事實；agent 的場景與 persona 則由使用它的人共用。[docs/MULTI-TENANCY.zh-tw.md](docs/MULTI-TENANCY.zh-tw.md) 從配置方式到隔離機制，完整說明團隊、使用者與 agent。完整規則見 [docs/API.zh-tw.md](docs/API.zh-tw.md)，架構說明見 [docs/ARCHITECTURE.zh-tw.md](docs/ARCHITECTURE.zh-tw.md)。

**伺服器與 agent 在不同機器上？** 請參考 [docs/REMOTE-CLIENTS.zh-tw.md](docs/REMOTE-CLIENTS.zh-tw.md)，內容包含 TLS、發放 key、安裝 `amem` client、Claude Code／Codex／Antigravity／Hermes 整合，以及升級時 client 端要做的事。

**從舊版升級？** 更新程式碼後，在安裝目錄執行 `./upgrade.sh`。全手動的步驟與還原方式請見 [docs/UPGRADE.zh-tw.md](docs/UPGRADE.zh-tw.md)。

**移除：** 在跑 agent 的機器上執行 `./uninstall.sh client`，會移除 Claude Code、Codex、Antigravity、Hermes 的整合和 `amem`。在伺服器上執行 `./uninstall.sh server`，會移除容器和映像，但保留資料；加上 `--purge-data` 才會連資料一起刪除。兩者都可以先加 `--dry-run`，看看會移除哪些東西。詳細說明與手動步驟請見 [docs/UNINSTALL.zh-tw.md](docs/UNINSTALL.zh-tw.md)。

## 本機開發

```bash
cargo test --workspace                 # 單元 + 整合 + 端到端測試（Postgres/Redis 測試預設略過）
# 對真實的 Postgres 與 Redis 執行整合測試：
docker run -d --rm --name amem-pg -e POSTGRES_PASSWORD=test -p 25432:5432 pgvector/pgvector:pg16
docker run -d --rm --name amem-redis -p 26379:6379 redis:8.4.0
AGENT_MEMORY_TEST_POSTGRES_URL=postgres://postgres:test@127.0.0.1:25432/postgres \
AGENT_MEMORY_TEST_REDIS_URL=redis://127.0.0.1:26379 cargo test --workspace
AGENT_MEMORY_LLM_API_KEY=sk-... cargo run -p memory-server
cd panel && npm install && npm run dev # Panel（Vite + Hono）
```

## 記憶如何運作

```text
/v3/conversation/add ──► L0 對話（SQLite + FTS5）
        │  warm-up 1→2→4→每 N 輪，或 idle 600s
        ▼
   L1 抽取（LLM：情境切分 + persona/episodic/instruction，priority 門檻）
   └─ 去重：混合召回候選 → LLM 判定 store / update / merge / skip
        │  delay 90s，且距上次 L2 ≥ 900s；之後每 3600s 掃一次
        ▼
   L2 場景（team+agent 共享；UPDATE > MERGE > CREATE、heat、上限 15 個、META 檔頭）
        │  冷啟動 / 每 50 條新記憶 / 場景要求更新
        ▼
   L3 Persona（persona.md + 自動附加 Scene Navigation）
```

召回（`/v3/recall`）：清理先前注入的區塊 → L1 混合檢索（BM25 + 向量 + RRF k=60，無 embedding 時降級 BM25）→ 門檻、筆數與字元預算 → 輸出 `<relevant-memories>` 與 `<user-persona>`、`<scene-navigation>`。

中文全文檢索：以「CJK 重疊雙字詞 + 拉丁字詞」切詞後交給 FTS5，免字典且結果可預測。

## 專案結構

```text
crates/
├── memory-domain/          實體、值物件、純政策（隔離、RRF、ACL、Skill、場景格式、觸發規則）
├── memory-application/     Use cases 與 ports（記憶、召回、管線、元資料、Skill）
├── memory-infrastructure/  SQLite / 檔案系統 / OpenAI 客戶端 / 背景 worker
├── memory-gateway/         axum v3 HTTP API、信封、驗證、CORS
├── memory-server/          設定（相容 memory-core.yaml）與組裝（composition root）
└── memory-client/          `amem` client：recall／capture hook、secret 過濾、離線佇列、MCP server
clients/                    Claude Code、Codex、Antigravity 的 plugin，以及 Hermes memory provider
panel/                      TypeScript Monitor Panel（Hono BFF + React）
docker/core.Dockerfile      Core 映像（內建 claude 與 agy CLI）
install.sh                  互動式安裝程式
upgrade.sh                  將既有安裝升級到 0.1.1
uninstall.sh                移除 client 或伺服器
docs/ARCHITECTURE.zh-tw.md  架構說明（含 mermaid 架構圖）
docs/API.zh-tw.md           API 一覽與設計取捨
docs/REMOTE-CLIENTS.zh-tw.md  伺服器與 client 分離部署指引
docs/UPGRADE.zh-tw.md        舊版升級指引（upgrade.sh、手動步驟、還原）
docs/UNINSTALL.zh-tw.md      移除 client 或伺服器（uninstall.sh、手動步驟）
docs/MULTI-TENANCY.zh-tw.md  團隊、使用者與 agent：配置方式、設定、隔離機制
```

## 設定

設定檔鍵（`server`、`data`、`llm`、`memory.pipeline`、`memory.recall`、`memory.embedding`、`skill`…），範例見 `config/memory-core.example.yaml`。常用環境變數：

| 變數 | 預設 | 說明 |
| --- | --- | --- |
| `AGENT_MEMORY_CONFIG` | 自動尋找 `memory-core.yaml` | 設定檔路徑 |
| `AGENT_MEMORY_HOST` / `AGENT_MEMORY_PORT` | `127.0.0.1` / `8420` | 綁定位址；非 loopback 必須設定 API key |
| `AGENT_MEMORY_API_KEY` | 未設定 | Bearer token |
| `AGENT_MEMORY_STORAGE_BACKEND` / `AGENT_MEMORY_POSTGRES_URL` | `sqlite` / 未設定 | 儲存後端（見「儲存與協調後端」） |
| `AGENT_MEMORY_COORDINATION_BACKEND` / `AGENT_MEMORY_REDIS_URL` | `local` / 未設定 | 協調後端 |
| `AGENT_MEMORY_DATA_DIR` | `~/.agent-memory` | 資料目錄（每個 instance 一個 SQLite） |
| `AGENT_MEMORY_LLM_PROVIDER` | `openai` | `openai`（HTTP API）或 `cli`（本機工具） |
| `AGENT_MEMORY_LLM_BASE_URL` / `AGENT_MEMORY_LLM_API_KEY` / `AGENT_MEMORY_LLM_MODEL` | OpenAI | provider=openai 時使用 |
| `AGENT_MEMORY_LLM_CLI_*` | 未設定 | provider=cli 時使用，見上節 |
| `AGENT_MEMORY_EMBEDDING_PROVIDER` 等 | `none` | 啟用向量檢索 |
| `AGENT_MEMORY_CORS_ORIGINS` | 空 | 允許的來源（逗號分隔） |

## 儲存與協調後端

| 設定 | 選項 | 說明 |
| --- | --- | --- |
| `storage.backend`（`AGENT_MEMORY_STORAGE_BACKEND`） | `sqlite`（預設） | 每個 instance 一個 SQLite 檔；全文檢索用 FTS5，向量用 **sqlite-vec**（`vec0` KNN，隔離條件在索引內套用；擴充無法載入時退回逐筆比對） |
| | `postgres` | 每個 instance 一個 schema（`am_<hash>`）；全文檢索用 tsvector + GIN，向量用 **pgvector HNSW**（維度 > 2000 時為精確搜尋） |
| `coordination.backend`（`AGENT_MEMORY_COORDINATION_BACKEND`） | `local`（預設） | 管線狀態在 `system.db`，佇列與鎖在行程內；單一節點 |
| | `redis` | 管線狀態、工作佇列、分散式鎖都在 Redis；可同時執行多個 memory-core 副本 |

### PostgreSQL

- 連線：`AGENT_MEMORY_POSTGRES_URL=postgres://user:pass@host:5432/db`。
- 權限：需要資料庫的 CREATE 權限（建立 `agent_memory` 與各 `am_*` schema）。啟用 embedding 時需要建立 `vector` 擴充：以 superuser 執行，或由 DBA 預先 `CREATE EXTENSION vector`。
- 與 SQLite 的差異：向量檢索為近似（HNSW）；變更 embedding 維度會清除既有向量；文字不可含 NUL 字元。

### Redis

- 連線：`AGENT_MEMORY_REDIS_URL=redis://[:password@]host:6379/0`；所有 key 以 `coordination.redis.keyPrefix`（預設 `agent_memory`）為前綴。
- 佇列為 **at-least-once**：取出的 job 帶有租約（`coordination.redis.queueLeaseMs`，預設 15 分鐘），執行中會自動延長；節點當機時 job 在租約到期後重新派送。極端情況下同一 job 可能執行兩次，管線設計上可以容忍。
- 鎖以 `SET NX PX` 取得並在執行期間續約（`lockTtlMs`，預設 10 分鐘）。
- 不支援 Redis Cluster（腳本會動態組 key）；高可用請用單一 primary 搭配 replica／Sentinel。
- 失敗 job 的 dead-letter 清單保留最新 10,000 筆。

### sqlite-vec

- 只有設定 embedding（`memory.embedding.provider` 非 `none`）時才會建立索引；`/health` 的 `stores.vectorIndex` 顯示 `sqlite-vec`、`brute-force`、`pgvector-hnsw`、`pgvector-exact` 或 `none`。
- 單次 KNN 上限 4096 筆；每個 team 分區約占用 1024 個向量的區塊空間（1536 維約 6 MB）。

## 使用本機 CLI 作為 LLM

除了 OpenAI 相容的 API（API key），也可以讓 memory-core 呼叫本機的命令列工具來產生記憶，並指定工具執行時所在的專案目錄（讓工具讀得到該專案的設定與脈絡）：

```yaml
llm:
  provider: cli
  timeoutMs: 180000
  cli:
    command: claude              # 或 ollama 等可讀 stdin、輸出文字答案的工具
    args: ["-p"]                 # ollama 範例：["run", "llama3.1"]
    promptMode: stdin            # 工具只吃參數時用 arg，並在 args 中放 {prompt}
    workingDir: ~/projects/my-app
    maxConcurrency: 2
```

等價的環境變數：`AGENT_MEMORY_LLM_PROVIDER=cli`、`AGENT_MEMORY_LLM_CLI_COMMAND`、`AGENT_MEMORY_LLM_CLI_ARGS`（JSON 陣列或以空白分隔）、`AGENT_MEMORY_LLM_CLI_PROMPT_MODE`、`AGENT_MEMORY_LLM_CLI_WORKDIR`。

- 不經過 shell，直接以參數陣列啟動子行程；prompt 預設經 stdin 傳入，不會出現在行程列表。
- 每次呼叫受 `timeoutMs` 限制，逾時即終止子行程；非 0 結束碼或空輸出都視為 LLM 不可用，交由管線重試。
- 各工具的旗標請以該工具文件為準；工具需能把完整答案印到 stdout。

### Docker 映像內建的 CLI

`docker/core.Dockerfile` 預設以官方原生安裝腳本（不使用 npm，皆會驗證校驗碼）安裝三個 CLI，並以非 root 的服務使用者執行：

| CLI | 指令 | 建議設定 | 登入方式 |
| --- | --- | --- | --- |
| Claude Code | `claude` | `ARGS=-p`、`PROMPT_MODE=stdin` | 設定 `ANTHROPIC_API_KEY`，或執行 `docker compose exec -it memory-core claude` 後輸入 `/login` |
| Antigravity CLI | `agy` | `ARGS='["-p","{prompt}"]'`、`PROMPT_MODE=arg`（`agy -p` 不讀 stdin） | 執行 `docker compose exec -it memory-core agy`，在瀏覽器開啟顯示的 Google 登入網址 |
| Codex CLI | `codex` | `ARGS='["exec","--skip-git-repo-check","--ephemeral","-s","read-only","-c","features.hooks=false","-"]'`、`PROMPT_MODE=stdin` | 執行 `docker compose exec -it memory-core codex login --device-auth` 並依畫面指示完成，或把 API key 以 stdin 傳給 `codex login --with-api-key` |

- 登入狀態存放在 named volume（`claude-config` → `~/.claude`、`antigravity-config` → `~/.gemini`、`codex-config` → `~/.codex`），重建容器不會遺失。
- 專案目錄：以 `PROJECT_DIR` 指定主機路徑，掛載到容器的 `/workspace` 並作為 CLI 的工作目錄。
- 未登入時，錯誤會出現在 Panel 的 worker 指標（例如 `Not logged in · Please run /login`），管線會重試。
- `agy` 以參數傳遞 prompt，受 Linux 單一參數 128 KiB 上限限制；超過時會直接回報錯誤。
- `agy` 需要 CPU 支援 AES-NI（虛擬機請將 CPU 型別設為 host）。
- Codex 的參數讓它保持唯讀、不留下 session 檔，並關閉 hooks，避免為 Codex 安裝的記憶 client 把蒸餾用的 prompt 本身寫進記憶。
- 不需要的 CLI 可在建置時略過：`--build-arg INSTALL_CLAUDE=false`、`--build-arg INSTALL_ANTIGRAVITY=false`、`--build-arg INSTALL_CODEX=false`；Claude Code 版本可用 `--build-arg CLAUDE_VERSION=latest` 或指定版本號。
- 映像因此約 940 MB；若只用 HTTP API，建議以上述參數略過這些 CLI。

## 設計取捨

詳見 [docs/API.zh-tw.md](docs/API.zh-tw.md)。重點：

- L2/L3 與 Skill 抽取使用結構化 JSON 操作（而非帶工具的 agentic 迴圈），輸出契約固定、易於測試。
- 召回與搜尋一律套用租戶隔離；管線游標使用資料庫序號，時間戳相同或補登的訊息不會被跳過。
- 未提供：分析報表、外部任務匯入、上游模型群組路由、`/v3/skill/export`。

## 授權

MIT

## 致謝

分層記憶與 Skill 的設計思路參考自開源專案 TencentDB Agent Memory（MIT）；本專案為獨立實作，不含其程式碼，也不連線至任何騰訊服務。
