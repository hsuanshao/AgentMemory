# 升級

[English](UPGRADE.md)

## 目錄

- [0.4.0 之後（未發布）](#040-之後未發布)
- [升級到 0.4.0](#升級到-040)
- [升級到 0.3.0](#升級到-030)
- [0.1.1 到 0.2.x 之間升級](#011-到-02x-之間升級)
- [從 0.1.1 之前的版本升級](#從-011-之前的版本升級)
  - [有哪些改變](#有哪些改變)
  - [開始之前](#開始之前)
  - [A. 自動升級：`upgrade.sh`](#a-自動升級upgradesh)
  - [B. 手動：逐步執行相同步驟](#b-手動逐步執行相同步驟)
  - [C. 更新 client](#c-更新-client)
  - [D. 確認結果](#d-確認結果)
  - [E. 還原](#e-還原)
  - [F. 疑難排解](#f-疑難排解)

## 0.4.0 之後（未發布）

```bash
./upgrade.sh
```

**場景和 persona 改存到新的資料表。** memory-core 啟動時會執行 schema 遷移 3，把 `profile_files` 裡的每個版本複製到 `scenes`、`scene_versions`、`persona_versions`，之後這些表只會追加。`profile_files` 會保留，但不再有任何寫入。client 看到的場景和 persona 不變，API 路徑也不變。

> [!WARNING]
> **退回 0.4.0 必須還原備份**（[E. 還原](#e-還原)）。0.4.0 讀的仍是 `profile_files`，在升級過的資料上，它會顯示升級前的場景和 persona；它寫入的內容，之後再升級時也不會被帶過來，因為遷移已經記錄為完成。還原備份也會把升級後的其他所有變更一起撤銷，這點和任何退版一樣。

**從這一版起，memory-core 會拒絕由更新版本寫入的資料。** 它會檢查 SQLite 的 `user_version`、Postgres 的 `schema_migrations` 和 Redis 的 `schema_version`；資料比它新時，會停止並顯示錯誤，提示改跑新版本或還原備份。0.4.0 和更早的版本沒有這項檢查，退回這些版本時要特別注意。

## 升級到 0.4.0

```bash
git fetch --tags && git checkout v0.4.0
./upgrade.sh
```

**授權：** 從 0.4.0 開始，server 端改用 Business Source License 1.1；`amem` client 和各工具的 plugin 維持 MIT。請確認你的使用方式是否在 [COMMERCIAL.zh-tw.md](../COMMERCIAL.zh-tw.md) 的免費範圍內。這一版不需要遷移資料。

> [!WARNING]
> **只要 `AGENT_MEMORY_LLM_CLI_ARGS`（`llm.cli.args`）讓蒸餾用的 CLI 拿到比預設更多的能力，memory-core 就不會啟動。** 升級前請先檢查你的 `.env`。

蒸餾讀的是不可信的對話內容，所以蒸餾模型永遠不會拿到工具（#37）。以下參數現在會被拒絕，錯誤訊息會指出找到的是哪一個：

- `--dangerously-skip-permissions`、`--allow-dangerously-skip-permissions`、`--dangerously-bypass-approvals-and-sandbox`，以及其他所有 `--dangerously-…`；
- `--yolo`、`--full-auto`、`--allowedTools` / `--allowed-tools`、`--mcp-config`、`--permission-prompt-tool`；
- `default`、`plan` 以外的 `--permission-mode`；
- claude：帶工具清單的 `--tools`；
- codex：沒有 `exec` 的參數；`--enable …`、`--search`、`-p` / `--profile`、`read-only` 以外的 `-s` / `--sandbox`，以及這些 `-c` 設定：`mcp_servers.…`、`features.…=true`、`disabled` 以外的 `web_search`、`read-only` 以外的 `sandbox_mode`、`sandbox_permissions`、`approval_policy`、`profile`。值直接接在短參數後面的寫法（`-cKEY=VALUE`、`-sMODE`、`-pNAME`）也一樣會被檢查。`-c model_reasoning_effort=high`、`-c service_tier=fast` 這類設定沒有問題。

請把它們拿掉，蒸餾不需要其中任何一個。以前因為工具被拒絕而一直失敗的 CLI，現在會直接 drop 那批輸入（0.3.2）。

**`upgrade.sh` 會幫你檢查。** 備份完成後，它會先建置新版本，用你的設定執行 `memory-server check-config`，通過後才啟動新版本。如果設定被拒絕，或是在新版本通過 `/health` 之前任何一步失敗（例如建置失敗，或新版本啟動不了），它會印出錯誤、把 `.env` 還原成升級前的樣子、用原本的 image 重新執行舊版本，然後停止。如果新版本在失敗前已經遷移了儲存的資料，請再從備份還原資料（[E. 還原](#e-還原)）。要手動檢查的話，在 `git checkout` 新版本之後執行：

```bash
docker compose build memory-core
docker compose run --rm --no-deps -T memory-core check-config
```

**另外：** 使用 `claude` 或 `codex` 時，memory-core 現在會在蒸餾前先關掉 CLI 自帶的工具（`AGENT_MEMORY_LLM_CLI_HARDENING=auto`，預設值）。**蒸餾用的 codex 不再讀 `config.toml`：** 如果有需要的設定放在那裡，請改放到 `AGENT_MEMORY_LLM_CLI_ARGS`（`-c key=value`）。見 [DISTILLATION 第 6 節](ops/DISTILLATION.zh-tw.md#6-工具與蒸餾)。

## 升級到 0.3.0

0.3.0 把 **instance** 改名為**部門（department）**：一台伺服器容納多個部門，部門裡有團隊，團隊裡有成員和 agent。新名稱同時用在 API、設定和儲存的資料上：

| | 0.3.0 之前 | 0.3.0 |
| --- | --- | --- |
| 請求 header | `x-memory-service-id` | `x-memory-department-id` |
| `amem` | `service_id`、`AMEM_SERVICE_ID`、`--service-id` | `department_id`、`AMEM_DEPARTMENT_ID`、`--department-id`。`client.json` 第一次使用時會自動改寫 |
| Claude Code plugin 選項 | Service ID | Department ID |
| `.env` | `CORE_INSTANCE_ID` | `CORE_DEPARTMENT_ID` |
| `memory-core.yaml` | `maxOpenInstances`、`maxUsersPerInstance`、`maxTeamsPerInstance` | `maxOpenDepartments`、`maxUsersPerDepartment`、`maxTeamsPerDepartment`。設定檔還用舊名稱時，memory-core 會拒絕啟動 |
| API | `/v3/instance/destroy` `{instance_id}`、`/v3/meta/instance-quota/get`、`/v3/internal/meta/user/list-by-instance` | `/v3/department/destroy` `{department_id}`、`/v3/meta/department-quota/get`、`/v3/internal/meta/user/list-by-department` |
| `memory-server` 指令 | `--instance` | `--department` |
| Team 角色 | `admin`、`member`、`reviewer` | `admin`、`leader`、`member`。原本的 reviewer 改為 member；leader 可以看到整個 team 的管線狀態、生成紀錄和 dead letter（[COLLABORATION 第 12 節](COLLABORATION.zh-tw.md#12-依角色檢視運作狀態)） |
| 帶 user key 查詢運作狀態 | 整個部門 | 依使用者角色允許的範圍。重跑 dead letter 需要系統管理者 |
| 儲存的資料 | `<data>/instances`、Postgres `agent_memory.instances`、Redis `…:pipe:inst:…` | memory-core 啟動時自動遷移（[改了哪些](ops/SCHEMA-MIGRATIONS.zh-tw.md#3-目前的遷移)） |

**伺服器和所有 client 要一起更新。** 0.3.0 之前的 client 會收到 `401`，訊息是「x-memory-service-id was renamed in 0.3.0; update the client」。

1. 停掉會寫入伺服器的 agent。如果跑了多個 memory-core replica，要全部停掉：舊版 replica 讀不懂遷移後的資料。
2. 更新程式碼，在安裝目錄執行升級腳本：

   ```bash
   cd /path/to/agent-memory
   git fetch --tags && git checkout v0.4.0
   ./upgrade.sh
   ```

   `upgrade.sh` 會備份資料、把 `.env` 的 `CORE_INSTANCE_ID` 改名，然後重建並啟動服務。memory-core 啟動時會遷移儲存的資料。

   也可以手動執行：
   1. 依 [B1](#b1-備份) 備份。
   2. 把 `.env` 的 `CORE_INSTANCE_ID` 改成 `CORE_DEPARTMENT_ID`；如果 `memory-core.yaml` 有設定上表那三個項目，也一起改名。
   3. 執行：

      ```bash
      docker compose build
      docker compose run --rm -T memory-core migrate   # 選用：啟動前先遷移並顯示結果
      docker compose up -d
      ```
3. 更新每台 client 機器上的 `amem`（[REMOTE-CLIENTS 第 4 節](REMOTE-CLIENTS.zh-tw.md#4-在每台-client-機器安裝-amem)）。在 Claude Code 裡，如果之前設定過 **Service ID**，請把 **Department ID** 設成同樣的值。
4. 確認：
   - `curl -s http://127.0.0.1:8420/health` 顯示新的 `version`；
   - `docker compose logs memory-core | grep -i migrat` 列出執行過的步驟；
   - 在 client 上，`amem status` 顯示預期的 `department_id`，`amem recall test` 能正常回應、沒有錯誤。

遷移只能往前。要回到 0.2.x，請依 [E](#e-還原) 還原備份。

## 0.1.1 到 0.2.x 之間升級

0.1.1 到 0.2.x 的版本沒有改變資料格式，也沒有改變 client 的驗證方式。在安裝目錄（放 `.env` 的那個目錄）執行：

```bash
cd /path/to/agent-memory
git fetch --tags && git checkout v0.2.3   # 或：git pull
docker compose up -d --build
```

`docker compose` 會從 `.env` 讀取 `COMPOSE_PROFILES` 和其他設定，所以會啟動同樣的服務。用 `curl -s http://127.0.0.1:8420/health` 確認：`version` 會顯示新版本。client 不受影響，方便時再更新每台機器上的 `amem`（[REMOTE-CLIENTS §4](REMOTE-CLIENTS.zh-tw.md#4-在每台-client-機器安裝-amem)）。

要回到舊版時，checkout 前一個 tag，再執行同樣的 `docker compose up -d --build`。

## 從 0.1.1 之前的版本升級

這部分寫給還在跑 0.1.1 之前版本的人。直接升到 0.3.0 時也適用：`upgrade.sh` 會一次完成兩個升級。每個步驟都是你自己執行的指令，全程不需要透過 AI 或 agent。你可以交給 `./upgrade.sh` 一次完成（[A](#a-自動升級upgradesh)），也可以照著手動做一樣的步驟（[B](#b-手動逐步執行相同步驟)）。兩種做法最後都要更新 client（[C](#c-更新-client)）。

## 有哪些改變

| | 0.1.1 之前 | 0.1.1 |
| --- | --- | --- |
| Client 驗證 | 伺服器 API key（`Authorization: Bearer …`） | 每位使用者各自的 key（`x-memory-user-key: sk-mem-…`）。伺服器 API key 只用於維運與管理 |
| 寫入時的 team / agent | 可省略，省略時資料寫進共用的 `default` 桶 | 必填。使用者必須是該 team 的成員，agent 也必須屬於該 team |
| 舊資料 | — | 不再提供 `default` 桶的資料，升級時要搬移一次 |
| Client | 自行撰寫的 script 或 plugin | `amem` client，以及 Claude Code、Codex、Antigravity、Hermes 的 plugin |

**這是不相容的變更。** 升級後只送 Bearer key 的 client 會收到 `401`，要依 C 節改設定後才能繼續使用。

## 開始之前

- 你需要能在伺服器上開 shell，並進入當初的安裝目錄（有 `.env` 的那個目錄）。那台機器上要有 Docker Compose v2 與 `curl`。
- 伺服器會停機幾分鐘：備份時會先停止，之後再重建。
- 請先停止或關閉所有會寫入記憶伺服器的 agent（Hermes、Claude Code、Antigravity、script）。升級期間寫入的資料會是舊格式。
- 更新程式碼：

  ```bash
  cd /path/to/agent-memory        # 有 .env 的目錄
  git fetch --tags && git checkout v0.4.0  # 或：git pull
  ```

## A. 自動升級：`upgrade.sh`

```bash
./upgrade.sh
```

腳本會依序執行下列步驟，每一步都可以安全地重複執行。

1. **備份**到 `backups/<時間>/`。內容包括 `.env`、checkout 之前的 commit（`previous-commit`），以及 memory-core 資料 volume 的壓縮檔。用 Postgres 時另外做 `pg_dump`：使用自己的伺服器時，會改用和伺服器主版本相同的 `pg_dump`（`postgres:<主版本>-alpine`），因為舊版的 `pg_dump` 不接受較新的伺服器；啟用 Redis profile 時，也會把 Redis 的資料打包。目前執行中的映像會另外標成 `memory-core:pre-upgrade-<時間>` 和 `memory-panel:pre-upgrade-<時間>`。備份前會先停止 memory-core，確保資料一致。
2. **設定**：把 `.env` 中改過名稱的設定改成新名稱（0.3.0：`CORE_INSTANCE_ID` → `CORE_DEPARTMENT_ID`），並補上新增的設定（`MEMORY_IMAGE_TAG=local`）。其他設定都不會改動。
3. **重建並啟動**：執行 `docker compose up -d --build`，等到 `/health` 回應正常。
4. **搬移舊記憶**。腳本會先列出找到的資料，再問你兩件事：
   - *Team*：預設是 `default-team`，也就是第一次安裝時建立的 team。
   - *Agent*：預設是 `default-agent-<admin>`。建議填你最常用的 client 名稱，例如 `hermes`。不存在的 team 或 agent 會自動建立。

   確認之後，`default` 桶裡的所有資料都會搬進這個 team 與 agent，擁有者是系統管理者。範圍包括對話、記憶、L2 場景、L3 persona 和 skills。管線進度也一起搬過去，所以不會重複蒸餾。
5. **Client 設定**：替管理者發一把新的 user key。就算第一次安裝時印出的 key 已經遺失也沒關係。接著印出 `amem login` 指令，並把所有值寫進 `upgrade-client-settings.txt`（權限 600）。

若不想回答問題，可以先設定環境變數，再加上 `--non-interactive`：

```bash
UPGRADE_AGENT_NAME=hermes ./upgrade.sh --non-interactive
```

| 變數 | 預設值 | 說明 |
| --- | --- | --- |
| `UPGRADE_ADMIN_NAME` | 最早建立的系統管理者 | 誰擁有搬過去的資料，以及新 key 發給誰 |
| `UPGRADE_TEAM_NAME` | `default-team` | 接收資料的 team，不存在時自動建立 |
| `UPGRADE_AGENT_NAME` | `default-agent-<admin>` | 接收資料的 agent，不存在時自動建立 |
| `UPGRADE_SKIP_BACKUP` | `0` | 設為 `1` 會跳過備份。只有在你已經自己備份時才這樣做 |

完成後請接著看 [C](#c-更新-client)。

## B. 手動：逐步執行相同步驟

以下指令都在安裝目錄下執行。先把設定載入目前的 shell：

```bash
source .env   # 不要加 set -a：docker compose 必須自己讀 .env
CORE=http://127.0.0.1:${CORE_PORT:-8420}
H=(-H "Authorization: Bearer $AGENT_MEMORY_API_KEY" -H "x-memory-department-id: ${CORE_DEPARTMENT_ID:-default}" -H "content-type: application/json")
```

### B1. 備份

```bash
B=backups/$(date +%Y%m%d-%H%M%S); mkdir -p -m 700 "$B"
cp .env "$B/env"; git rev-parse 'HEAD@{1}' > "$B/previous-commit"   # checkout 之前的 commit
# 保留目前的映像，還原時就不必重新 build：
docker image tag memory-core:local "memory-core:pre-upgrade-${B##*/}"
docker image tag memory-panel:local "memory-panel:pre-upgrade-${B##*/}"
# Docker 內的 Postgres（COMPOSE_PROFILES 含 postgres）：
docker compose exec -T postgres pg_dump -U "${POSTGRES_USER:-memory}" "${POSTGRES_DB:-memory}" > "$B/postgres.sql"
# 所有後端都要：memory-core 的資料 volume
docker compose stop memory-core
docker compose run --rm --no-deps -T --entrypoint tar memory-core czf - -C /data . > "$B/memory-core-data.tgz"
# Docker 內的 Redis（COMPOSE_PROFILES 含 redis）：
docker compose stop redis
docker compose run --rm --no-deps -T --entrypoint tar redis czf - -C /data . > "$B/redis-data.tgz"
```

若使用自己的 Postgres 伺服器（`AGENT_MEMORY_POSTGRES_URL`），請用你平常的工具備份，例如 `pg_dump "$AGENT_MEMORY_POSTGRES_URL" > "$B/postgres.sql"`。

### B2. 補上新設定

打開 `.env`，把下列清單中缺少的設定補上：

```bash
MEMORY_IMAGE_TAG=local   # docker compose build 出來的映像 tag
```

若同一台機器還跑著另一套部署，請給它不同的 tag，兩邊 build 時才不會互相覆蓋映像。

### B3. 重建並啟動

```bash
docker compose up -d --build
curl -fsS "$CORE/health"   # 重複執行直到有回應
```

### B4. 先看再搬（dry run）

```bash
curl -sS -X POST "$CORE/v3/internal/upgrade" "${H[@]}" \
  -d '{"dry_run": true, "agent_name": "hermes"}'
```

回應會列出管理者（`username`）、`team_name`/`team_id` 和 `agent_name`/`agent_id`。id 是 `null` 表示該 team 或 agent 會在正式執行時建立。`moved` 是舊桶裡還有多少筆資料。這一步不會改動任何東西。

Body 的欄位都可以省略：`admin_username`、`team_name`（預設 `default-team`）、`agent_name`（預設 `default-agent-<admin>`）、`issue_key`、`dry_run`。

> 若回應是 `no active system admin`，表示這套安裝從來沒有建立管理者，請先建立：
> `curl -sS -X POST "$CORE/v3/internal/meta/user/init-admin" "${H[@]}" -d '{"username":"admin"}'`

### B5. 搬移資料並取得 key

```bash
curl -sS -X POST "$CORE/v3/internal/upgrade" "${H[@]}" \
  -d '{"agent_name": "hermes", "issue_key": true}'
```

記下回應中的 `team_id`、`agent_id` 和 `user_key`。Key 只會顯示這一次，也可以用來登入 Panel。重複執行這個指令不會有副作用：已經沒有資料可搬，也會沿用既有的 team 與 agent。不過每次帶 `issue_key` 都會多發一把 key，用不到的可以在 Panel 撤銷。

你也可以自己指定確切的目標：先在 Panel 建好 team、使用者和 agent，再呼叫：

```bash
curl -sS -X POST "$CORE/v3/internal/migrate/default-scope" "${H[@]}" \
  -d '{"team_id":"team-…","agent_id":"agt-…","user_id":"usr-…","dry_run":true}'   # 確認後改成 false 再執行
```

## C. 更新 client

每個 client 都需要四個值：伺服器 URL、**user key**、**team id**、**agent id**。升級時已經印出來，`upgrade-client-settings.txt` 裡也有。若要讓其他人用自己的 key，請在 Panel 建立使用者並加入 team，見 [REMOTE-CLIENTS.zh-tw.md §3](REMOTE-CLIENTS.zh-tw.md#3-建立使用者團隊與-agent管理者)。

1. 在每台跑 agent 的機器上**安裝 `amem` 並登入**，步驟見 [REMOTE-CLIENTS.zh-tw.md §4](REMOTE-CLIENTS.zh-tw.md#4-在每台-client-機器安裝-amem)：

   ```bash
   cargo install --git <本 repository> memory-client      # 或在 checkout 裡：cargo install --path crates/memory-client
   amem login --server-url http://SERVER:8420 --team-id team-… --agent-id agt-… --user-key -
   # 貼上 sk-mem-… key 後按 Enter
   ```

2. **各 client 要做的事：**

   | Client | 要做的事 |
   | --- | --- |
   | Hermes（舊版 `agentmemory` provider） | 舊版放在 `<hermes 原始碼>/plugins/memory/agentmemory`，優先順序比使用者 plugin 高，要先移除。接著安裝新版 provider，見 [REMOTE-CLIENTS.zh-tw.md §8](REMOTE-CLIENTS.zh-tw.md#8-hermes-agent)。請使用接收舊資料的那個 agent，才能召回舊記憶 |
   | Claude Code | 安裝 plugin，見 [REMOTE-CLIENTS.zh-tw.md §5](REMOTE-CLIENTS.zh-tw.md#5-claude-code) |
   | Antigravity | 安裝 plugin，見 [REMOTE-CLIENTS.zh-tw.md §6](REMOTE-CLIENTS.zh-tw.md#6-antigravity) |
   | Codex CLI | 安裝 plugin，並用 `/hooks` 信任它的 hook，見 [REMOTE-CLIENTS.zh-tw.md §7](REMOTE-CLIENTS.zh-tw.md#7-codex-cli) |
   | 自己寫的 script | Bearer key 只留給管理用的呼叫。資料相關的呼叫（`/v3/recall`、`/v3/conversation/add`、`/v3/session/add` 等）要送 `x-memory-user-key: sk-mem-…`，並在 body 帶 `team_id` 和 `agent_id`（或改用 `x-memory-team-id` / `x-memory-agent-id` header）。見 [API.zh-tw.md](API.zh-tw.md) |

每個 agent 都有自己的場景和 persona，agent 之間不共用任何東西。使用同一個 agent 的人會共用它的場景和 persona，但各自的對話和事實仍是私有的。除非你希望多個 client 共用，否則建議每個 client 各用一個 agent。詳見 [COLLABORATION.zh-tw.md](COLLABORATION.zh-tw.md#2-哪些是私有的哪些是共用的)。

## D. 確認結果

```bash
amem status                      # 顯示設定，queue 應為 0
amem recall "升級前曾經告訴 agent 的事"
```

開啟 Panel（`http://SERVER:8125`，用 user key 登入），選擇對應的 team 與 agent，就能看到舊的對話和記憶。在伺服器上，client 重新連上之後，`docker compose logs -f memory-core` 不應該出現授權錯誤。

## E. 還原

退版一定要連資料一起還原：遷移過的資料，舊版本無法正確讀取；0.4.0 之後的版本遇到比自己新的資料也會拒絕啟動。升級後寫入的內容都會遺失。

使用步驟 1 的備份，例如 `B=backups/20261019-101500`：

```bash
docker compose down                                   # 不會刪除 volume
git checkout "$(cat "$B/previous-commit")"
cp "$B/env" .env
# 換回原本的映像（清單在 $B/images）。若沒有保留映像，最後一步要改用
# `docker compose up -d --build` 重新 build 舊版。
docker image tag "memory-core:pre-upgrade-${B##*/}" memory-core:local
docker image tag "memory-panel:pre-upgrade-${B##*/}" memory-panel:local
docker compose run --rm --no-deps -T --entrypoint sh memory-core \
  -c 'find /data -mindepth 1 -delete && tar xzf - -C /data' < "$B/memory-core-data.tgz"
# Redis profile：
docker compose run --rm --no-deps -T --entrypoint sh redis \
  -c 'find /data -mindepth 1 -delete && tar xzf - -C /data' < "$B/redis-data.tgz"
# Postgres profile：
docker compose up -d postgres
docker compose exec -T postgres dropdb -U "${POSTGRES_USER:-memory}" --force "${POSTGRES_DB:-memory}"
docker compose exec -T postgres createdb -U "${POSTGRES_USER:-memory}" "${POSTGRES_DB:-memory}"
docker compose exec -T postgres psql -q -U "${POSTGRES_USER:-memory}" "${POSTGRES_DB:-memory}" < "$B/postgres.sql"
# 最後
docker compose up -d
```

確定不會再還原之後，可以用 `docker image rm memory-core:pre-upgrade-… memory-panel:pre-upgrade-…` 刪掉保留的映像。

Client 也要改回原本的設定。

## F. 疑難排解

| 症狀 | 原因與處理 |
| --- | --- |
| `Agent '…' already has L2/L3 memories` / `409 target_has_profile` | 找到了舊資料，但指定的 agent 已經有自己的場景，系統不會把兩組場景合併。請換一個新的 agent 名稱再執行一次 |
| `no active system admin` | 這套安裝沒有建立過管理者。`upgrade.sh` 會自動建立；手動做法是 B4 的 `init-admin` 指令 |
| Client 收到 `401` | Client 仍然只送 Bearer key，請照 C 節修改 |
| Client 收到 `403 not a member` | 該使用者不在這個 team。請在 Panel 的 Teams → members 加入 |
| `upgrade.sh` 顯示 `No .env here` | 請在既有的安裝目錄執行，不是在新 clone 的目錄 |
| 升級時印出的 key 遺失 | 存在 `upgrade-client-settings.txt`。也可以再跑一次 `./upgrade.sh`：資料不會重複搬，並會發一把新的 key |
| 用 Claude/Antigravity/Codex 蒸餾的登入狀態不見了 | 登入資料存在 `claude-config` / `antigravity-config` / `codex-config` volume，升級會保留。若你執行過 `docker compose down -v`，請重新登入：`docker compose exec -it memory-core agy`（或 `claude`、`codex login --device-auth`） |
