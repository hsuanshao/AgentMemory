# 伺服器與 Client 分離部署指引

[English](./REMOTE-CLIENTS.md) | 繁體中文

本文說明最常見的部署方式：**一台 Agent Memory 伺服器**（memory-core + Memory Panel）放在你管理的主機上，**其他機器上的 agent**（Claude Code、Codex、Antigravity、Hermes）透過網路讀寫記憶。

```text
 筆電 / 工作站                                     伺服器主機
┌───────────────────────────────┐   HTTPS   ┌──────────────────────────────┐
│ Claude Code ─┐                │
│ Codex ───────┤                │           │ 反向代理（TLS）              │
│ Antigravity ─┼─ amem ─────────┼──────────►│   └─► memory-core :8420      │
│ Hermes ──────┘  (user key)    │           │ Memory Panel :8125（管理者） │
│  ~/.config/agent-memory/      │           │ SQLite 或 Postgres（+Redis） │
│  ~/.local/share/agent-memory/ │           └──────────────────────────────┘
└───────────────────────────────┘
```

- 每台 client 機器只需要一個小型執行檔 **`amem`**。各工具的整合（Claude Code、Codex、Antigravity 的 plugin，以及 Hermes memory provider）都只是呼叫 `amem`。設定保存、secret 過濾、離線佇列都由它負責。
- Client 使用**個人的 user key**（`sk-mem-…`）驗證，完全不需要伺服器的 `AGENT_MEMORY_API_KEY`。

## 目錄

