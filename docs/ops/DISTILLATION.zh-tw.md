# 監看與修復蒸餾

[English](DISTILLATION.md)

memory-core 會在背景蒸餾對話：L1 擷取記憶，L2 整理成 scene，L3 改寫 persona。每一步都會呼叫設定好的 LLM。這份文件說明如何確認蒸餾是否正常運作，以及出問題時怎麼處理。

## 目錄

- [1. 會記錄哪些資訊](#1-會記錄哪些資訊)
- [2. 在哪裡查看](#2-在哪裡查看)
- [3. 常見的失敗原因](#3-常見的失敗原因)
- [4. 更換模型](#4-更換模型)
- [5. Dead letter 與重跑](#5-dead-letter-與重跑)
- [6. 工具與蒸餾](#6-工具與蒸餾)
- [7. 機密與遮罩](#7-機密與遮罩)
- [8. Embedding 與負載](#8-embedding-與負載)

## 1. 會記錄哪些資訊

每一次呼叫到 LLM 的 L1、L2、L3 執行，都會留下一筆生成紀錄：

| 狀態 | 意思 |
| --- | --- |
| `succeeded` | 寫出了記憶、scene 或 persona |
| `empty` | LLM 有回應，但沒有寫出任何東西：沒有值得保留的內容，或回應沒有改變任何東西。連續出現大量 `empty`，可能代表模型回答得不好 |
| `failed` | 呼叫失敗（額度用完、逾時、沒有輸出），或回應無法使用。`error_message` 會說明原因 |
| `dropped` | 和 `failed` 一樣，但發生在 L1 或 L2 的**最後一次**嘗試：這批輸入被略過，讓該 session 或 agent 可以繼續（見下方） |

每筆紀錄也會記下使用的模型。CLI provider 會記成指令加上參數選用的模型，例如 `cli:agy:gemini-3.8-flash-high`。

失敗的 job 會在 5、15、45 秒後重試（`worker.maxRetries`，預設 3 次）。會重複發生的失敗不會重試：模型想使用工具，而蒸餾永遠不允許使用工具（見 #37）。回應不是該步驟要求的 JSON 時仍會重試，因為下一次的回應可能就正常。每次嘗試都失敗，或失敗會重複發生時，這個 job 就會成為 **dead letter**，並保留最後一次的錯誤和失敗時使用的模型。dead letter 之後會怎麼處理，見第 5 節。

**L1 和 L2 會 drop 無法蒸餾的那一批。** L1 依序處理一個 session，L2 依序處理一個 agent 的 atom，兩者都從游標開始，而游標只有成功時才會前進。如果某一批每次嘗試都失敗，之後每次執行都會先遇到同一批，後面的內容就全部被擋住。所以在最後一次嘗試仍失敗時：
- 游標會越過這一批，之後的對話或 atom 照常蒸餾；
- 這次執行記錄為 `dropped`，附上錯誤和輸入的 id（`input_refs`）；
- dead letter 保存的是只處理這一批的 job（`range: {first, last}`，儲存序號）。重跑它只會重做這一批，不會動到游標。

不會刪除任何東西：對話仍在 L0，atom 仍在 L1。L3 和 skill job 沒有游標，失敗不會擋住任何東西，所以不會被 drop。

| Coordination backend | Dead letter 存放位置 |
| --- | --- |
| `redis` | 存在 Redis，保留最新的 10,000 筆，所有 replica 共用 |
| `local` | 存在記憶體，保留最新的 1,000 筆，重新啟動後就會消失 |

## 2. 在哪裡查看

**Panel → Dashboard → Distillation**：
- 目前使用的 LLM；
- 過去 24 小時各層 succeeded、empty、failed 的次數與失敗率；
- 最近幾筆失敗和它們的錯誤；
- 各類 job 的 dead letter 數量。

System admin 還會看到重跑 dead letter 的按鈕。**Panel → Generation logs** 列出每一次執行；可以依狀態篩選（`#/logs?status=failed`），點開單筆就能看到錯誤、模型、輸入和輸出。

**在伺服器上**，於安裝目錄執行：

```bash
# 過去 24 小時各層、各狀態的次數，以及最近幾筆失敗
docker compose exec memory-core memory-server generations
docker compose exec memory-core memory-server generations --status empty --layer l1 --hours 6

# Dead letter，新的在前
docker compose exec memory-core memory-server dead-letters
docker compose exec memory-core memory-server dead-letters --kind l1 --json
```

這些指令會呼叫正在執行的伺服器 API，所以不論使用哪種 storage 或 coordination backend，行為都一樣。`--department` 可以選擇 `default` 以外的部門。背後用到的 API 端點見 [API.zh-tw.md](../API.zh-tw.md#健康檢查與監控)。

## 3. 常見的失敗原因

| 錯誤（在 `error_message` 或 dead letter 中） | 可能原因 | 處理方式 |
| --- | --- | --- |
| `RESOURCE_EXHAUSTED`、`quota`、`429` | LLM 帳號的額度用完了 | 等額度重置，或換一個有獨立額度的模型（第 4 節），之後再重跑 |
| `` `agy` produced no output; stderr: … `` | CLI 正常結束但沒有輸出回應，後面附上它寫到 stderr 的內容 | 通常是暫時性的，會自動重試。如果同一批一直失敗，先看 stderr 的部分，或換個模型試試 |
| `` `…` timed out after … `` | 回應時間超過 `llm.timeoutMs` | 調高逾時，或換一個比較快的模型 |
| `llm_output_invalid` | 回應不是該步驟要求的 JSON | 模型不太遵守指示，換一個模型 |
| `LLM returned 5xx` | OpenAI 相容端點出錯 | 檢查端點；短暫的中斷通常靠重試就能撐過 |
| `LLM_TOOL_REQUEST: …`（舊的紀錄是 `… auto-denied …`） | 模型想使用工具，而不是回答（第 6 節） | 不用處理；這批輸入會被 drop，不會重試。如果常常發生，改用 memory-core 會關閉工具的 CLI，或換一個模型 |
| `Unknown feature flag: …` | 新版 `codex` 改名或移除了某個要關閉的 feature（第 6 節） | 更新 memory-core；更新前可以先設 `AGENT_MEMORY_LLM_CLI_HARDENING=off` |

## 4. 更換模型

CLI provider 的模型寫在 `.env` 的參數裡：

```bash
AGENT_MEMORY_LLM_CLI_ARGS=["-p","{prompt}","--model","gemini-3.8-flash-high"]
```

OpenAI 相容 provider 則是 `AGENT_MEMORY_LLM_MODEL`。換之前先確認模型能回應、也還有額度，例如 `docker compose exec memory-core agy -p "Reply OK" --model gemini-3.8-flash-high`。確認後執行 `docker compose up -d memory-core` 套用。重新啟動前還在執行的 job，重啟後會再派送一次。

## 5. Dead letter 與重跑

**Dead letter 會自動處理。** 每 5 分鐘有一個清理程序（sweeper）檢查一次：

| Dead letter 的狀況 | 處理方式 |
| --- | --- |
| 第一次失敗超過一天，或已經重跑 3 次 | **移除**。它的失敗紀錄仍保留在生成紀錄中（`failed` / `dropped`，附錯誤和輸入 id），失去的只有重做的入口 |
| 暫時性失敗（額度、逾時、服務中斷、回應無效） | 失敗後 1 小時**自動重跑**，之後依序間隔 2 小時、4 小時。每次最多 10 筆 |
| 會重複發生的失敗（工具被拒絕） | 等到設定的模型變更後，才像其他 dead letter 一樣重跑；同一個模型只會再失敗一次。在開始記錄模型之前寫入的 dead letter，視為用目前的模型失敗 |

不論手動或自動，每次重跑都算進 3 次。相關設定在 `memory-core.yaml` 的 `worker` 區段：`deadLetterTtlHours`（24）、`deadLetterMaxRedrives`（3）、`deadLetterRedriveAfterMinutes`（60）、`deadLetterSweepSeconds`（300）、`deadLetterRedriveBatch`（10）。Panel 會顯示每筆的重跑次數；`/health` 會統計 `dead_letters_redriven` 和 `dead_letters_expired`。

- **0.2.3 以前寫入的 dead letter** 沒有時間紀錄，會被當成已超過一天，升級後第一次清理就會移除。如果還有想保留的，請在升級前先手動重跑。
- **手動重跑是 admin 的覆寫**：即使已經重跑 3 次也可以再手動重跑。這也算一次重跑，所以如果又失敗，下一次清理就會移除。

修好原因之後，可以不等自動重跑，手動把 dead letter 排回佇列：

```bash
docker compose exec memory-core memory-server dead-letters redrive --limit 10
docker compose exec memory-core memory-server dead-letters redrive --kind skill --limit 20
```

Panel 上的 **Re-run** 按鈕效果相同，每次 10 筆。重跑的 job 會從第一次嘗試重新開始，最舊的先跑。有多個 replica 時，每筆 dead letter 只會由其中一個重跑。

請小批重跑，並在每批之間觀察失敗情況：一次把積壓全部送出，可能又把 LLM 的額度用完。重跑可以安全地重複執行：
- L1 job 會從該 session 的游標接著處理，不會重複處理同樣的訊息。
- 帶 `range` 的 job（被 drop 的輸入）只會重做那一批。如果又失敗，會回到同一筆 dead letter，不會再被 drop 一次。
- L2、L3 和 skill job 處理的是 agent 或 task 當下的狀態。

要查哪些內容被 drop、原因是什麼，請看 **Panel → Generation logs**（`#/logs?status=dropped`）或 `memory-server generations --status dropped`。Dashboard 的 Distillation 卡片會列出各層被 drop 的次數。

## 6. 工具與蒸餾

> [!WARNING]
> **只要 `AGENT_MEMORY_LLM_CLI_ARGS` 讓 CLI 拿到比預設更多的能力，不論 `AGENT_MEMORY_LLM_CLI_HARDENING` 怎麼設，memory-core 都不會啟動。** 錯誤訊息會指出是哪個參數，請把它拿掉。見[會被拒絕的參數](#會被拒絕的參數)和 [UPGRADE](../UPGRADE.zh-tw.md#升級到-040)。

蒸餾讀的是對話內容，而對話內容不可信，任何參與對話的人都能在裡面寫指令。所以蒸餾模型永遠不會拿到工具，也不會略過權限檢查（#37）。memory-core 的做法是：

- 已知的 agent CLI，在讀到 prompt 之前先把它自帶的工具關掉；
- 每個 CLI 都會在 prompt 裡被告知沒有工具；
- 模型如果還是想用工具，會以 `LLM_TOOL_REQUEST` 失敗。這種失敗不會重試：L1/L2 直接 drop 那批輸入，它的 dead letter 會等到模型更換後才重跑（[第 5 節](#5-dead-letter-與重跑)）。

### Agent CLI

預設是 `llm.cli.hardening: auto`（環境變數 `AGENT_MEMORY_LLM_CLI_HARDENING`）。memory-core 會把以下參數加在 `AGENT_MEMORY_LLM_CLI_ARGS` 前面，這些參數只會拿掉能力：

| CLI | 加上的參數 | 效果 |
| --- | --- | --- |
| `claude` | `--tools "" --strict-mcp-config --disable-slash-commands` | 沒有內建工具，不載入設定裡的 MCP server，沒有 skill 和 slash command |
| `codex` | 對 `apps`、`plugins`、`remote_plugin`、`shell_tool`、`unified_exec`、`code_mode_host`、`multi_agent`、`goals`、`skill_search`、`tool_suggest`、`browser_use`、`browser_use_external`、`computer_use`、`in_app_browser`、`image_generation` 加上 `--disable`；另加 `-c web_search=disabled`、`-c sandbox_mode="read-only"`，並在 `exec` 後面加上 `--ignore-user-config` | 沒有 connector、plugin、shell 或程式執行、子 agent、瀏覽和網路搜尋。不讀 `config.toml`，所以裡面的 MCP server 和 profile 都不會載入 |
| `agy` 和其他 CLI | 不加 | 沒有已知的關閉方式。工具呼叫在 headless 模式下會被拒絕，以 `LLM_TOOL_REQUEST` 失敗 |

memory-core 啟動時會記錄加上了哪些參數，或警告沒辦法加。

**驗證方式。** 在容器裡用同樣的參數問 CLI 它有幾個工具：

```bash
docker compose exec -T memory-core sh -c 'echo "How many tools do you have? Reply with just the number." | claude --tools "" --strict-mcp-config --disable-slash-commands -p'
```

回答應該是 `0`。`codex` 如果試圖用工具，輸出裡會出現 `code-mode host is disabled`，但這一次仍然會正常回答。

**codex 與 `config.toml`。** 蒸餾用的 codex 不會讀 `$CODEX_HOME/config.toml`，但仍然從 `CODEX_HOME` 讀取登入資訊。需要的設定請放在 `AGENT_MEMORY_LLM_CLI_ARGS`，例如 `-c model=…` 或 `-c model_reasoning_effort=high`。在 codex 0.163 上，`unified_exec` 沒辦法關閉；負責執行指令的 `shell_tool` 已經關閉。hardening 開啟時，沒有 `exec` 的 codex 參數會被拒絕，因為只有 `exec` 能跳過 `config.toml`。

**`hardening: off`** 會照原樣傳入參數，並記錄一則警告。只在 CLI 新版改了參數名稱、memory-core 還沒跟上時暫時使用。會被拒絕的參數照樣會被拒絕。

### 會被拒絕的參數

memory-core 啟動時、以及執行 `memory-server check-config` 時，會檢查 `AGENT_MEMORY_LLM_CLI_ARGS`，並拒絕：

- `--dangerously-skip-permissions`、`--allow-dangerously-skip-permissions`、`--dangerously-bypass-approvals-and-sandbox`，以及其他所有 `--dangerously-…`；
- `--yolo`、`--full-auto`、`--allowedTools` / `--allowed-tools`、`--mcp-config`、`--permission-prompt-tool`；
- `default`、`plan` 以外的 `--permission-mode`；
- claude：帶工具清單的 `--tools`；
- codex：沒有 `exec` 的參數；`--enable …`、`--search`、`-p` / `--profile`、`read-only` 以外的 `-s` / `--sandbox`，以及這些 `-c` 設定：`mcp_servers.…`、`features.…=true`、`disabled` 以外的 `web_search`、`read-only` 以外的 `sandbox_mode`、`sandbox_permissions`、`approval_policy`、`profile`。值直接接在短參數後面的寫法（`-cKEY=VALUE`、`-sMODE`、`-pNAME`）也一樣會被檢查。`-c model_reasoning_effort=high`、`-c service_tier=fast` 這類設定沒有問題。

### 自架模型（OpenAI 相容端點）

memory-core 送出的 request 從來不帶 `tools`。所以如果回應只有工具呼叫，就表示模型想採取行動。以下任一情況、而且沒有其他文字時，memory-core 會判定為 `LLM_TOOL_REQUEST`：
- `finish_reason: "tool_calls"` 或 `"function_call"`；
- 訊息裡有 `tool_calls` 或 `function_call`；
- 工具呼叫格式的文字，例如 `<tool_call>…</tool_call>`。

回應裡如果同時有答案，就拿掉工具呼叫的部分後照常使用，模型順手多寫了一個工具呼叫不會因此被 drop。空的回應如果沒有工具呼叫，仍然視為暫時性失敗而重試。

## 7. 機密與遮罩

蒸餾會把讀到的內容變成長期記憶，所以機密在儲存前就要拿掉。每個命中都會換成 `[REDACTED:<kind>]`。

**在哪裡遮罩。** `amem` client 在排入佇列或送出前先遮罩一次。memory-core 在每條寫入路徑上再遮罩一次，所以直接呼叫 HTTP API 的 client 也涵蓋在內：

- 對話（`/v3/conversation/add`）和原始 session 紀錄（`/v3/session/add`）。原始紀錄裡，欄位名稱是機密類（`password`、`pass`、`pin`、`otp`、`dbPassword`、`access_token`、`SECRET_KEY_BASE`、`credentials` 等）的值會整個換成 `[REDACTED:secret-field]`，因為文字規則看不到欄位名稱；
- skill 萃取緩衝（`/v3/skill/conversation/add`）、skill 內容，以及 UTF-8 文字格式的 skill 檔案（`.sh`、`.env.example` 等）；
- 透過 API 寫入的 atom，以及 L1 模型寫出的 atom（模型可能寫出輸入裡沒有的機密）；
- 場景和 persona，不論來自 L2／L3 或 API。

L1 和 skill 的 prompt 也要求模型不得記錄任何憑證的值。

**規則**（`crates/memory-client/redact`，`amem` 和 memory-core 共用）：

| Kind | 抓什麼 |
| --- | --- |
| `private-key`、`jwt`、`openai-key`、`github-token` 等 | 已知的 token 和金鑰格式 |
| `secret-assignment` | 英文鍵名的 `password=…`、`api_key: …`。鍵名含機密字詞即算（`DB_PASSWORD=`、`SECRET_KEY_BASE=`、`GITHUB_TOKEN_RO=`）；鍵名以設定類字詞結尾（`count`、`limit`、`length`、`type` 等）或是在數 `tokens` 的，才是設定值，所以 `max_tokens=`、`token_count:` 不算，`RATE_LIMIT_TOKEN=`、`MAX_API_KEY=` 仍會被遮 |
| `secret-phrase` | 用自然語言提到密碼，後面接著值。關鍵字含常見縮寫：英文（`password`、`pw`、`pwd`、`pass`、`psw` 等）、中文（`密碼`、`密码`、`口令`、`金鑰` 等）、日文（`パスワード`、`パス` 等）、韓文（`비밀번호`、`비번` 等）、越南文（`mật khẩu`、`mk`）、泰文、印尼／馬來文、印地文、阿拉伯文、西班牙文、葡萄牙文、法文、德文、土耳其文和俄文。接在 `:`、`=`、`：` 之後的值一律遮掉；接在「是」這類字（`是`、`là`、`は`、`는`、`es` 等）或空白之後的，只遮看起來像機密的值（含數字、符號或大小寫混合）。有日常意思的字（`token`、`pass`、`mk`、`clave`、`パス`、`トークン`、`รหัส` 等）必須接 `:`、`=` 或「是」這類字才算，所以 `token 4096`、`tests pass 100ms` 不會被遮 |
| `password-shape` | 不需要關鍵字：8 個以上的 ASCII 字元，同時包含字母、數字，以及夾在中間的 `!@#$%^&+=?~\|` 其中之一。不會遮掉的有：網址、路徑、`x=1` 這類設定值、email、query string（`?page=2&sort=asc1`）、程式碼（以 `&&`、`\|\|`、`!=`、`==` 連接識別字或數字，例如 `cmd1&&cmd2`），以及整串都是模型名稱的寫法（`4@qwen3-8b`、`Claude@Opus-5.5`）。字串裡只是含有模型名稱還不夠，`Falcon#2024x` 仍會被遮 |

**設定。**

```yaml
redaction:
  passwordShape: true          # AGENT_MEMORY_REDACTION_PASSWORD_SHAPE
  patterns:                    # AGENT_MEMORY_REDACTION_PATTERNS（JSON 陣列）
    - kind: tw-national-id
      pattern: '\b[A-Z][12]\d{8}\b'
    - kind: phone
      pattern: '(?P<keep>\btel:\s*)\+?\d[\d -]{7,}\d'
```

- `keep` 群組會保留該部分（這裡是 `tel:` 標籤）。
- 規則無法編譯，或 `AGENT_MEMORY_REDACTION_PATTERNS` 不是 JSON 陣列時，memory-core 會拒絕啟動；`memory-server check-config` 也會回報。
- `.env` 裡的 `$` 會被 Docker Compose 展開，要寫 `$$`，或改寫在設定檔裡。
- 身分證字號、電話這類個資預設不遮。哪些格式重要因國家和部署而異，請在這裡自行加入。
- 這些設定只影響 memory-core。`amem` 一律使用內建規則（含形狀規則），所以關掉 `passwordShape` 或加入 patterns，都不會改變 client 送出前遮掉的內容。

**部門規則。** 部門的系統管理者可以在內建和部署設定的規則之外，加入自己的規則：在 Panel（Metadata → Redaction）或透過 `/v3/meta/redaction-rule/*`（[API](../API.zh-tw.md)）。其他使用者、MCP 和蒸餾模型都看不到也改不了。

- 每條規則包含一個 kind（顯示為 `[REDACTED:<kind>]`，不能和內建的重複）和一個 Rust regex，最多 500 字元。無法編譯、會匹配空字串或過大的 pattern 會被拒絕。每個部門最多 50 條啟用中的規則。
- **Try it**（`test`）用部門現有的規則，加上還沒存的 pattern，跑一段範例文字，顯示遮罩結果和各 kind 命中幾次。範例文字不會被儲存。
- 新增或修改的規則，在同一個 replica 上下一次寫入就生效；其他 replica 30 秒內跟上。已存的資料不會被改寫。
- 刪除規則就是停用。誰在什麼時候新增、修改的紀錄會保留，之後也可以重新啟用。
- 讀不到規則時，這個部門的寫入會失敗，不會在沒有規則的情況下照樣寫入。

**限制。**

- 規則是啟發式的，只是安全網，不是保證。
- 升級前、或規則加入前已經存進去的資料不會被改寫。
- skill 的二進位資源檔（非 UTF-8）不會掃描。

## 8. Embedding 與負載

開啟 embedding 時，每個 atom 存入時都會算向量。這個呼叫失敗或逾時，atom 還是會存下來：在補上向量之前，recall 只能用關鍵字找到它，無法用語意找到。

**補向量。** memory-core 每隔 `backfillIntervalSecs`（預設 600 秒；啟動後先等一個間隔才跑第一次）會替沒有向量的 atom 補算，每個部門每次最多 `backfillBatch` 個。

- 每次從上次停下的地方接著做，依 atom 寫入的順序，做到最後再從頭開始。
- 服務拒絕輸入的 atom（HTTP 400、413、422，例如內容超過模型上限）會跳過，不論連續幾筆，都不會擋住後面的 atom；下一輪再試。
- 服務因為自身原因（連不上、逾時、5xx、429）連續三筆失敗時，視為服務掛了，這個部門留到下次再做。
- 連續拒絕 32 筆輸入時，視為設定錯誤（例如 `model` 打錯，或模型不支援 `dimensions`）：這一輪停止，並記一則 `embedding service refuses every input; check memory.embedding model and dimensions` 的 log，`reembed` 也會顯示同樣的訊息。
- atom 本身不會被改寫，所以 L2 不會把它當成新的。

要立刻跑一輪並看結果：

```bash
docker compose exec memory-core memory-server reembed
docker compose exec memory-core memory-server reembed --department default --limit 500 --json
```

**設定。**

| 設定 | 環境變數 | 預設 | |
| --- | --- | --- | --- |
| `memory.embedding.timeoutMs` | `AGENT_MEMORY_EMBEDDING_TIMEOUT_MS` | 10000 | 每次呼叫 embedding 服務的逾時 |
| `memory.embedding.maxConcurrency` | `AGENT_MEMORY_EMBEDDING_MAX_CONCURRENCY` | 4 | 同時進行的請求數，超過的排隊等候 |
| `memory.embedding.backfillIntervalSecs` | `AGENT_MEMORY_EMBEDDING_BACKFILL_INTERVAL_SECS` | 600 | `0` 關閉背景補向量；`reembed` 仍可使用 |
| `memory.embedding.backfillBatch` | | 200 | 每個部門每次背景補算的 atom 數 |
| `worker.concurrency` | `AGENT_MEMORY_WORKER_CONCURRENCY` | 8 | 同時執行的 pipeline 工作（L1/L2/L3/skill） |
| `llm.cli.maxConcurrency` | `AGENT_MEMORY_LLM_CLI_MAX_CONCURRENCY` | 2 | 同時執行的 CLI 模型程序 |

補向量和 L1、`atomic/update` 共用 `maxConcurrency`：服務中斷後大量補算的期間，它們的 embedding 呼叫可能要排隊。補向量每次請求送 32 個 atom。

embedding 伺服器規格小或與其他服務共用、負載高時會逾時：先調低 `maxConcurrency`，再調高 `timeoutMs`。服務中斷後有大量 atom 沒有向量：等服務恢復後跑一次 `reembed`。

數值型的環境變數無法解析時（例如 `AGENT_MEMORY_EMBEDDING_MAX_CONCURRENCY=abc`），memory-core 會在啟動時停止並指出是哪一個；`memory-server check-config` 也會回報。
