# 發版

[English](RELEASE.md)

## 目錄

- [1. 版本號](#1-版本號)
- [2. 版本號出現在哪些檔案](#2-版本號出現在哪些檔案)
- [3. 發版檢查清單](#3-發版檢查清單)
- [4. Release note](#4-release-note)
- [5. 自動化](#5-自動化)

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
4. **檢查可能受影響的文件**：`CHANGELOG.md` 和 `CHANGELOG.zh-tw.md`（把「未發布」的內容移到新版本下）、README（兩種語言）、需要維運者動手時的 UPGRADE 指南、端點有變動時的 API.md。所有給使用者看的文件都有英文和繁體中文版，兩邊都要改。
5. 推送之前**掃描金鑰和私人資訊**：`.github/scripts/check-consistency.sh X.Y.Z` 會一次做完步驟 2 和 5 的檢查。手動的話，`git grep -nE 'sk-(ant|proj|mem)-[A-Za-z0-9_-]{16}|ghp_[A-Za-z0-9]{20}|BEGIN [A-Z ]*PRIVATE KEY'` 應該找不到任何東西，搜尋內部主機名稱或家目錄路徑也一樣。
6. **開 PR**，經過 review 後合併。
7. 在 `main` 的合併 commit 上**打 tag** 並推送：

   ```bash
   git switch main && git pull
   git tag -a vX.Y.Z -m vX.Y.Z
   git push origin vX.Y.Z
   ```

8. **發布 GitHub release。** tag 會啟動 `release.yml`（[第 5 節](#發版)），建置所有檔案並建立 draft release；補上 release note（見下一節）後發布。

## 4. Release note

建議的結構：

- 先寫維運者必須做什麼（如果有的話，例如升級步驟、client 的修改），再寫新功能，最後是修正。
- 連到指南，不要重複指南的內容。
- 用 GitHub 帳號和 PR 編號標註貢獻者。

## 5. 自動化

### CI

[`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) 在每個 pull request 和每次推到 `main` 時執行：

| Job | 執行內容 |
| --- | --- |
| fmt | `cargo fmt --all -- --check` |
| clippy | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| test | `cargo test --workspace --locked`，搭配 `pgvector/pgvector:pg16` 和 `redis:8.4.0` service 容器與 `AGENT_MEMORY_TEST_*_URL` 環境變數，所以 SQLite、Postgres、Redis 的測試都會跑 |
| msrv | 用 Rust 1.88 執行 `cargo check --workspace --all-targets --locked` |
| panel | Node 22：`npm ci`、typecheck、test、build |
| scripts | 用 shellcheck v0.10.0 檢查每支腳本；確認 Hermes plugin 能編譯 |
| consistency | [`.github/scripts/check-consistency.sh`](../../.github/scripts/check-consistency.sh)：[第 2 節](#2-版本號出現在哪些檔案)的每個檔案版本一致，以及步驟 5 的金鑰和私人路徑掃描 |
| docker | build 兩個映像但不推送（core 用 `INSTALL_CLAUDE/ANTIGRAVITY/CODEX=false` 加快速度） |

發版前可以在本機跑一致性檢查；加上版本號參數時，還會確認每個檔案都是那個版本：`.github/scripts/check-consistency.sh 0.2.0`。

`Cargo.toml` 的 `rust-version` 改變時，msrv job 的工具鏈也要跟著改。

### 發版

推送 `vX.Y.Z` tag 時（檢查清單第 7 步）會執行 [`.github/workflows/release.yml`](../../.github/workflows/release.yml)：

| Job | 做什麼 |
| --- | --- |
| version | `check-consistency.sh X.Y.Z`：確認 tag 和[第 2 節](#2-版本號出現在哪些檔案)每個檔案的版本相同 |
| ci | 跑完整的 `ci.yml` |
| amem | 建置 `linux-x86_64`、`linux-arm64`（靜態連結，musl）、`macos-arm64`、`macos-x86_64` 的 `amem`，打包成 `amem-X.Y.Z-<platform>.tar.gz` |
| image | 在各架構的原生 runner 上建置 `agentmemory-core`（含內建的 LLM CLI）和 `agentmemory-panel` 的 amd64、arm64 映像。每個映像會先啟動測試：core 的 `/health` 要回傳這次的版本，內建的 CLI 要能執行；Panel 的 `/health` 要有回應。通過後才以 digest 推送 |
| manifest | 把兩個架構合併到 `ghcr.io/<owner>/agentmemory-core` 和 `-panel`，標上 `X.Y.Z`、`X.Y` 和 `latest` |
| release | 建立 **draft** GitHub release，附上壓縮檔和 `SHA256SUMS` |

之後：

1. 打開 draft release，寫 release note（第 4 節），決定是否標成 pre-release，然後發布。
2. 只有第一次需要：GHCR 新建的 package 預設是私有的。到 repo 的 Packages 打開每個 package（`agentmemory-core`、`agentmemory-panel`），Package settings → Change visibility → Public。

某個 job 失敗時，先在 `main` 修好，再對這個 tag 重跑 workflow。重跑會替換 release 的檔案，但保留已寫好的 release note。tag 打錯了：刪掉 draft release 和 tag（`git push origin :refs/tags/vX.Y.Z`），再重新打 tag。

想試跑整個建置流程但不發布，可以手動啟動：Actions → Release → Run workflow。它會建置所有執行檔和映像，但什麼都不推送、也不建立 release；壓縮檔會留在 workflow 的 artifacts。

尚未完成：Compose 和安裝腳本仍然在本機建置映像，還沒改成從 GHCR 拉取；REMOTE-CLIENTS 仍然用 `cargo install` 安裝 `amem`。
