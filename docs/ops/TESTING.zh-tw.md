# 測試

[English](TESTING.md)

依你修改的範圍跑對應的測試；發版前全部都要跑。CI 會在每個 pull request 跑第 1 到 4 節（[RELEASE.zh-tw.md §5](RELEASE.zh-tw.md#5-自動化)）；第 5 節和截圖比對仍然要手動做。

## 目錄

- [1. Rust：格式、lint、測試](#1-rust格式lint測試)
- [2. Postgres 與 Redis 整合測試](#2-postgres-與-redis-整合測試)
- [3. Panel](#3-panel)
- [4. Shell 腳本](#4-shell-腳本)
- [5. 整套 stack 煙霧測試](#5-整套-stack-煙霧測試)
- [6. 測試程式碼的規則](#6-測試程式碼的規則)

## 1. Rust：格式、lint、測試

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

- 格式採用 rustfmt 預設值，沒有 `rustfmt.toml`。
- Clippy 在 `-D warnings` 下必須沒有任何警告。新版工具鏈可能會加入新的 lint，所以請用目前的 stable 版本跑。
- workspace 宣告 `rust-version = "1.88"`，每個 crate 都繼承這個設定。較新的本機工具鏈會編譯 1.88 不接受的程式碼；用到較新的語言或函式庫功能時，請用 `cargo +1.88 check --workspace --locked` 確認。`.cargo/config.toml` 讓 `cargo update` 只挑仍支援 1.88 的依賴版本；其他問題由 CI 的 msrv job 抓出來。
- `cargo test` 包含單元測試，以及 `crates/memory-server/tests/e2e.rs` 的端到端測試（以 SQLite 和 local 協調後端啟動真正的伺服器）。同一次執行裡的 Postgres 和 Redis 測試，在沒有設定的情況下會印出 `skipping: …` 並視為通過（見下一節）。

## 2. Postgres 與 Redis 整合測試

這些測試只有在設定了 URL 時才會執行，所以單純跑 `cargo test` **不會**測到 PostgreSQL 和 Redis。只要修改了 `crates/memory-infrastructure/src/postgres/`、`src/redis/`、`src/sqlite_to_postgres.rs` 或任何 SQL，都需要跑這一組：

```bash
docker run -d --rm --name amem-pg -e POSTGRES_PASSWORD=test -p 25432:5432 pgvector/pgvector:pg17
docker run -d --rm --name amem-redis -p 26379:6379 redis:8.4.0
until docker exec amem-pg pg_isready -U postgres -q; do sleep 1; done

AGENT_MEMORY_TEST_POSTGRES_URL=postgres://postgres:test@127.0.0.1:25432/postgres \
AGENT_MEMORY_TEST_REDIS_URL=redis://127.0.0.1:26379 \
  cargo test --workspace --locked

docker stop amem-pg amem-redis
```

| 測試檔 | 需要 | 涵蓋範圍 |
| --- | --- | --- |
| `memory-infrastructure/tests/postgres_stores.rs` | Postgres | 每個 Postgres adapter，以和 SQLite 相同的期望值檢查（排序、中日韓文搜尋、隔離、key） |
| `memory-infrastructure/tests/sqlite_to_postgres.rs` | Postgres | 透過 SQLite adapter 寫入的資料，在 `migrate` 之後透過 Postgres adapter 讀回：搜尋、向量、版本、key、序號、重跑 |
| `memory-server/tests/e2e.rs`（`the_pipeline_runs_on_the_postgres_backend`、`legacy_default_data_migrates_on_postgres`、`redis_coordination_runs_the_pipeline`） | Postgres／Redis | 整個伺服器跑在這些後端上，包含舊版範圍的資料搬移 |

每個測試都使用隨機命名的部門，結束後會刪除，所以同一個資料庫可以重複使用。失敗的測試可能會留下它的 schema（`am_<hash>`）；把容器刪掉重來即可。

## 3. Panel

```bash
cd panel
npm ci
npm run typecheck
npm test
npm run build
```

需要 Node 22 以上（`panel/package.json` 的 `engines`）。`npm ci` 也會拒絕和 `package.json` 不同步的 `package-lock.json`。

### 樣式相關的修改：比對截圖

型別檢查和單元測試看不到 CSS。只要修改可能改變 Panel 的外觀（Tailwind、PostCSS、Vite、共用元件），就要比對新舊兩個 build：

1. build 基準分支，把 `panel/web/dist` 另存一份；再 build 修改後的版本。
2. 啟動一個 memory-core 並寫入資料（API 呼叫見 [§5](#5-整套-stack-煙霧測試)），再對它啟動兩個 Panel server，各服務一個 build：`PORT=… CORE_URL=… UI_DIST_DIR=<dist 的絕對路徑> node server/dist/index.js`。`UI_DIST_DIR` 必須是絕對路徑，因為 `npm start -w server` 會從 `panel/server` 解析它。
3. 用 Playwright 在兩邊登入，對每個路由（`#/dashboard`、`#/memory`、`#/logs`、`#/skills`、`#/meta`，以及登入頁）在亮色和暗色模式（`colorScheme`）下截圖，再用 `pixelmatch` 逐對比對。
4. 即時數值（dashboard 的時間、uptime）一定會不同。其他差異請先比對兩個 build 上該元素的 `getComputedStyle`，再判斷是否真的有變：樣式完全相同時，文字點陣化也可能讓像素不同。
5. 也要檢查截圖看不到的：按鈕的 `cursor`、`::placeholder` 顏色、focus 外框。

如果 Playwright 無法為這台主機下載瀏覽器（它會拒絕不認得的發行版），可以用 `executablePath` 指定現有的 Chromium 或 headless shell。

## 4. Shell 腳本

```bash
docker run --rm -v "$PWD:/mnt" -w /mnt koalaman/shellcheck:v0.10.0 -x \
  install.sh upgrade.sh uninstall.sh migrate-to-postgres.sh
```

`upgrade.sh` 和 `migrate-to-postgres.sh` 會 `source ./install.sh` 取用共用函式，所以要加 `-x`。CI 會跑同一個指令（另外加上 `.github/scripts/*.sh`），只要有任何警告就失敗，所以每支腳本都要保持沒有警告。

腳本的實際行為只能靠實際執行來測（見下一節）。

## 5. 整套 stack 煙霧測試

`install.sh`、`upgrade.sh` 和 `migrate-to-postgres.sh` 會操作 Docker Compose、build 映像並修改 `.env`，單元測試涵蓋不到，所以要在一份可丟棄的 stack 上測。以下三點可以避免影響同一台機器上已經在跑的安裝：

- **另開目錄。** Compose 會用目錄名稱為容器和 volume 命名（`<目錄>_memory-core-data`……）。
- **用不同的 port**：設定 `CORE_PORT`／`PANEL_PORT`。
- **用不同的映像 tag**：設定 `MEMORY_IMAGE_TAG`。不設的話，build 會覆蓋 `memory-core:local`，而執行中的安裝可能正在使用它。

```bash
# 1. 複製工作目錄（只含 git 追蹤的檔案）
S=$(mktemp -d)/stack && mkdir -p "$S"
git ls-files -z | xargs -0 -I{} cp --parents {} "$S/"
cd "$S"

# 2. 以非互動模式安裝 SQLite 版本
CORE_PORT=28520 PANEL_PORT=28525 MEMORY_IMAGE_TAG=smoke \
INSTALL_STORAGE=sqlite INSTALL_COORDINATION=local INSTALL_LLM=openai INSTALL_ADMIN_NAME=admin \
  ./install.sh --non-interactive          # 會印出管理員的 user key

# 3. 透過 API 寫入資料（team、agent、幾筆對話），例如
API=$(grep '^AGENT_MEMORY_API_KEY=' .env | cut -d= -f2)
H=(-H "Authorization: Bearer $API" -H "x-memory-department-id: default"
   -H "x-memory-user-key: <管理員 key>" -H "content-type: application/json")
curl -s -X POST localhost:28520/v3/internal/meta/user/list-by-department "${H[@]}" -d '{}'   # 取得管理員的 user_id
curl -s -X POST localhost:28520/v3/meta/team/create  "${H[@]}" -d '{"name":"platform","owner_user_id":"usr-…"}'
curl -s -X POST localhost:28520/v3/meta/agent/create "${H[@]}" -d '{"team_id":"team-…","owner_user_id":"usr-…","name":"codex"}'
curl -s -X POST localhost:28520/v3/conversation/add  "${H[@]}" \
  -d '{"team_id":"team-…","agent_id":"agt-…","session_id":"s1","messages":[{"role":"user","content":"我們下週一發布第二版"}]}'

# 4. 執行要測的腳本，再透過 API 檢查結果
MIGRATE_TARGET=postgres-docker ./migrate-to-postgres.sh --non-interactive
curl -s localhost:28520/health                      # "backend":"postgres"
curl -s -X POST localhost:28520/v3/conversation/query "${H[@]}" -d '{"team_id":"team-…","agent_id":"agt-…"}'

# 5. 也試試重跑和文件裡的還原方式
./migrate-to-postgres.sh --non-interactive          # "already stores its data in PostgreSQL"
cp backups/pre-postgres-*/env .env && docker compose up -d

# 6. 清掉這份 stack 建立的所有東西
docker compose --profile postgres --profile redis down -v --remove-orphans
docker image rm memory-core:smoke memory-panel:smoke
cd / && rm -rf "$S"
```

第一次 build 要好幾分鐘，因為 core 映像會安裝內建的 LLM CLI。如果只想測 `memory-server` 本身，直接執行 `target/debug` 裡的執行檔，並設定 `AGENT_MEMORY_DATA_DIR`、`AGENT_MEMORY_PORT` 和儲存相關的環境變數即可，幾秒鐘就能完成。

## 6. 測試程式碼的規則

- **不要寫死長得像金鑰的字串。** GitGuardian、GitHub push protection 這類掃描器會標記任何長得像真 token 的字串，測試裡也一樣。請在執行時用前綴加低熵的填充字元組出來，像 `crates/memory-client/redact/src/lib.rs` 的 `fake("sk-mem-", 'x', 32)`。絕對不要把測試伺服器印出來的 key 貼進測試。
- **需要外部服務的測試，在沒設定時要跳過，不能失敗**，並用 `eprintln!("skipping: …")` 說明。要提供哪些服務由 CI 決定。
- **從另一端讀回來驗證。** 儲存相關的程式碼，用一個 adapter 寫入、用另一個 adapter（或透過 HTTP API）檢查，而不是只數筆數。
