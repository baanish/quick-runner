# AGENTS.md

Working notes for agents and contributors on QuickRunner (`qr`).

## Build / test / run

- Build: `cargo build --release` (binary at `target/release/qr`)
- Test: `cargo test`
- Install locally: `cargo install --path .`

## Cursor Cloud specific instructions

- **Toolchain:** The crate is edition 2024 (`rust-version = "1.85"`). The base VM image ships an
  older default (`rustc 1.83`), which cannot build this crate. The startup update script installs
  and defaults the `stable` toolchain (`rustup default stable`) with `rustfmt` + `clippy`, so a
  fresh shell already has a working `cargo`. If you ever see edition-2024 build errors, run
  `rustc --version` and `rustup default stable`.
- **Lint (matches `.github/workflows/ci.yml`):** `cargo fmt --all -- --check` and
  `cargo clippy --all-targets --locked -- -D warnings`.
- **No services to run.** `qr` is a fully self-contained local CLI — no DB server, web server, or
  network dependency is required. Persistence is a local SQLite file (bundled `rusqlite`) plus JSON
  caches; `cargo test` mocks the AI HTTP layer, so no API key is needed for the suite.
- **Isolated manual testing:** every config key is overridable via `QR_*` env vars, so you can
  exercise the tool without touching the real user config. Useful ones:
  `QR_PROJECT_ROOTS` (colon-separated), `QR_SCAN_DEPTH`, `QR_STATS_ENABLED=true`,
  `QR_STATS_DB_PATH=/tmp/…`. A quick end-to-end smoke test:
  `qr scan` → `qr go --print-path <name>` → `qr stats`.
- **Test fixtures:** `test-projects/` holds four dependency-free LeetCode-easy sample projects
  (Rust / Node / Python / Go), one per language `qr learn` detects. Point QuickRunner at them with
  `export QR_PROJECT_ROOTS="$PWD/test-projects"` (from the repo root) to exercise `scan`, `go`,
  `run`, and `learn` against real projects. See `test-projects/README.md`. Running them creates
  gitignored artifacts (`target/`, `.qr/`, etc.).
- **Gotcha — `qr go` needs a TTY for ambiguous matches:** in a non-interactive shell, a query that
  matches multiple projects errors with `Multiple matches for '<q>'` instead of showing the picker.
  Use a unique substring (or `--print-path`) when scripting.
- **AI features** (`qr do` / `qr learn`) need a real OpenAI/Anthropic-compatible key
  (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY` or the OS keychain); they are not required for build,
  lint, tests, or the core scan/go/stats flow.

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
