# AGENTS.md

Working notes for agents and contributors on QuickRunner (`qr`).

## Build / test / run

- Build: `cargo build --release` (binary at `target/release/qr`)
- Test: `cargo test`
- Install locally: `cargo install --path .`

## Deferred decisions — do not pre-emptively implement

### Stats DB schema migrations

`src/stats_db.rs` creates the `command_runs` table with `CREATE TABLE IF NOT EXISTS`
and there is intentionally **no migration scaffolding** yet — only one schema version
has ever shipped, so none is needed.

**The first time you change the `command_runs` schema** (add / rename / drop a column),
you must add migration support *in the same change*. `CREATE TABLE IF NOT EXISTS` does
nothing when the table already exists, so an existing user's database keeps the old
columns and the next `INSERT`/`SELECT` referencing a new column fails with
`no such column`, breaking every stats write. Add a `PRAGMA user_version` check on open
and run ordered `ALTER TABLE` / migration steps to upgrade older databases.

Until then, leave it as-is — don't add the scaffolding before there's a schema change
that needs it.
