# 移除

[English](UNINSTALL.md)

Agent Memory 分成兩部分，要分開移除：**client** 裝在每台跑 agent 的機器上，**伺服器**裝在跑 Docker Compose 的那台機器上。`./uninstall.sh` 可以移除任一部分。下面也會說明腳本實際做了什麼，你可以照著手動完成同樣的事。

## 目錄

- [快速參考](#快速參考)
- [A. 移除 client](#a-移除-client)
- [B. 移除伺服器](#b-移除伺服器)
- [C. 疑難排解](#c-疑難排解)

## 快速參考

```bash
./uninstall.sh client                 # 在跑 agent 的機器上
./uninstall.sh server                 # 在伺服器的安裝目錄；保留資料
./uninstall.sh server --purge-data    # 連所有記憶、使用者、key 一起刪除
```

| 選項 | 說明 |
| --- | --- |
| `--dry-run` | 只列出會移除哪些東西，不實際刪除 |
| `--yes` | 不逐一詢問；搭配 `--purge-data` 時也會略過它的確認 |
| `--purge-data` | 只用於 `server`：連 Docker volume（記憶、使用者、key、Claude/Antigravity/Codex CLI 登入）一起刪除 |

請在本 repository 的 checkout 目錄下執行。沒有安裝的部分會直接略過，所以重複執行也沒關係。

## A. 移除 client

```bash
./uninstall.sh client --dry-run   # 先看
./uninstall.sh client
```

會移除以下項目：

| 項目 | 移除的內容 | 手動做法 |
| --- | --- | --- |
| Claude Code | 各 scope（local、project、user）的 `agent-memory` plugin、`agent-memory` marketplace、plugin 快取 | `claude plugin uninstall agent-memory@agent-memory --scope <scope>`、`claude plugin marketplace remove agent-memory`、`rm -rf ~/.claude/plugins/cache/agent-memory` |
| Codex | `agent-memory` plugin、它的 marketplace、plugin 快取 | `codex plugin remove agent-memory@agent-memory`、`codex plugin marketplace remove agent-memory`、`rm -rf ~/.codex/plugins/cache/agent-memory` |
| Antigravity | 使用者層級的 plugin，以及執行腳本那個目錄裡的 workspace 版本 | `agy plugin uninstall agent-memory`、`rm -rf ~/.gemini/config/plugins/agent-memory`；在專案目錄執行 `rm -rf .agents/plugins/agent-memory` |
| Hermes | 從 `clients/hermes` 安裝的 provider（`$HERMES_HOME/plugins/agentmemory`，預設是 `~/.hermes`） | `rm -rf ~/.hermes/plugins/agentmemory` |
| `amem` | 設定（含 user key）、離線佇列與游標、執行檔 | `rm -rf ~/.config/agent-memory ~/.local/share/agent-memory`，再執行 `cargo uninstall memory-client` |

注意事項：

- **尚未送出的資料**：如果離線佇列裡還有對話（例如伺服器連不上時累積的），腳本刪除前會先警告。請先執行 `amem flush` 把它們送出。
- **裝在個別專案的 plugin**：只會在你執行腳本時所在的目錄尋找，包括 Claude Code 的 `local` 與 `project` scope，以及 Antigravity 的 `.agents/plugins/agent-memory`。請到每個這樣的專案裡執行，例如 `cd ~/my-project && /path/to/AgentMemory/uninstall.sh client`，或手動移除。
- **Hermes**：腳本只會移除會呼叫 `amem` 的 provider。同名的舊版 provider（例如放在 Hermes 原始碼目錄裡的那份）不會被動到。移除後，如果 `memory.provider` 仍是 `agentmemory`，請改成別的 provider：`hermes config set memory.provider <name>`。
- **自訂路徑**：和安裝時一樣，會依照 `AMEM_CONFIG`、`AMEM_DATA_DIR`、`XDG_CONFIG_HOME`、`XDG_DATA_HOME`、`CODEX_HOME`、`HERMES_HOME` 的設定。
- **不會動到伺服器上的資料**：這台機器送出的記憶仍留在伺服器上。若也要刪除，請在 Panel 刪除該使用者的資料，或照 B 節移除伺服器。

## B. 移除伺服器

在伺服器的安裝目錄（有 `.env` 的那個目錄）執行：

```bash
./uninstall.sh server --dry-run   # 先看
./uninstall.sh server             # 保留資料
```

執行內容：

1. 停止並刪除這套安裝的所有容器：memory-core、Panel，以及有啟用 profile 時的 Postgres、Redis。手動做法：`docker compose --profile postgres --profile redis down --remove-orphans`。
2. 刪除為這套安裝 build 的映像：`memory-core:<MEMORY_IMAGE_TAG>` 和 `memory-panel:<MEMORY_IMAGE_TAG>`，tag 預設是 `local`。手動做法：`docker image rm memory-core:local memory-panel:local`。若另一套部署用了相同的 tag、還在使用這個映像，腳本會保留它並提示你。
3. **保留資料 volume**：裡面是記憶、使用者、key，以及 Claude／Antigravity CLI 的登入資料。之後在同一個目錄再執行一次 `./install.sh`，並選擇沿用 `.env`，就能全部恢復。

### 連資料一起刪除

```bash
./uninstall.sh server --purge-data
```

這會連 volume 一起刪除（手動做法：`docker compose --profile postgres --profile redis down --volumes`），也會刪除 `upgrade.sh` 為了還原而保留的 `:pre-upgrade-…` 映像（清單在 `backups/*/images`），這套安裝的所有記憶、使用者、key 和 CLI 登入都會永久消失。因此腳本會要求你輸入安裝目錄的名稱來確認。如果之後可能還需要這些資料，請先備份，見 [UPGRADE.zh-tw.md 的 B1](UPGRADE.zh-tw.md#b1-備份)。

如果用的是外部 Postgres（`AGENT_MEMORY_POSTGRES_URL`），那個資料庫不會被刪除，請用你平常的工具處理。

### 移除之後

- 目錄裡還留著含 伺服器 API key 的 `.env`；升級過的話，還有 `backups/` 和 `upgrade-client-settings.txt`。不再需要時，把整個目錄刪掉即可。
- 仍指向這台伺服器的 client 會一直把資料存在本機佇列，請照 A 節移除。

## C. 疑難排解

| 症狀 | 原因與處理 |
| --- | --- |
| `No .env here` | `server` 必須在安裝目錄執行。請先 `cd` 到該目錄，或在那裡用路徑呼叫腳本 |
| `Image … is still used by another container` | 這台機器上另一套部署用了相同的 `MEMORY_IMAGE_TAG`，所以刻意保留。等那套也移除後再刪 |
| 執行 `client` 後 plugin 仍在清單裡 | 它是用別的名稱或從別的目錄安裝的。請用 `claude plugin list` / `agy plugin list` 確認，再依上表手動移除 |
| `Cancelled; nothing removed.` | 輸入的確認文字和目錄名稱不同。請重新執行，並輸入完全相同的名稱 |
