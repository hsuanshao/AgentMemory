# 變更紀錄

[English](CHANGELOG.md)

各版本的重要變更。細節與升級步驟見 [release notes](https://github.com/hsuanshao/AgentMemory/releases)；跨版本升級見 [UPGRADE.zh-tw.md](docs/UPGRADE.zh-tw.md)。格式依照 [Keep a Changelog](https://keepachangelog.com/zh-TW/1.1.0/)，版號依照[語意化版本](https://semver.org/lang/zh-TW/)（1.0 之前，次版本可能改變資料或 API，並會在說明中註明）。

## [未發布]

**升級前請先閱讀 [UPGRADE → 升級到 0.4.3](docs/UPGRADE.zh-tw.md#升級到-043)。** 退回 0.4.2 必須還原備份（schema 遷移 6）。

### 安全性
- **User key 改存 SHA-256 雜湊。** Schema 遷移 6 原地轉換已儲存的 key；現有的 key 照常可用，Panel 顯示最後四個字元。呼叫端自己指定的 key（`init-admin`、`user/create-with-key`）必須是 `sk-mem-` 加上至少 32 個英數字、`-` 或 `_`（[UPGRADE](docs/UPGRADE.zh-tw.md#user-key-改存雜湊)）。
- **被拒絕的憑證依 client 位址節流**：15 分鐘內從第 5 次起，每次拒絕會晚一點回應（250 ms，倍增到最多 5 秒）；第 20 次會讓這個位址被擋 15 分鐘（429 `too_many_failures`、`Retry-After`）。只採信 `server.trustedProxies`（`AGENT_MEMORY_TRUSTED_PROXIES`，新增；Compose 預設信任 Panel 與 Caddy）送來的 `X-Forwarded-For`。門檻：`server.throttle` 或 `AGENT_MEMORY_THROTTLE_{DELAY_AFTER,LOCK_AFTER,LOCK_MINUTES}`（設 0 關閉，適用所有 client 都顯示同一個位址的部署）。使用 Redis 協調時計數由所有 replica 共用（[UPGRADE](docs/UPGRADE.zh-tw.md#錯誤的-key-會被節流並記錄)）。
- **憑證稽核紀錄**：拒絕、封鎖、以及用伺服器 API key 發出的 key（`init-admin`、`internal/upgrade`），記錄時間、位址、轉送鏈、user agent、部門、路徑與 key 雜湊的開頭。跨部門保存（`audit.db` / `agent_memory.audit_events`），保留 90 天（`AGENT_MEMORY_AUDIT_RETENTION_DAYS`）。系統管理員在 Panel → Metadata → Audit（`/v3/audit/list`）看自己部門的；營運者用 `/v3/internal/audit/list` 看全部。
- **`internal/upgrade` 只在有 legacy 資料要搬時才發 key**，或設了 `AGENT_MEMORY_ALLOW_KEY_RECOVERY=true`（新增）以找回遺失的管理者 key。
- **蒸餾模型的輸入改成 JSON 資料**：L1 萃取與去重、L2 場景、L3 persona 都和 skill 萃取一樣，對話內容無法結束資料、也無法假裝成指令（[UPGRADE](docs/UPGRADE.zh-tw.md#蒸餾模型的輸入改成-json-資料)）。
- **新安裝的連接埠只聽在本機**：`install.sh` 會詢問，預設 `127.0.0.1`（`BIND_ADDR`，新增）；`upgrade.sh` 加上 `BIND_ADDR=0.0.0.0`，既有安裝照舊。可選的 Compose profile `tls` 在 memory-core 與 Panel 前面放一個提供 HTTPS 的 Caddy（[TLS](docs/ops/TLS.zh-tw.md)）。
- Panel：登入的 key 只留在這個瀏覽器分頁（sessionStorage，不再用 localStorage），撤銷的 key 10 秒內失效（原本 60 秒），驗證結果以雜湊快取，並送出 Content-Security-Policy。Panel 會把瀏覽器的位址轉給 Core；驗證 key 時遇到封鎖（429）或 Core 錯誤，會照實轉回，不再說成 key 無效。
- `amem` 與營運指令（`memory-server generations`、`dead-letters`）不跟隨轉址，key 不會被送到別的位址。`amem` 建立的檔案一開始就只有擁有者能讀；`amem login --ca-cert`（`AMEM_CA_CERT`）可以信任私有 CA，例如 Caddy 為 IP 位址簽發憑證用的 CA。`install.sh` 與 `upgrade.sh` 經由 stdin 把伺服器 API key 交給 curl，不再放在命令列上。
- `amem` 依主程式上限截斷召回內容時，會關閉已開啟的區塊。
- 內部錯誤或依賴服務無法使用時只回應錯誤代碼與 request id；細節（路徑、主機、查詢）只寫在 memory-core 的 log。
- 已離開 team 的 agent 擁有者不能再修改這個 agent 或它的 binding；binding 記錄實際設定它的呼叫者（請求裡的 `created_by` 會被忽略）。
- `init-admin` 依部門序列化，同時呼叫兩次只會建立一位第一位管理者。
- 使用者、team、asset 的名稱篩選把 `%` 和 `_` 當成一般字元。

### 修正
- `amem recall` 把 `--` 之後的每個字都當成查詢；Hermes 外掛也這樣傳，以 `-` 開頭的查詢不會被當成選項。
- 所有提案都過期未決的 skill 萃取任務每小時關閉一次，不再於每次列表時讀取。

## [0.4.2] - 2026-10-24

**升級前請先閱讀 [UPGRADE → 升級到 0.4.2](docs/UPGRADE.zh-tw.md#升級到-042)。** 這一版收緊了存取權限：部門只能由 `init-admin` 建立、伺服器 API key 不再能開啟部門路由、蒸餾用的 CLI 改用環境變數白名單、萃取出的 skill 要先核准。退回 0.4.1 必須還原備份（schema 遷移 4 與 5）。

### 安全性
- **skill 萃取只提案，由人核准。** 模型拿到的封存對話是一個 JSON 陣列，裡面的文字（網頁、工具輸出）無法結束 transcript，也無法冒充指示；它只能修改提供給它看的、該 agent 的 skill；它建立或修改的內容會先成為提案，等對話的使用者本人（限於他能使用的 agent）、該 team 的 admin，或系統管理員在 Panel 的 Skills 頁或透過 `skill/proposal/{list,approve,reject}` 核准後才生效。核准時，若 skill 在提案之後已被修改，會拒絕套用。提案存下前會先遮罩，未決定的提案在 30 天後過期（`AGENT_MEMORY_SKILL_PROPOSAL_TTL_DAYS`，新增）。`skill.extraction.requireApproval: false`（`AGENT_MEMORY_SKILL_REQUIRE_APPROVAL`，新增）會像以前一樣直接套用（[UPGRADE](docs/UPGRADE.zh-tw.md#萃取出的-skill-要先核准)）。
- **每個部門由自己的使用者管理；伺服器 API key 只給營運者。** `meta/*`、`pipeline/*`、`stats/overview` 與 `memory-generation-log/*` 需要該部門使用者的 user key，並依該使用者的角色判斷；`memory-prompt/*`、`knowledge/*` 與 `chat-memory/clear` 需要系統管理員的 user key。伺服器 API key 不再能開啟這些路由，只能用於 `internal/*` 與 `department/destroy`，以及新增的 `internal/pipeline/dead-letter/{list,redrive}`、`internal/memory-generation-log/{list,summary}`（`memory-server dead-letters`／`generations` 現在改用它們）。Panel 不再需要也不再送出 `CORE_API_KEY`（仍有設定時會記一則警告），`PANEL_REQUIRE_USER_KEY=false` 也不再能匿名存取，因為 Core 會要求 user key。**只帶伺服器 API key 呼叫這些路由的腳本會得到 401，需改用系統管理員的 user key**（[UPGRADE](docs/UPGRADE.zh-tw.md#伺服器-api-key-不再能開啟部門路由)、[ARCHITECTURE](docs/ARCHITECTURE.zh-tw.md#三個信任區)）。
- **蒸餾用的 CLI 不再繼承 memory-core 的環境變數。** 它只拿到基本變數（`PATH`、`HOME`、語系、憑證、proxy）、正在使用的 CLI 自己的設定（`ANTHROPIC_*`／`CLAUDE_*`、`OPENAI_*`／`CODEX_*`、`GEMINI_*`／`GOOGLE_*`）、`llm.cli.env`，以及 `llm.cli.passEnv`（`AGENT_MEMORY_LLM_CLI_PASS_ENV`，新增）列出的變數。`AGENT_MEMORY_*`（API key、資料庫與 Redis 的 URL、LLM 與 embedding 的 key）永遠不會傳給它；`passEnv` 的項目若會比對到它們，memory-core 啟動時就會停止（[DISTILLATION §6](docs/ops/DISTILLATION.zh-tw.md#cli-繼承哪些環境變數)）。`llm.cli.env` 也不能設定 `AGENT_MEMORY_*`。若 CLI 依賴容器裡的其他變數，請把它列進去，特別是透過雲端供應商（Bedrock、Vertex AI、Azure）或經由 wrapper script 執行的 CLI（[UPGRADE](docs/UPGRADE.zh-tw.md#蒸餾用的-cli-不再繼承-memory-core-的環境變數)）。
- **部門只由 `init-admin` 建立。** 對不存在的部門發出請求，不會再建立它的儲存空間（SQLite 資料庫或 Postgres schema）。資料面與部門路由的回應和 user key 錯誤時相同，`meta/auth/verify` 回 `valid: false`，營運者路由回 404 `unknown_department`（[COLLABORATION §13](docs/COLLABORATION.zh-tw.md#13-部門與儲存)）。安裝從未建立管理者時，`upgrade.sh` 和以前一樣會自動建立。**若部署會對從未初始化的部門發請求，升級後必須先對它執行 `init-admin`**（[UPGRADE](docs/UPGRADE.zh-tw.md#部門只由-init-admin-建立)）。儲存層只開啟這樣建立的部門，背景工作也建不出部門：在 `department/destroy` 之前排入的 pipeline job，現在會被丟棄，不會再把部門重建成空的；`department/destroy` 也會刪除該部門的 dead letter；多台機器時，一台刪除的部門在其他台立刻就是不存在。
- **召回輸出會引用儲存的內容，而不是原樣嵌入。** 各整合注入的 persona、scene 導覽和召回的記憶裡，可能開始一個標籤的 `<` 會寫成 `&lt;`，中間夾了空白或不可見格式字元也一樣（`x<1` 維持原樣），每個區塊也標明它是資料、不是指令。透過 API 新建的 scene 路徑限定為以 `/` 分隔、由字母、數字、`-`、`_`、`.` 組成的片段，summary 最多 500 字元；已經存在的 scene 保留原名稱，仍然可以更新（[ARCHITECTURE 第 5 節](docs/ARCHITECTURE.zh-tw.md#5-召回)）。
- **私人 agent 只屬於它的 owner。** 記憶 API 現在會套用 agent 的可見性：私人 agent（它的記憶、場景、persona 和 skills，包括整個 team 範圍的 skill 列表與搜尋）只有 owner、team 的管理者和系統管理者能使用，已封存的 agent 一律拒絕。agent 會維持建立時的可見性（預設 `team`）：每人一個 agent 的團隊，應該用 `/v3/meta/agent/update` 把每個 agent 設成 `"visibility": "private"`（[COLLABORATION 第 9 節](docs/COLLABORATION.zh-tw.md#9-記憶請求的授權)）。
- **memory-core 在每條寫入路徑上遮罩機密**，儲存或蒸餾前就先處理：對話、原始 session 紀錄、skill 緩衝與 skill、atom（含 L1 模型寫出的內容），以及場景和 persona。之前只有 `amem` client 會遮罩，直接寫進 HTTP API 的文字會原樣存下，模型也可能把機密帶進 L1 記憶（#57）。
- 遮罩規則能抓到用自然語言提到並寫出值的密碼，涵蓋英文和亞洲主要語言（中文、日文、韓文、越南文、泰文、印尼／馬來文、印地文、阿拉伯文），以及西班牙文、葡萄牙文、法文、德文、土耳其文和俄文，常見縮寫也包含在內（`pw`、`pwd`、`pass`、`mk`、`비번`、`パス` 等）；`token`、`pass` 這類有日常意思的字，值前面要有 `:`、`=` 或「是」才算。另外也會抓長得像密碼的字串（8 個以上字元，混合字母、數字和符號），網址、路徑、email、query string、程式運算子和模型名稱除外。原始 session 紀錄裡，機密類欄位（`password`、`access_token` 等）的值會整個遮掉；文字格式的 skill 檔案也會和 skill 一樣遮罩。L1 和 skill 的 prompt 也要求模型不得記錄任何憑證的值。`amem` 使用同一套規則（#57）。
- `redaction.passwordShape`（`AGENT_MEMORY_REDACTION_PASSWORD_SHAPE`）可關閉形狀規則；`redaction.patterns`（`AGENT_MEMORY_REDACTION_PATTERNS`，JSON 陣列）可加入身分證字號、電話等規則。規則無法編譯時，memory-core 會在啟動時停止（[DISTILLATION §7](docs/ops/DISTILLATION.zh-tw.md#7-機密與遮罩)）。升級前已存入的資料不會被改寫。

### 新增
- **部門遮罩規則**（#57）：部門的系統管理者可以在內建和部署設定的規則之外加入自己的規則，在 Panel（Metadata → Redaction）或透過 `/v3/meta/redaction-rule/{list,create,update,disable,enable,test}`。修改後下一次寫入就生效（其他 replica 30 秒內）；規則只會停用、不會刪除，並記錄誰改了什麼。`test` 可以在存檔前先用範例文字預覽。範圍明顯過寬的 pattern（單獨一個字元就會匹配，或匹配日常文字 20% 以上）會被拒絕，因為它會把之後所有寫入都遮掉。規則不會動到已經存在、且 kind 已知的 `[REDACTED:…]` 標記，`amem` 也一樣；長得像標記的文字（`[REDACTED:hunter2xyz99]`）會照常被遮。`redaction.patterns` 的 kind 必須是 1-40 個 a-z、0-9 和 `-` 組成的字元，否則 memory-core 不會啟動。Schema migration 5 新增這個表；之後要退回較早的版本必須還原備份（[DISTILLATION §7](docs/ops/DISTILLATION.zh-tw.md#7-機密與遮罩)）。
- 沒有向量就存進去的 atom（embedding 呼叫失敗或逾時）之後會補上：memory-core 每 10 分鐘重試一次（`memory.embedding.backfillIntervalSecs`），`memory-server reembed` 可以立刻執行。atom 本身不會被改寫，所以 L2 不會把它當成新的（[DISTILLATION §8](docs/ops/DISTILLATION.zh-tw.md#8-embedding-與負載)，#57）。
- Schema migration 4 新增一個「沒有向量的 atom」的索引，作為補向量的佇列。memory-core 啟動時（或 `memory-server migrate`）會執行；之後要退回 0.4.1 必須還原備份，因為 0.4.1 會拒絕 schema 版本較新的資料。Postgres 會在升級後每個部門第一次被使用時建立這個索引；`atoms` 很大時，第一個請求要等它建完（[SCHEMA-MIGRATIONS](docs/ops/SCHEMA-MIGRATIONS.zh-tw.md)）。
- 數值型的 `AGENT_MEMORY_*` 環境變數無法解析時，memory-core 現在會在啟動時停止並指出是哪一個，不再默默忽略；`check-config`（以及 `upgrade.sh`）會在替換任何東西之前就回報。
- `memory-server` 的維運指令（`reembed`、`generations`、`dead-letters`、`migrate`、`check-config` 等）把 log 寫到 stderr，stdout（`--json`）可以直接給程式解析；server 本身的 log 仍寫到 stdout。
- embedding 逾時、同時進行的 embedding 請求數（`memory.embedding.maxConcurrency`，預設 4，新增）、worker 並行數和 CLI 模型並行數，都可以用環境變數設定：`AGENT_MEMORY_EMBEDDING_TIMEOUT_MS`、`AGENT_MEMORY_EMBEDDING_MAX_CONCURRENCY`、`AGENT_MEMORY_WORKER_CONCURRENCY`、`AGENT_MEMORY_LLM_CLI_MAX_CONCURRENCY`（#57）。

### 變更
- CI 與測試說明改用 PostgreSQL 17（`pgvector/pgvector:pg17`）跑 Postgres 測試。
- `amem` 對 Claude Code session 只在 persona 和 scene 導覽的內容和這個 session 上次收到的不同時才送，`SessionStart`（啟動、resume、`/clear`、fork、壓縮）之後再送一次；相關記憶仍然每個 prompt 都送。在一個約 20 個 agent 的部署裡，這部分占全部注入 context 的三分之二，而且每 4 個 prompt 有 3 個是原封不動重送。預設另外每 20 個 prompt 重送一次，以涵蓋沒有 `SessionStart` 的壓縮或 `/rewind`；`AMEM_SYSTEM_CONTEXT_EVERY`（或 `client.json` 的 `system_context_every`）可設成別的 `N`、`0` 只在變動時送、`1` 恢復原本行為。Claude Code plugin 為此多註冊一個 `SessionStart` hook；手寫的 `settings.json` 要自行加上 `amem hook claude-code session-start`。寫入 session 狀態的 hook 會對旁邊的 `.lock` 檔取得 `flock`，彼此不會互相覆蓋；hook 最多等一秒，拿不到就照樣寫入。Codex 和 Antigravity 不變（#39）。

### 修正
- `/v3/session/add` 又能存下回合了：授權層會把解析後的範圍寫進 body，所以每個通過授權的請求都會收到 422 `unknown field: team_id`。現在會先拿掉這些範圍欄位，再檢查回合內容，並把回合依 agent 分開存放（`sessions/<source>/<team>/<agent>/<user>/<session>/`，紀錄裡也有 `agent`）；client 自己送的 `agent`、`user`、`session` 欄位仍會被拒絕（[API](docs/API.zh-tw.md)）。

## [0.4.1] - 2026-10-23

### 新增
- memory-core 遇到由更新版本寫入的資料（SQLite `user_version`、Postgres `schema_migrations`、Redis `schema_version`）時拒絕啟動，錯誤訊息會提示改跑新版本或還原備份。這次升級後要退回 0.4.0，必須還原備份（[UPGRADE](docs/UPGRADE.zh-tw.md#升級到-041)）。

### 變更
- **場景（L2）和 persona（L3）改成只追加的版本儲存**（`scenes`、`scene_versions`、`persona_versions`），不再覆寫檔案。L2 更新、合併或 `/v3/scenario/rm` 都會新增一個版本，不會刪除任何版本。每個場景有固定的 id；合併會記錄它併入了哪些場景，並把那些場景標為退役，所以被合併或移除的場景仍可以用 `version` 讀取。關鍵欄位（名稱、摘要、熱度、版本的由來、作者）是資料表欄位，內文維持 markdown，其他屬性原樣保留。schema 遷移 3 會在啟動時匯入 `profile_files`，並保留這張表。
- persona 不再因為場景導覽改變就多一個版本：導覽改在讀取 persona 時，依目前的場景產生。
- `/v3/scenario/write` 和 `/v3/core/write` 會把呼叫的使用者記為新版本的作者。內容與最新版本完全相同的寫入不會新增版本，直接回傳最新版本（作者仍是原本的作者）。清除 chat memory 仍是唯一會刪除場景與 persona 歷史的操作。
- 場景在 `/v3/scenario/{ls,read,write}` 中有固定的 `scene_id`；`/v3/scenario/read` 接受 `scene_id`，可以讀到名稱已被其他場景用掉的舊場景。`/v3/core/read` 帶 `version` 時只回傳該版本的內文，不附場景導覽。
- L2 執行期間，使用者透過 API 寫入或移除的場景，會保留使用者的版本：L2 依舊快照規劃的操作（更新、合併，或名稱已被使用者用掉的建立）會被略過，不會蓋掉使用者的版本，也不會讓整批失敗。那些記憶仍留在 L1。

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

[未發布]: https://github.com/hsuanshao/AgentMemory/compare/v0.4.2...HEAD
[0.4.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.4.2
[0.4.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.4.1
[0.4.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.4.0
[0.3.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.2
[0.3.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.1
[0.3.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.3.0
[0.2.3]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.3
[0.2.2]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.2
[0.2.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.1
[0.2.0]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.2.0
[0.1.1]: https://github.com/hsuanshao/AgentMemory/releases/tag/v0.1.1
