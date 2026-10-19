# 發版

[English](RELEASE.md)

## 目錄

- [1. 版本號](#1-版本號)
- [2. 版本號出現在哪些檔案](#2-版本號出現在哪些檔案)
- [3. 發版檢查清單](#3-發版檢查清單)
- [4. Release note](#4-release-note)
- [5. 自動化（規劃中）](#5-自動化規劃中)

## 1. 版本號

版本以 `vMAJOR.MINOR.PATCH` 的 tag 標在 `main` 上（`v0.1.0`、`v0.1.1`）。目前還沒有成文的規則決定要升哪一段。要注意的是，`v0.1.1` 就已經改變了 client 的驗證方式、需要執行升級步驟，所以光看版本號無法判斷維運者是否需要動手，要看 release note 和 UPGRADE 指南。

每個元件都帶著同一個發版號。`memory-server` 在 `/health` 的 `version` 回報它，`amem --version` 會印出它，`amem` 的 MCP server 也會宣告它，這些都來自 Cargo workspace 的版本；Panel 和外掛有各自的 manifest，必須寫成同一個版本（見第 2 節）。

## 2. 版本號出現在哪些檔案

| 檔案 | 欄位 | 說明 |
| --- | --- | --- |
| `Cargo.toml` | `[workspace.package] version` | 所有 crate 都繼承它（`/health`、`amem --version`）。改完之後跑一次 `cargo build`，讓 `Cargo.lock` 跟著更新 |
| `clients/claude-code/.claude-plugin/plugin.json` | `version` | |
| `clients/codex/.codex-plugin/plugin.json` | `version` | |
| `panel/package.json`、`panel/server/package.json`、`panel/web/package.json` | `version` | 改完之後執行 `cd panel && npm install --package-lock-only`，讓 `package-lock.json` 跟著更新 |
| `docs/UPGRADE*.md`、`docs/REMOTE-CLIENTS*.md` | `git checkout vX`、`cargo install … --tag vX` | 只改文件刻意指定某個版本的地方 |

不跟著發版號走的：

- `clients/hermes/agentmemory/plugin.yaml` 有自己的版本號（目前是 `2.0.0`）。
- `clients/antigravity/plugin.json` 和 `.claude-plugin/marketplace.json` 沒有版本欄位。
- `upgrade.sh` 和程式碼裡「memories written before 0.1.1」這類文字，指的是改變資料模型的那一版，維持原樣。

找出要改的地方：`git grep -n '<舊版本號>' -- ':!Cargo.lock' ':!*.md'`，再逐一檢查每個結果。

## 3. 發版檢查清單

1. 從最新的 `main` **開分支**（先 `git pull`；貢獻者會在 GitHub 上合併 PR）。
2. **更新**[第 2 節](#2-版本號出現在哪些檔案)列出的每個檔案。
3. **跑完** [TESTING.zh-tw.md](TESTING.zh-tw.md) 的每一層測試：格式、clippy、含 Postgres 和 Redis 的完整測試、Panel、shellcheck。如果這次發版改了 `install.sh`、`upgrade.sh`、`migrate-to-postgres.sh` 或 Compose 檔，也要跑整套 stack 煙霧測試。
4. **檢查可能受影響的文件**：README（兩種語言）、需要維運者動手時的 UPGRADE 指南、端點有變動時的 API.md。所有給使用者看的文件都有英文和繁體中文版，兩邊都要改。
5. 推送之前**掃描金鑰和私人資訊**：`git grep -nE 'sk-(ant|proj|mem)-[A-Za-z0-9_-]{16}|ghp_[A-Za-z0-9]{20}|BEGIN [A-Z ]*PRIVATE KEY'` 應該找不到任何東西，搜尋內部主機名稱或家目錄路徑也一樣。
6. **開 PR**，經過 review 後合併。
7. 在 `main` 的合併 commit 上**打 tag** 並推送：

   ```bash
   git switch main && git pull
   git tag -a vX.Y.Z -m vX.Y.Z
   git push origin vX.Y.Z
   ```

8. 為這個 tag **發布 GitHub release**，附上 release note（見下一節）。

## 4. Release note

建議的結構：

- 先寫維運者必須做什麼（如果有的話，例如升級步驟、client 的修改），再寫新功能，最後是修正。
- 連到指南，不要重複指南的內容。
- 用 GitHub 帳號和 PR 編號標註貢獻者。

## 5. 自動化（規劃中）

目前還沒有 `.github/workflows`。規劃如下：

**`ci.yml`**：每個 pull request 和推到 `main` 時執行：

| Job | 執行內容 |
| --- | --- |
| fmt | `cargo fmt --all -- --check` |
| clippy | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| test | `cargo test --workspace --locked`（SQLite） |
| test-services | 同上，加上 `pgvector/pgvector:pg16` 和 `redis:8.4.0` service 容器與 `AGENT_MEMORY_TEST_*_URL` 環境變數 |
| msrv | 用 Rust 1.88 執行 `cargo check --workspace --locked` |
| panel | Node 22：`npm ci`、typecheck、test、build |
| scripts | 對每支腳本跑 shellcheck |
| consistency | [第 2 節](#2-版本號出現在哪些檔案)的每個檔案版本一致；以及第 3 節步驟 5 的金鑰掃描 |
| docker | build 兩個映像但不推送（core 用 `INSTALL_CLAUDE/ANTIGRAVITY/CODEX=false` 加快速度） |

**`release.yml`**：推送 `v*.*.*` tag 時執行：

1. 確認 tag 等於 Cargo workspace 的版本，再跑一次 CI。
2. 建置 Linux x86_64/arm64（musl）和 macOS arm64/x86_64 的 `amem` 執行檔，附上 `SHA256SUMS`，讓 client 機器不再需要 Rust 工具鏈。
3. 建置多架構（amd64/arm64）映像並推到 GitHub Container Registry，名稱為 `agentmemory-core` 和 `agentmemory-panel`，標上版本號、`MAJOR.MINOR` 和 `latest`。
4. 用這些檔案建立 **draft** GitHub release，由維護者補上 release note 後發布。

連帶的後續工作：Compose 和安裝腳本改成優先拉取已發布的映像（保留 `build:` 給本機修改用），並在 REMOTE-CLIENTS 補上下載 `amem` 的方式。

shellcheck job 要設為必過之前，必須先修掉 [TESTING.zh-tw.md §4](TESTING.zh-tw.md#4-shell-腳本) 列出的警告。
