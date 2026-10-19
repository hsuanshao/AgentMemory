# Operations and maintenance

[繁體中文](README.zh-tw.md)

Notes for people who maintain Agent Memory itself: running the tests, cutting a release, and changing the parts of the code that need extra care. If you only run a server, the guides one level up ([UPGRADE](../UPGRADE.md), [MIGRATE-TO-POSTGRES](../MIGRATE-TO-POSTGRES.md), [UNINSTALL](../UNINSTALL.md)) are what you need.

| Document | What it covers |
| --- | --- |
| [TESTING.md](TESTING.md) | Every test layer and how to run it: Rust unit and end-to-end tests, the Postgres and Redis integration tests, the Panel, shell scripts, and a full-stack smoke test that leaves your own install alone |
| [RELEASE.md](RELEASE.md) | Version numbers and where they live, the release checklist, and the planned CI and release automation |
| [STORAGE-MIGRATION.md](STORAGE-MIGRATION.md) | How the SQLite → PostgreSQL copy works, and what to update when the storage schema changes |
