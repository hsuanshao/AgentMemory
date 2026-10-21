# 參與 AgentMemory 開發

[English](CONTRIBUTING.md)

感謝你的協助。AgentMemory 是給團隊協作中的 agent 使用的自架記憶服務：一台伺服器容納多個部門，部門裡有團隊，團隊裡有成員和他們的 agent（[COLLABORATION.zh-tw.md](../docs/COLLABORATION.zh-tw.md)）。不論大小的貢獻都歡迎：修一個錯字、回報問題、修 bug、加功能都可以。

參與本專案即表示你同意遵守[行為準則](CODE_OF_CONDUCT.md)。

## 目錄

- [回報問題與提出需求](#回報問題與提出需求)
- [寫程式之前](#寫程式之前)
- [開發環境](#開發環境)
- [送出 pull request 前的檢查](#送出-pull-request-前的檢查)
- [慣例](#慣例)
- [Pull request](#pull-request)
- [安全問題](#安全問題)

## 回報問題與提出需求

請使用 issue 範本。回報 bug 時請附上：
- 版本（`/health` 的 `version`，以及 client 上 `amem --version` 的輸出）；
- 使用的 storage 和 coordination backend；
- 使用的 LLM provider；
- 重現步驟。

貼上記錄前，請先移除 key 和個人資料。

安全問題請不要公開回報，改用 [SECURITY.md](SECURITY.md) 說明的方式私下回報。

## 寫程式之前

除了小修正之外，請先開一個 issue，說明問題和你打算怎麼改。這樣可以避免花時間做了不適合的改動，例如改變專案範圍或引入新的相依套件。

## 開發環境

- **Rust**：使用 stable 版本。專案的最低版本是 Rust 1.88（`rust-version`），CI 會檢查。
- **Node.js 22**：用於 Panel（`panel/`）。
- **Docker**：執行 Postgres 和 Redis 的整合測試，以及完整堆疊測試時需要。

專案結構和設計見 [ARCHITECTURE.zh-tw.md](../docs/ARCHITECTURE.zh-tw.md)。維護者的說明（測試、發版、遷移）在 [docs/ops/](../docs/ops/README.zh-tw.md)。

## 送出 pull request 前的檢查

CI 會跑以下所有檢查。請依你改動的部分執行對應的項目（詳見 [TESTING.zh-tw.md](../docs/ops/TESTING.zh-tw.md)）：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

- **改到 Postgres 或 Redis 的程式碼，或任何 SQL**：還要對真實的伺服器跑整合測試（[TESTING.zh-tw.md 第 2 節](../docs/ops/TESTING.zh-tw.md#2-postgres-與-redis-整合測試)）。沒有設定連線網址時，這些測試會略過並顯示通過。
- **Panel**：`cd panel && npm ci && npm run typecheck && npm test && npm run build`。
- **Shell 腳本**：使用 shellcheck v0.10.0（[TESTING.zh-tw.md 第 4 節](../docs/ops/TESTING.zh-tw.md#4-shell-腳本)）。
- **改到 `install.sh`、`upgrade.sh`、`migrate-to-postgres.sh` 或 Compose 檔**：要跑整套 stack 煙霧測試（[TESTING.zh-tw.md 第 5 節](../docs/ops/TESTING.zh-tw.md#5-整套-stack-煙霧測試)）。

## 慣例

- **Commit 和 PR 標題**遵循 [Conventional Commits](https://www.conventionalcommits.org/)：`feat:`、`fix:`、`docs:`、`refactor:`、`test:`、`ci:`、`chore:`，可加範圍（`feat(amem): …`）。PR 一律以 squash 合併，所以 PR 標題就是 `main` 上的 commit。
- **文件是雙語的。** 每份給使用者看的文件都有英文版和繁體中文版（`*.zh-tw.md`），請在同一個 PR 裡兩份一起更新。
- **儲存資料的變更一律透過有版本的遷移。** 不能只改 `CREATE … IF NOT EXISTS` 的 DDL，要新增遷移步驟，見 [SCHEMA-MIGRATIONS.zh-tw.md](../docs/ops/SCHEMA-MIGRATIONS.zh-tw.md)。
- **改名的設定要明確報錯。** 改名的設定、header 或 API 欄位，必須回報錯誤並指出新名稱，不能默默忽略。
- **用詞**：部門（department）、團隊（team）、成員（member）、agent。本專案不使用「租戶」（tenant）或「多租戶」（multi-tenancy）。
- **蒸餾永遠不給工具。** 蒸餾用的 LLM 讀的是不可信的對話內容，不能給它任何工具或指令權限，也不能加上略過權限檢查的參數（#37）。
- **測試裡的機密在執行時才產生。** repo 裡不能有真實的 key，也不能有看起來像真的 key；CI 會掃描。

## Pull request

- 一個 PR 只做一件事。說明為什麼要改、改了什麼、怎麼測試；PR 範本會列出要涵蓋的項目。
- CI 必須通過。維護者會 review，可能會要求修改，包括文件。
- 發版由維護者負責（[RELEASE.zh-tw.md](../docs/ops/RELEASE.zh-tw.md)），你不需要更新版號。

你貢獻的內容會以本專案的 [MIT 授權](../LICENSE)釋出。

## 安全問題

見 [SECURITY.md](SECURITY.md)。漏洞請不要開公開的 issue。
