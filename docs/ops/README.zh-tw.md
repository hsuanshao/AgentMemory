# 維運與維護

[English](README.md)

給 Agent Memory 本身維護者的筆記：怎麼跑測試、怎麼發版，以及修改哪些程式碼時要特別小心。如果你只是架設伺服器，需要的是上一層的指南（[UPGRADE](../UPGRADE.zh-tw.md)、[MIGRATE-TO-POSTGRES](../MIGRATE-TO-POSTGRES.zh-tw.md)、[UNINSTALL](../UNINSTALL.zh-tw.md)）。

| 文件 | 內容 |
| --- | --- |
| [TESTING.zh-tw.md](TESTING.zh-tw.md) | 每一層測試與執行方式：Rust 單元與端到端測試、Postgres 和 Redis 整合測試、Panel、shell 腳本，以及不影響你自己安裝環境的整套 stack 煙霧測試 |
| [RELEASE.zh-tw.md](RELEASE.zh-tw.md) | 版本號與它出現在哪些檔案、發版檢查清單，CI 與自動發版 |
| [DISTILLATION.zh-tw.md](DISTILLATION.zh-tw.md) | 監看背景蒸餾：執行狀態、Panel 卡片、`memory-server generations` 與 `dead-letters`、常見的 LLM 失敗原因、更換模型、重跑 dead letter |
| [STORAGE-MIGRATION.zh-tw.md](STORAGE-MIGRATION.zh-tw.md) | SQLite → PostgreSQL 搬移的運作方式，以及儲存 schema 改變時要更新哪些地方 |
| [TLS.zh-tw.md](TLS.zh-tw.md) | 讓其他機器上的 client 走 HTTPS：Compose 的 `tls` profile（Caddy）、信任它的 CA、改用其他反向代理 |
| [SCHEMA-MIGRATIONS.zh-tw.md](SCHEMA-MIGRATIONS.zh-tw.md) | 版本之間儲存資料的遷移：各儲存位置的版本記在哪裡、何時執行、`memory-server migrate`，以及如何新增遷移 |
