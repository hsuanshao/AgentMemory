# SQLite → PostgreSQL 搬移：內部設計

[English](STORAGE-MIGRATION.md)

說明 `memory-server migrate-storage` 如何運作，以及儲存 schema 改變時要更新哪些地方。實際執行搬移請看 [MIGRATE-TO-POSTGRES.zh-tw.md](../MIGRATE-TO-POSTGRES.zh-tw.md)。

## 目錄

- [1. 組成](#1-組成)
- [2. 一次執行做了什麼](#2-一次執行做了什麼)
- [3. 複製資料列](#3-複製資料列)
- [4. 衍生資料](#4-衍生資料)
- [5. 安全性保證](#5-安全性保證)
- [6. Schema 改變時](#6-schema-改變時)
- [7. 已知限制](#7-已知限制)

## 1. 組成

| 元件 | 角色 |
| --- | --- |
| `crates/memory-infrastructure/src/sqlite_to_postgres.rs` | 搬移本身：資料表清單、每個 instance 一個交易、驗證、報告 |
| `crates/memory-server/src/migrate_storage.rs` | `migrate-storage` 子指令：載入伺服器的正常設定、用伺服器的 embedding 維度連線、印出報告 |
| `migrate-to-postgres.sh` | 包在外層的維運流程：備份、修改 `.env`、dry run、搬移、切換、健康檢查 |
| `crates/memory-infrastructure/tests/sqlite_to_postgres.rs` | 對真實 Postgres 的端到端檢查（[TESTING.zh-tw.md §2](TESTING.zh-tw.md#2-postgres-與-redis-整合測試)） |

子指令透過 `Config::load()` 讀設定，所以讀到的和伺服器完全一樣：`AGENT_MEMORY_DATA_DIR`、`AGENT_MEMORY_POSTGRES_URL`、embedding 設定。這也是腳本用 `docker compose run memory-core` 執行它的原因：同一個映像、同一個 `/data` volume、同一份環境變數。

## 2. 一次執行做了什麼

對每個 `<data>/instances/<id>/memory.db`，依 instance id 排序：

1. **計數**：數 SQLite 和該 instance 的 Postgres schema 裡每張表的筆數。目標是從 registry（`agent_memory.instances`）查詢，不會建立它，所以 dry run 不會寫入任何東西。
2. **判斷**（`plan()`）：
   - 目標的每張表都是空的 → 搬移；
   - 目標的每張表都和來源筆數相同 → `already_migrated`，跳過；
   - 其他情況 → 回報 `target_not_empty` 錯誤，並列出不一致的資料表。不會覆蓋，也不會合併。

   `meta_config_params` 不列入這個判斷，因為 Postgres 的 DDL 會先寫入預設值。
3. **搬移**（dry run 時不執行）：透過 `PgDb::schema()` 建立 schema（和伺服器同一條路徑，包含 embedding 欄位），然後在**單一交易**內複製所有資料表。
4. **推進 sequence**：對原樣複製了 `seq` 的資料表。
5. **驗證**：在交易內再數一次每張表；只要有一張對不上就回傳錯誤，整個交易 rollback。
6. **提交。**

instance 之間互相獨立：某個 instance 失敗時，之前完成的 instance 仍保持已提交。重跑時會跳過它們（步驟 2），只重試剩下的。

## 3. 複製資料列

資料表清單是 `TABLES` 常數：列出 `sqlite/schema.rs` 的每張表和欄位，每個欄位標記為以下其中一種：

| 類型 | SQLite | Postgres |
| --- | --- | --- |
| `Text` | `TEXT`／`NULL` | `TEXT` |
| `Int` | `INTEGER`／`NULL` | `BIGINT` |
| `Bool` | `INTEGER` 0/1 | `BOOLEAN` |
| `Json` | 存放 JSON 的 `TEXT` | `JSONB` |

型別不符的值會直接報錯，不會默默轉換。

資料列依 `rowid` 順序讀取，每次 1,000 筆，用多列 `INSERT` 寫入，每次綁定的參數控制在 30,000 個以內（Postgres 上限是 65,535）。

**序號原樣保留。** `conversations.seq` 和 `atoms.seq` 原封不動複製，之後再把各自的 `BIGSERIAL` sequence 設到複製過來的最大值之後（`setval(pg_get_serial_sequence(…), max(seq) + 1, false)`）。這很重要，因為管線的游標就是這些數字：session state 的 `l1_cursor` 對應 `conversations.seq`，profile state 的 `l2_cursor` 對應 `atoms.seq`。重新編號的話，管線會重複蒸餾或漏掉對話。`generation_refs.seq` 只存在於 Postgres，這些資料列會依 `rowid` 順序取得新的編號。

`meta_config_params` 使用 `ON CONFLICT … DO UPDATE`，所以 SQLite 裡改過的值會取代預設值。

不複製的：FTS5 資料表（`*_fts`）、`index_meta` 和 sqlite-vec 索引。它們都是衍生資料（見下一節）。

## 4. 衍生資料

Postgres 把關鍵字 token 和向量存在資料列本身。搬移時用**和 Postgres adapter 寫入時相同的函式**計算它們，所以搬過去的資料列和伺服器自己寫入的沒有差別：

| 資料表 | 欄位 | 來源 | 等同於 |
| --- | --- | --- | --- |
| `conversations` | `tokens` | `tsvector_literal(content)` | `PostgresMemoryStore::append` |
| `atoms` | `tokens` | `tsvector_literal(content + " " + background)` | `PostgresMemoryStore::upsert` |
| `atoms` | `embedding` | SQLite 的 `embedding` BLOB（little-endian `f32`） | 帶 embedding 的 `upsert` |
| `skills` | `tokens` | 只有 head 版本：`tsvector_literal(index_source(doc))`；舊版本為 `NULL` | `PostgresSkillStore::append_version` |

只有在目標有 `embedding` 欄位（有設定 embedding），而且存的向量維度剛好相符時，才會複製向量。否則它會計入 `embeddings_dropped` 並略過；atom 本身仍會複製。

## 5. 安全性保證

- **不寫入任何來源資料列。** 開啟 SQLite 資料庫時會走正常的初始化流程，可能會建立缺少的資料表（`CREATE … IF NOT EXISTS`），除此之外只讀取。
- **每個 instance 全有或全無**，由交易和筆數檢查保證。
- **絕不覆蓋或合併**非空的目標（步驟 2）。
- **可重複執行**：重跑會補上缺的、跳過已完成的。
- **來源必須保持靜止。** 每一頁是在各自的 SQLite 讀取交易中讀取的，如果伺服器在搬移期間寫入，可能產生不一致的快照，這時筆數檢查通常會失敗。這就是維運流程要先停掉 memory-core 的原因。子指令本身無法判斷伺服器是否還在執行。

## 6. Schema 改變時

SQLite 和 Postgres 的 DDL 是一張表對一張表的對應（`sqlite/schema.rs`、`postgres/schema.rs`）。改 schema 時，搬移也要跟著改：

| 改了什麼 | 還要更新 |
| --- | --- |
| 新增資料表 | 加進 `TABLES`，除非它是衍生資料 |
| 新增、改名或刪除欄位 | `TABLES` 裡對應的項目（名稱和類型，依 DDL 順序） |
| Postgres 新增衍生欄位（token、向量……） | 新增一個 `Derived` case，用 adapter 自己的函式計算 |
| adapter 改了 token 的來源文字 | 對應的 `Derived` case。盡量共用同一個函式，就像 `index_source` 那樣 |
| 新的資料表有從 SQLite 原樣複製的 `BIGSERIAL` | 設定 `copies_seq: true` |
| Postgres DDL 會預先寫入資料的新資料表 | 設定 `upsert` 及其衝突條件 |

如果 `TABLES` 沒有依序列出 SQLite 的每張表和每個欄位，單元測試 `covers_every_sqlite_table_and_column` 就會失敗，所以前兩項不會被默默漏掉。其餘的由真實 Postgres 的測試把關（搜尋、向量、版本、序號）；新增衍生欄位時，請一併擴充那個測試。

## 7. 已知限制

- **管線狀態不在搬移範圍內。** 它屬於協調後端。使用 `local` 協調時，它留在 `/data/system.db`，因為序號有保留，游標仍然對得上。把協調後端從 `local` 換成 Redis 時，這些狀態不會帶過去：之後有新對話進來的 session 會從第一則對話開始處理，較早的對話可能會再被蒸餾一次。未來可以做一個 `migrate-coordination`，把 `session_states` 和 `profile_states` 複製到 Redis。
- **只能單向。** 沒有 Postgres → SQLite 的搬移。退回的方式是使用未被修改的 SQLite 檔案和備份的 `.env`，切換後寫入的資料會遺失。
- **記憶體用量**以每張表一頁（1,000 筆）為上限，但每個 instance 是單一交易。非常大的 instance 代表目標端會有一個很長的交易。
