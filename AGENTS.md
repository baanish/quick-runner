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
- **Test fixtures (environment-provided, not in the repo):** four dependency-free LeetCode-easy
  sample projects (Rust / Node / Python / Go), one per language `qr learn` detects, live at
  `$HOME/qr-test-projects` on the VM. They are seeded by `$HOME/.local/bin/qr-seed-test-projects`
  (idempotent), which the startup update script runs. Point QuickRunner at them with
  `export QR_PROJECT_ROOTS="$HOME/qr-test-projects"` to exercise `scan`, `go`, `run`, and `learn`
  against real projects. If they are ever missing, regenerate with `qr-seed-test-projects`.
- **Gotcha — `qr go` needs a TTY for ambiguous matches:** in a non-interactive shell, a query that
  matches multiple projects errors with `Multiple matches for '<q>'` instead of showing the picker.
  Use a unique substring (or `--print-path`) when scripting.
- **AI (`qr do`)** is the only command that makes a live model call (`qr learn` is static
  marker-based detection and needs no key). It resolves the API key from the protocol's
  well-known env var (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`), then `config.toml`, then the OS
  keychain — so exporting `OPENAI_API_KEY` is enough to authenticate. Point it at a specific
  model/endpoint with `QR_AI_MODEL` and `QR_AI_BASE_URL` (there is no `OPENAI_MODEL`/
  `OPENAI_BASE_URL` mapping — map those yourself). `qr do` prints the AI-suggested command and
  only runs it after you type `y` (default is No), so it is safe to pipe `n` for a
  non-executing smoke test. None of this is required for build, lint, tests, or scan/go/stats.
- **Gotcha — AI fallback provider:** the default config defines an `[ai.fallback]` pointing at
  `https://api.openai.com/v1`. If you set `QR_AI_BASE_URL` to a proxy/gateway (whose key is not a
  real OpenAI key) and the primary call has a transient failure/timeout, `qr` falls back to
  api.openai.com and fails with a confusing `401 Incorrect API key`. When using a custom endpoint,
  also set `QR_AI_FALLBACK_BASE_URL` (and `QR_AI_FALLBACK_MODEL`) to the same endpoint.
- **Gotcha — secrets in the Desktop terminal:** injected secret env vars are present in the agent
  shell but a freshly opened Desktop GUI terminal may not inherit them. To drive an AI demo from the
  Desktop, write the needed vars to a temp file the agent shell can produce and `source` it in the
  Desktop shell (then delete it) — do not print secret values or commit them.

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
