# 從 SQLite 搬到 PostgreSQL

[English](MIGRATE-TO-POSTGRES.md)

SQLite 是預設的儲存後端：不用另外安裝，每個部門一個檔案。當服務規模變大（使用者、agent、記憶越來越多），PostgreSQL 會是更合適的選擇：資料庫可以放在獨立的託管伺服器上、有自己的備份，而且多個 memory-core 副本可以共用它。本指南說明如何把既有安裝的資料一次搬過去。你可以讓 `./migrate-to-postgres.sh` 自動完成（[A](#a-自動遷移migrate-to-postgressh)），也可以手動執行相同步驟（[B](#b-手動逐步執行相同步驟)）。

## 目錄

- [什麼時候該搬](#什麼時候該搬)
- [哪些會搬、哪些不動](#哪些會搬哪些不動)
- [開始之前](#開始之前)
- [A. 自動遷移：`migrate-to-postgres.sh`](#a-自動遷移migrate-to-postgressh)
- [B. 手動：逐步執行相同步驟](#b-手動逐步執行相同步驟)
- [C. 確認結果](#c-確認結果)
- [D. 還原](#d-還原)
- [E. 疑難排解](#e-疑難排解)
- [下一步：多個副本](#下一步多個副本)

## 什麼時候該搬

如果一台 memory-core 就能順利服務整個團隊，繼續用 SQLite 就好。遇到以下情況再考慮 PostgreSQL：

- 想跑**一個以上的 memory-core 副本**（放在負載平衡器後面，或為了可用性）。SQLite 檔案無法在多台機器之間共用。
- 希望資料放在**託管資料庫**裡，使用維運團隊既有的時間點還原、讀取副本和監控。
- 想用 SQL 工具查詢或統計記憶資料。

## 哪些會搬、哪些不動

| | 搬到哪裡 |
| --- | --- |
| 對話（L0）、記憶（L1）、場景與 persona 檔案（L2/L3） | PostgreSQL，id、版本與時間戳都不變 |
| 使用者、user key、團隊、成員、agent、資產、ACL、設定 | PostgreSQL，內容不變。**client 繼續用同一把 key 和同樣的 id，不必修改** |
| Skill（所有版本）、記憶 prompt、生成紀錄 | PostgreSQL |
| 關鍵字搜尋索引 | 在 PostgreSQL 重建（`tsvector`），中文也支援 |
| 向量（有開 embedding 時） | 複製到 pgvector。維度和目前 embedding 設定不同的向量不會搬；那筆記憶改用關鍵字召回 |
| 管線進度（哪些對話已經蒸餾過） | 留在協調後端，而且仍然對得上：搬移時保留每一列的序號，所以不會重複蒸餾，也不會漏掉 |
| memory-core `/data` 底下的檔案（skill 資源、session 物件） | 留在同一個 volume，兩種後端都使用它 |
| SQLite 檔案 | **完全不動**，這就是你的退路 |

`/data/departments` 底下的每個部門都會搬。每個部門在單一交易內完成，提交前會比對 SQLite 和 PostgreSQL 的各表筆數。如果中途出錯，PostgreSQL 不會留下該部門的任何資料，直接重跑即可。

## 開始之前

- 你需要伺服器上的 shell，位置在安裝目錄（有 `.env` 的那個目錄），並且有 Docker Compose v2 和 `curl`。
- 先把程式碼更新到包含 `migrate-to-postgres.sh` 的版本：

  ```bash
  cd /path/to/TeamAgentMemory          # 有 .env 的目錄
  git pull
  ```

- **預留停機時間。** 從備份開始到改用 PostgreSQL 重新啟動，memory-core 都是停止的，通常幾分鐘；步驟 4／B3 的 dry run 會顯示總共有多少筆資料。請先停止或暫停會寫入記憶的 agent；使用 `amem` 佇列的 client 之後也會自動重送。
- **選擇目標資料庫。**
  - *用 Docker 跑 PostgreSQL*（預設）：腳本會加上 `postgres` profile，在 memory-core 旁邊跑 `pgvector/pgvector:pg16`，並自動產生密碼。
  - *既有的伺服器*：需要可用 [pgvector](https://github.com/pgvector/pgvector) 擴充的 PostgreSQL（內建選項和測試都使用 PostgreSQL 16）。連線的使用者需要該資料庫的 `CREATE` 權限。有開 embedding 時，`vector` 擴充必須已經存在，或該使用者有權建立它。請使用**空的資料庫**：如果某個部門的 schema 已經有不同的資料，搬移會拒絕寫入。同一台機器上的伺服器要用 `host.docker.internal` 連線，不能用 `localhost`。

## A. 自動遷移：`migrate-to-postgres.sh`

```bash
./migrate-to-postgres.sh
```

它會先問你資料要搬到哪裡，接著：

1. **備份**：停止 memory-core 後，複製 `.env`，並把 memory-core 的 `/data` volume 打包到 `backups/pre-postgres-<時間>/`。
2. **把 `.env` 指向 PostgreSQL**：設定 `AGENT_MEMORY_POSTGRES_URL`（選 Docker 時也會設定 `POSTGRES_PASSWORD` 和 `postgres` profile）。儲存後端暫時仍是 `sqlite`。
3. 用你的程式碼**重新 build memory-core**。
4. **顯示即將搬移的內容**（dry run，不寫入任何資料），並詢問是否繼續。
5. **搬移並驗證**每個部門。
6. **切換**：把 `AGENT_MEMORY_STORAGE_BACKEND` 改成 `postgres`，啟動服務，並確認 `/health` 回報 `"backend":"postgres"`。

自動化部署：

```bash
MIGRATE_TARGET=postgres-docker ./migrate-to-postgres.sh --non-interactive
MIGRATE_TARGET=postgres-url AGENT_MEMORY_POSTGRES_URL=postgres://memory:…@db.internal:5432/memory \
  ./migrate-to-postgres.sh --non-interactive
```

`MIGRATE_SKIP_BACKUP=1` 會跳過 `/data` 的打包（`.env` 仍會備份）；只有在你用其他方式備份了這個 volume 時才使用。

搬移成功後再執行一次不會有影響：它看到 `AGENT_MEMORY_STORAGE_BACKEND=postgres` 就會直接結束。如果中途失敗，重跑即可，已經搬完的部門會被認出並跳過。

## B. 手動：逐步執行相同步驟

### B1. 備份並停止服務

```bash
B=backups/pre-postgres-$(date +%Y%m%d-%H%M%S); mkdir -p "$B"; chmod 700 "$B"
cp .env "$B/env"
docker compose stop memory-core
docker compose run --rm --no-deps -T --entrypoint tar memory-core czf - -C /data . > "$B/memory-core-data.tgz"
```

### B2. 把 `.env` 指向 PostgreSQL

編輯 `.env`，`AGENT_MEMORY_STORAGE_BACKEND=sqlite` 先不要改。

- 用 Docker 跑 PostgreSQL：在 `COMPOSE_PROFILES` 加上 `postgres`（以逗號分隔），把 `POSTGRES_PASSWORD` 設成一組夠長的隨機值，並設定 `AGENT_MEMORY_POSTGRES_URL=postgres://memory:<該密碼>@postgres:5432/memory`。
- 既有的伺服器：設定 `AGENT_MEMORY_POSTGRES_URL=postgres://user:pass@host:5432/db`。

### B3. 先看再搬（dry run）

```bash
docker compose build memory-core
docker compose run --rm -T memory-core migrate-storage --dry-run
```

搬移在一次性的 memory-core 容器裡執行：和正式服務同一個映像、同一個 `/data` volume、同一份設定，所以資料目錄和 embedding 維度都會和 memory-core 實際使用的一致。開啟 `postgres` profile 時，`compose run` 會先啟動 `postgres` 服務。輸出會列出每個部門和各表的筆數，例如：

```text
department default: would be copied
  conversations                 18342
  atoms                          2210
  profile_files                    96
  meta_users                        4
  ...
  embeddings: 2210 kept, 0 dropped (embeddings off or another dimension)
Dry run: nothing was written.
```

這一步不會寫入任何資料。如果目標資料庫已經有其他資料，會在這裡出現 `target_not_empty` 錯誤。

### B4. 搬移

```bash
docker compose run --rm -T memory-core migrate-storage
```

每個部門最後都會顯示 `copied and verified`。兩個指令都可以加上 `--json`，輸出給程式讀取的報告。

### B5. 切換並啟動

把 `.env` 的 `AGENT_MEMORY_STORAGE_BACKEND` 改成 `postgres`，然後：

```bash
docker compose up -d
curl -s http://127.0.0.1:${CORE_PORT:-8420}/health   # "backend":"postgres"
```

## C. 確認結果

- `/health` 顯示 `"stores": {"backend": "postgres", …}`。
- 用平常的 user key 登入 Panel：使用者、團隊、agent 和記憶都還在。
- 在 client 上執行 `amem status`（或讓有擷取的 agent 跑下一輪對話），不需要改任何設定就能成功。
- 再跑一次 `docker compose run --rm -T memory-core migrate-storage --dry-run`，每個部門都會顯示 `already migrated, skipped`。請在 memory-core 開始寫入新資料之前做這個檢查，否則筆數會不同。

## D. 還原

SQLite 檔案從頭到尾都沒有被修改。要改回 SQLite：

```bash
cp backups/pre-postgres-<時間>/env .env
docker compose up -d
```

**切換之後**寫入的記憶只存在 PostgreSQL，不會帶回 SQLite。如果 `/data` volume 本身損壞，先還原備份檔：

```bash
docker compose stop memory-core
docker compose run --rm --no-deps -T --entrypoint sh memory-core -c 'rm -rf /data/* && tar xzf - -C /data' \
  < backups/pre-postgres-<時間>/memory-core-data.tgz
```

## E. 疑難排解

| 狀況 | 原因與處理 |
| --- | --- |
| `set AGENT_MEMORY_POSTGRES_URL …` | `.env` 裡沒有 URL（見步驟 B2）。 |
| `cannot connect to postgres at …` | 主機、port 或密碼錯誤，或容器連不到伺服器。同一台機器上的伺服器請用 `host.docker.internal`。 |
| `pgvector is required when embeddings are enabled` | 在伺服器上安裝 pgvector，或請 superuser 在目標資料庫執行 `CREATE EXTENSION vector`。 |
| `target_not_empty … drop schema am_… first` | 目標資料庫已經有該部門的另一份資料（之前的測試，或上次切換後寫入的資料）。請改用空的資料庫；如果確定那份資料可以丟掉，也可以刪除訊息中的 schema，以及它在 `agent_memory.departments` 裡的那一列。 |
| `embeddings: N dropped` | 這些向量的維度和現在 `AGENT_MEMORY_EMBEDDING_DIMENSIONS` 的設定不同，或 embedding 沒有開啟。記憶本身都有搬過去，召回時會用關鍵字找到它們。 |
| 搬到一半出錯停止 | 錯誤之前已回報完成的部門都已提交；出錯的那個部門不會留下任何資料。排除原因後重跑即可。 |

## 下一步：多個副本

PostgreSQL 讓資料庫可以共用。要跑多個 memory-core，還需要另外兩樣：

- **共用的 `/data` volume。** 不論用哪種後端，skill 資源和原始 session 物件都是 `/data/departments/<id>/` 底下的檔案，所以所有副本都必須掛載同一份儲存（例如 NFS 或其他網路 volume）。詳見 [ARCHITECTURE.zh-tw.md §8](ARCHITECTURE.zh-tw.md#8-後端與部署拓撲)。
- **共用的協調後端**（管線計時、工作佇列和鎖）。

協調後端用 Redis：設定 `AGENT_MEMORY_COORDINATION_BACKEND=redis` 和 `AGENT_MEMORY_REDIS_URL`（或加上 `redis` profile），詳見 README 的[儲存與協調後端](../README.zh-tw.md#儲存與協調後端)。請在這次遷移之後另外進行。`local` 後端保存的管線進度（在 `/data/system.db`）不會帶到 Redis：切換之後，有新對話進來的 session 會從頭開始處理，所以它較早的對話可能會再被蒸餾成記憶一次。挑 session 都閒置的時候切換（例如週末），可以把影響降到最低。
