<!-- English or 中文. Title: Conventional Commits, e.g. "fix(amem): …"; it becomes the squash commit. -->

## Why

<!-- The problem, and the issue it closes if any (Closes #…). -->

## Change

<!-- What changed and why this way. Note anything users or operators must do. -->

## Tests

<!-- What you ran and the result. -->

- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`
- [ ] Postgres/Redis integration tests, if storage, coordination or SQL changed ([TESTING.md §2](../blob/main/docs/ops/TESTING.md#2-postgres-and-redis-integration-tests))
- [ ] Panel typecheck, tests and build, if `panel/` changed
- [ ] Full-stack smoke test, if `install.sh`, `upgrade.sh`, `migrate-to-postgres.sh` or the Compose file changed

## Checklist

- [ ] Docs updated in **both** English and Traditional Chinese (`*.zh-tw.md`), where user-facing behaviour changed
- [ ] Stored-data changes come with a versioned migration ([SCHEMA-MIGRATIONS.md](../blob/main/docs/ops/SCHEMA-MIGRATIONS.md))
- [ ] Renamed settings, headers or fields are rejected with their new name, not silently ignored
- [ ] No real or realistic-looking secrets; the distillation LLM gets no tool permissions
