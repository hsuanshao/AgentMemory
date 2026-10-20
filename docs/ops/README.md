# Operations and maintenance

[繁體中文](README.zh-tw.md)

Notes for people who maintain Agent Memory itself: running the tests, cutting a release, and changing the parts of the code that need extra care. If you only run a server, the guides one level up ([UPGRADE](../UPGRADE.md), [MIGRATE-TO-POSTGRES](../MIGRATE-TO-POSTGRES.md), [UNINSTALL](../UNINSTALL.md)) are what you need.

| Document | What it covers |
| --- | --- |
| [TESTING.md](TESTING.md) | Every test layer and how to run it: Rust unit and end-to-end tests, the Postgres and Redis integration tests, the Panel, shell scripts, and a full-stack smoke test that leaves your own install alone |
| [RELEASE.md](RELEASE.md) | Version numbers and where they live, the release checklist, CI and the release workflow |
| [DISTILLATION.md](DISTILLATION.md) | Watching background distillation: run statuses, the Panel card, `memory-server generations` and `dead-letters`, common LLM failures, changing the model, redriving dead letters |
| [STORAGE-MIGRATION.md](STORAGE-MIGRATION.md) | How the SQLite → PostgreSQL copy works, and what to update when the storage schema changes |
| [SCHEMA-MIGRATIONS.md](SCHEMA-MIGRATIONS.md) | Versioned migrations of stored data between releases: where each store keeps its version, when they run, `memory-server migrate`, and how to add one |