1. [存取權限如何運作](#1-存取權限如何運作)
2. [準備伺服器](#2-準備伺服器)
3. [建立使用者、團隊與 Agent（管理者）](#3-建立使用者團隊與-agent管理者)
4. [在每台 client 機器安裝 `amem`](#4-在每台-client-機器安裝-amem)
5. [Claude Code](#5-claude-code)
6. [Antigravity](#6-antigravity)
7. [Codex CLI](#7-codex-cli)
8. [Hermes Agent](#8-hermes-agent)
9. [從上一版升級](#9-從上一版升級)
10. [哪些資料會送出、哪些不會](#10-哪些資料會送出哪些不會)
11. [疑難排解](#11-疑難排解)

---

## 1. 存取權限如何運作

伺服器分成三個信任區：

| 區域 | 路由 | 憑證 |
| --- | --- | --- |
| **使用者範圍** | 記憶（L0–L3）、`recall`、`session/add`、`skill/*` | `x-memory-user-key` |
| **部門** | `meta/*`、pipeline、stats、生成紀錄；prompts、knowledge 與 `chat-memory/clear` 只限系統管理員 | 該部門使用者的 `x-memory-user-key` |
| **營運者** | `internal/*`、`department/destroy` | 只接受 `Authorization: Bearer <AGENT_MEMORY_API_KEY>` |

client 和部門管理員都不需要伺服器 API key；它只留在架設伺服器的人手上。

每一次記憶呼叫都必須指定 **team** 與 **agent**：

- **team_id 與 agent_id 為必填。** 已經沒有 `default` 桶了，而且 agent 必須屬於該 team。
- **成員資格：** 使用者必須是該 team 的有效成員。
- **只能存取自己：** 成員只能讀寫自己的記憶（`user_id` 預設為呼叫者本人），只有系統管理員能指定其他使用者。
- **隔離範圍：** L0 對話與 L1 atoms 依 team + agent + user 隔離；L2 場景與 L3 persona 則由同一組 team + agent 的所有人共用。

> **Agent 怎麼分？** 多個工具（例如 Claude Code *和* Hermes）共用同一個 agent，就會共用同一組場景與 persona。若希望每個工具各自累積，請建立不同的 agent（`claude-code`、`hermes`…）。

## 2. 準備伺服器

依 [README](../README.zh-tw.md#快速開始) 安裝（`./install.sh`）。要讓遠端 client 使用，必須能從 client 機器連到 core API。

1. **在 8420 前面加上 TLS。** user key 放在 header 中傳送，在受信任網路以外請勿使用明文 HTTP。Compose 的 `tls` profile 會為此啟動 Caddy（`BIND_ADDR=127.0.0.1`、`TLS_SITE=memory.example.com`、`COMPOSE_PROFILES=tls`；見 [TLS](ops/TLS.zh-tw.md)，也說明了 IP 位址與其他反向代理的情況）。或者把伺服器放在私有網路（VPN、Tailscale、WireGuard）中，使用內部位址。

2. **綁定位址。** 使用 `docker compose` 時，`.env` 的 `BIND_ADDR` 決定 8420 與 8125 port 聽在哪裡：`127.0.0.1`（只有這台機器，新安裝的預設；其他機器透過 `tls` profile 連入）或 `0.0.0.0`（所有網路）。若直接執行執行檔，請設定 `AGENT_MEMORY_HOST=0.0.0.0`；沒有設定 `AGENT_MEMORY_API_KEY` 時，伺服器會拒絕綁定非 loopback 位址。

3. **Panel 不要對外公開。** 只有管理者需要用到 Panel（port 8125），請放在內部位址或 SSO／VPN 後面，不要直接開放在網際網路上。

4. **從 client 機器確認連線：**

   ```bash
   curl -s https://memory.example.com/health
   ```

## 3. 建立使用者、團隊與 Agent（管理者）

用 `install.sh` 顯示的管理員 user key 登入 Memory Panel（要規劃需要幾個 team 和 agent、各自共用什麼，請見 [COLLABORATION.zh-tw.md](COLLABORATION.zh-tw.md)），在 **Metadata** 中：

1. **Users → New user**：立刻複製產生的 `sk-mem-…` key，它只會顯示一次。在該使用者的 **Keys** 可以再發新 key（建議每台機器一把），之後也能撤銷。
2. **Teams → New team**：選擇 owner（owner 會成為 team admin）。
3. 選取 team → **Members → Assign user**：加入要共用這個 team 記憶的使用者。
4. 選取 team → **Agents → New agent**：例如 `claude-code`、`antigravity`、`hermes`。

交給每位使用者四個值：**伺服器 URL**、**user key**、**team_id**（`team-…`）、**agent_id**（`agt-…`）。id 可以在 Teams 與 Agents 表格中看到。若伺服器使用的部門不是 `default`，也要提供 **department id**。

非管理者也可以用自己的 key 登入 Panel，以唯讀方式瀏覽自己的記憶。

## 4. 在每台 client 機器安裝 `amem`

`amem` 是單一執行檔（`crates/memory-client`）。從 0.2.0 起，每個 [release](https://github.com/hsuanshao/TeamAgentMemory/releases) 都附有預先編譯好的版本，支援 Linux（x86_64 和 arm64，靜態連結）和 macOS（Apple silicon 和 Intel）：

```bash
VERSION=0.5.2
PLATFORM=linux-x86_64   # 或 linux-arm64、macos-arm64、macos-x86_64
BASE=https://github.com/hsuanshao/TeamAgentMemory/releases/download/v$VERSION
curl -fsSLO "$BASE/amem-$VERSION-$PLATFORM.tar.gz"
curl -fsSL "$BASE/SHA256SUMS" | grep " amem-$VERSION-$PLATFORM.tar.gz$" | shasum -a 256 -c
tar -xzf "amem-$VERSION-$PLATFORM.tar.gz"
mkdir -p ~/.local/bin
install -m 755 "amem-$VERSION-$PLATFORM/amem" ~/.local/bin/amem

amem --version
```

也可以用 Rust toolchain 自行編譯：

```bash
# 從本 repo 的 checkout 安裝
cargo install --path crates/memory-client

# 或直接從 git 安裝
cargo install --git https://github.com/hsuanshao/TeamAgentMemory --tag v0.5.2 memory-client
```

請確認 agent 工具看得到的 `PATH` 包含 `amem` 所在的目錄（上面的 `~/.local/bin`，或 `cargo install` 用的 `~/.cargo/bin`）。桌面應用程式啟動時的 `PATH` 有時比 shell 短。在 macOS 上，用瀏覽器而不是 `curl` 下載的檔案會被隔離，用 `xattr -d com.apple.quarantine ~/.local/bin/amem` 解除。

接著登入一次。`--user-key -` 會從 stdin 讀取 key，避免留在 shell 歷史紀錄中：

```bash
amem login --server-url https://memory.example.com \
           --team-id team-xxxxxxxxxxxx --agent-id agt-xxxxxxxxxxxx \
           --user-key -
# 貼上 sk-mem-… key 後按 Enter
```

`login` 存檔前會先做一次測試 recall。key 錯誤、不屬於該 team、或 agent 屬於其他 team，都會立刻提示。設定存放在 `~/.config/agent-memory/client.json`，只有你本人可讀。

```bash
amem status                 # 顯示設定（key 會遮蔽）與佇列數量
amem recall "release plan"  # 印出 agent 會收到的內容
```

同一台機器上的所有工具共用這份設定。也可以用 `AMEM_SERVER_URL`、`AMEM_USER_KEY`、`AMEM_TEAM_ID`、`AMEM_AGENT_ID`、`AMEM_DEPARTMENT_ID` 針對單一程序覆寫。若要讓每個工具使用不同的 agent，請使用下面各 plugin 的設定，或在該工具的環境變數中設定 `AMEM_AGENT_ID`。

有些 prompt 不是人打的，例如 orchestrator 輸入給 agent 的訊息。把 `AMEM_SKIP_PROMPT_PREFIXES`（或 `client.json` 的 `skip_prompt_prefixes`）設成以逗號分隔的前綴清單，例如 `[AGEND-`。去掉開頭空白後以其中一個前綴開頭的 prompt 不會拿來 recall，整輪也不會被 capture。在 agent 還在工作時送出的訊息，會當成目前這一輪的一部分被 capture。這類訊息若以略過前綴開頭，只會略過那一則，不影響它所在的那一輪。只比對 prompt 的開頭，所以人打的 prompt 只是中間出現前綴時照常 capture。預設為空，也就是不略過任何 prompt。

Recall 會回兩段：和 prompt 相關的記憶，以及 persona 和 scene 導覽；後者在同一個 session 的多數 prompt 都一樣。在 Claude Code 裡，記憶每個 prompt 都會注入；persona 和 scene 導覽只在內容和這個 session 上次收到的不同時才注入，Claude Code 啟動、resume、`/clear`、fork 或壓縮 context（`SessionStart`）之後也會再注入一次。另外每 20 個 prompt 會重複一次，以涵蓋沒有 `SessionStart` 的壓縮或 `/rewind` 把它們捲掉的情況。把 `AMEM_SYSTEM_CONTEXT_EVERY`（或 `client.json` 的 `system_context_every`）設成別的 `N`、設成 `0` 只在變動時注入，或設成 `1` 像以前一樣每個 prompt 都注入。判斷依據按 session 存在 `~/.local/share/agent-memory/state/claude-code/`。

## 5. Claude Code

Plugin 位於 `clients/claude-code`，透過本 repo 的 marketplace（`.claude-plugin/marketplace.json`）發佈。

```bash
claude plugin marketplace add hsuanshao/TeamAgentMemory
claude plugin install agent-memory@agent-memory
```

若有本機 checkout，也可以改用 `claude plugin marketplace add /path/to/TeamAgentMemory`。

啟用 plugin 時，Claude Code 會詢問設定：Server URL、User key（存在系統金鑰圈）、Team ID、Agent ID、Department ID、要略過的 prompt 前綴。**全部留空就會沿用 `amem login` 的設定**；填寫的話只對 Claude Code 生效，常見用法是另外指定 `agent_id`。

Plugin 的行為：

| Hook | 行為 |
| --- | --- |
| `SessionStart` | 記下這個 session 的 context 是新的（啟動、resume、`/clear`、fork、壓縮），下一個 prompt 會再附上 persona 和 scene 導覽。 |
| `UserPromptSubmit` | 依你的 prompt 進行 recall，把記憶加入 context（最多約 9,500 字元）。persona 和 scene 導覽只在和這個 session 上次收到的不同時加入，另外每 20 個 prompt 加入一次（`AMEM_SYSTEM_CONTEXT_EVERY`）。 |
| `Stop` | 送出這一輪：你的 prompt、回覆，以及所有 tool call 與結果。只送出上次 Stop 之後新增的內容。 |
| MCP server `agent-memory` | 提供 `memory_recall` 工具，讓 Claude 可以主動搜尋記憶。 |

伺服器出錯也不會卡住 Claude Code。hook 會安靜結束，錯誤寫入 `~/.local/share/agent-memory/amem.log`。

## 6. Antigravity

Plugin 位於 `clients/antigravity`（`plugin.json`、`hooks.json`、`mcp_config.json`）。從 checkout 安裝：

```bash
agy plugin install /path/to/TeamAgentMemory/clients/antigravity
agy plugin list
```

或把整個目錄複製到 `~/.gemini/config/plugins/agent-memory/`。

行為：

| Hook | 行為 |
| --- | --- |
| `PreInvocation` | 每一輪的第一次模型呼叫（`invocationNum == 0`）時，依你最新的需求進行 recall，以 ephemeral message 注入。 |
| `Stop` | 讀取對話 transcript，送出上次 Stop 之後新增的步驟（含 tool call）。 |
| MCP server `agent-memory` | 提供 `memory_recall` 工具。 |

注意事項：

- **Transcript 格式：** Antigravity 沒有公開 transcript 格式的文件。`amem` 讀取的是目前版本寫出的格式（`USER_INPUT`、`PLANNER_RESPONSE`、各種工具步驟）。若日後更新改變了格式，寫入會停止並在 `amem.log` 中說明原因，但 recall 仍可運作。
- **既有的 MCP 設定檔：** 若手動把 MCP server 加進 `~/.gemini/config/mcp_config.json`，請確認它是合法的 JSON，空檔案不算。

## 7. Codex CLI

Plugin 位於 `clients/codex`（`.codex-plugin/plugin.json`、`hooks/hooks.json`、`.mcp.json`），透過本 repository 的 Codex marketplace（`.agents/plugins/marketplace.json`）發布。

```bash
codex plugin marketplace add hsuanshao/TeamAgentMemory      # 或本機 checkout：/path/to/TeamAgentMemory
codex plugin add agent-memory@agent-memory
```

接著啟動一次 `codex`，執行 **`/hooks`**，檢查並信任這個 plugin 的三個 hook。Codex 不會執行還沒被信任的 hook，所以在信任之前，什麼都不會召回或寫入。自動化情境可以用 `codex exec --dangerously-bypass-hook-trust`，略過那一次執行的檢查。

Plugin 使用 `amem login` 的設定。若要讓 Codex 用獨立的 agent，啟動時在環境變數設定 `AMEM_AGENT_ID=agt-…`，hook 和 MCP server 都會沿用。

Plugin 的行為：

| Hook | 行為 |
| --- | --- |
| `UserPromptSubmit` | 依 prompt 召回記憶並加進 context。Plugin 把 Codex 單一 hook 的輸出上限調高到 4,000 tokens（`additionalContextLimit`），並記下這一回合的 prompt。 |
| `PostToolUse` | 記下每一次工具呼叫與結果。 |
| `Stop` | 送出整個回合：prompt、工具呼叫與結果、最終回覆。若某回合在 `Stop` 前被中斷，會跟下一回合一起送出。 |
| MCP server `agent-memory` | 工具 `memory_recall`。 |

注意事項：

- **不解析 transcript**：Codex 官方說明 session 檔的格式不穩定，所以 `amem` 不讀它，而是用官方文件化的 hook payload 組出整個回合，在 `Stop` 之前暫存在 `~/.local/share/agent-memory/state/codex/`。只會寫入最終回覆，不包含 Codex 在工具呼叫之間的簡短訊息。
- **Codex 本身的記憶**和 Agent Memory 是分開的，兩者互不影響。

## 8. Hermes Agent

Provider 位於 `clients/hermes/agentmemory`，並沿用 provider 名稱 `agentmemory`，既有的 `memory.provider: agentmemory` 設定可以繼續使用。

```bash
# 1. 移除 Hermes 原始碼目錄裡的舊版（它的優先順序較高，會蓋過新版）
rm -rf ~/.hermes/hermes-agent/plugins/memory/agentmemory

# 2. 以使用者 plugin 安裝（Hermes 更新後仍會保留）
mkdir -p ~/.hermes/plugins
cp -r /path/to/TeamAgentMemory/clients/hermes/agentmemory ~/.hermes/plugins/

# 3. 選用它（已設定過可略過）
hermes config set memory.provider agentmemory
hermes memory status
```

Hermes 會讀取 `amem login` 的設定。Provider 的行為：

- `prefetch` → 每一輪開始前執行 `amem recall`。
- `sync_turn` → 以 `amem capture --format openai` 送出上一輪之後新增的訊息（含 tool call）。由單一背景 worker 依序送出，對話不必等待網路。
- 子 agent、cron 與 flush 情境不會寫入。
- 提供 `agentmemory_search` 工具。

若 Hermes 的 `PATH` 找不到 `amem`，請在 Hermes 的環境變數設定 `AMEM_BIN=/完整路徑/amem`。舊版 provider 用的 `~/.hermes/.env` 中 `AGENTMEMORY_API_KEY`、`AGENTMEMORY_BASE_URL`、`AGENTMEMORY_SERVICE_ID` 已不再讀取，可以刪除。

## 9. 從上一版升級

上一版的 client 可以不指定 team 和 agent 就寫入，這些資料都在舊的 `default` 桶裡，而新版資料平面已不再提供這個桶。請在伺服器上更新後的安裝目錄執行 `./upgrade.sh`。它會先備份，接著重建服務，把舊資料搬到你指定的 team 與 agent，最後印出 client 要用的 key 和 `amem login` 指令。每個步驟、全手動的作法和還原方式，都寫在 [UPGRADE.zh-tw.md](UPGRADE.zh-tw.md)。之後依第 4–8 節設定各個 client。

日後要從某台機器移除 client，在 checkout 目錄執行 `./uninstall.sh client` 即可；可以先加 `--dry-run` 看會移除哪些東西。詳見 [UNINSTALL.zh-tw.md](UNINSTALL.zh-tw.md)。

## 10. 哪些資料會送出、哪些不會

- **文字**（你的訊息與 agent 的回覆）送到 L0，再由伺服器的管線蒸餾成 atoms（L1）、場景（L2）與 persona（L3）。
- **Tool call 與結果**送到 skill buffer，讓反覆出現、大量使用工具的工作能整理成可重用的 skill。每個 tool 結果只保留前 8,000 字元，每則訊息最多 32,000 字元。
- **Secret 會在你的機器上、排入佇列或送出之前就先過濾。** 涵蓋：
  - 私鑰區塊
  - URL 中的帳密
  - JWT
  - Anthropic、OpenAI 與記憶 user key
  - GitHub、Slack、AWS、Google 的 key
  - bearer token
  - `password=`、`token:`、`api_key` 這類賦值
  - 用自然語言提到密碼並接著寫出值，涵蓋英文和亞洲主要語言（`資料庫密碼是 …`、`mật khẩu: …`、`pw …`）
  - 長得像密碼的字串：8 個以上字元，混合字母、數字和 `!@#$` 這類符號

  符合的內容會換成 `[REDACTED:<種類>]`。memory-core 在儲存前會用同一套規則再過濾一次，所以直接寫進它 API 的資料也涵蓋在內（[DISTILLATION §7](ops/DISTILLATION.zh-tw.md#7-機密與遮罩)）。`amem` 一律使用內建規則，server 的 `redaction` 設定（關閉形狀規則、額外的 patterns）不會改變 client 遮掉的內容。過濾是以樣式比對進行，請把它當成安全網，而不是保證。
- **離線佇列：** 伺服器連不上（或 key 被拒）時，資料會暫存在 `~/.local/share/agent-memory/queue/`，只有你本人可讀。下一輪會依序補送，也可以手動執行 `amem flush`。最多保留 2,000 筆請求，超過時從最舊的開始丟棄。被伺服器判定為格式錯誤的請求會移到 `queue/failed/` 供檢查。
- **子 agent**（Claude Code 的 side chain、Hermes 的子 agent）不會被記錄。

## 11. 疑難排解

| 症狀 | 原因／解法 |
| --- | --- |
| `amem login` → `HTTP 401` | user key 錯誤或已撤銷。請到 Panel（Users → Keys）重新發一把。 |
| `HTTP 429 too_many_failures` | 這個位址（或同一個 NAT 後面的其他人）太多次用錯 key，被擋最多 15 分鐘。修正 key 後等 `Retry-After` 過去。管理員可以在 Panel → Metadata → Audit 看到這些嘗試。 |
| `the server redirects to https://…` | 伺服器改用 HTTPS 了。`amem` 不跟隨轉址（key 會被一起帶走）；請用 `amem login --server-url https://…` 重新登入。 |
| `amem login` 出現憑證錯誤 | 伺服器的憑證來自私有 CA（IP 位址時是 Caddy 自己的 CA）。登入時加上 `--ca-cert caddy-root.crt`（[TLS §3](ops/TLS.zh-tw.md#3-信任-caddy-的-caip-位址或內部名稱)）。 |
| `HTTP 403 agent does not belong to the team` | `agent_id` 屬於其他 team。請確認你所屬 team 的 Agents 表格。 |
| `HTTP 403 not an active member of the team` | 請管理者把你加入該 team。 |
| `HTTP 400 team_id is required` | client 沒有設定 team／agent。請執行 `amem login` 或填寫 plugin 設定。 |
| `amem status` 顯示佇列一直增加 | 這台機器連不到伺服器。請檢查 URL 與 TLS，然後執行 `amem flush`。 |
| 什麼都 recall 不到 | 新記憶要等伺服器管線蒸餾後才會出現，最多需要幾分鐘。請到 Panel 的 Dashboard 查看管線錯誤（例如伺服器沒有設定 LLM）。 |
| Hook 看起來沒有作用 | 查看 `~/.local/share/agent-memory/amem.log`，hook 不會大聲報錯。並確認工具的 `PATH` 找得到 `amem`。 |
| Hermes 仍在使用舊版 provider | `~/.hermes/hermes-agent/plugins/memory/agentmemory` 中的舊版優先順序較高，請刪除它（見第 7 節）。 |

Client 機器上的檔案：

| 路徑 | 內容 |
| --- | --- |
| `~/.config/agent-memory/client.json` | `amem login` 的設定（0600）。可用 `AMEM_CONFIG` 改位置。 |
| `~/.local/share/agent-memory/queue/` | 離線佇列（0600）。可用 `AMEM_DATA_DIR` 改基準目錄。 |
| `~/.local/share/agent-memory/state/` | 每個 session 的游標，確保每一輪只送一次。 |
| `~/.local/share/agent-memory/amem.log` | hook 與寫入的錯誤紀錄。 |
