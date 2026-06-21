# Contributing to QuickRunner

Thanks for your interest in contributing!

## Development

```bash
cargo build      # debug build (binary at target/debug/qr)
cargo test       # run the full test suite
cargo fmt        # format
cargo clippy     # lint
```

The minimum supported Rust version (MSRV) is **1.85** (required by edition 2024).

## Pull requests

- Keep PRs minimal and tightly scoped — one logical change per PR; don't bundle
  unrelated refactors or cosmetic changes into a fix.
- Add a test for every bug fix and feature.
- Run `cargo test` and `cargo fmt` before pushing. CI runs build + test on Linux
  and macOS.
- Write a clear description: the problem, the change, and how you verified it.

## Project layout

- `src/commands/` — one module per subcommand (`go`, `run`, `alias`, `stats`,
  `do_cmd`, `learn`, `config_cmd`, `doctor`).
- `src/config.rs` — config schema, loading, and `QR_*` env overrides.
- `src/ai/` — the provider-agnostic AI client (OpenAI- and Anthropic-compatible).
- `src/scanner.rs` — project discovery and the cache.
- `src/atomic.rs` — atomic file writes; `src/secret.rs` — OS keychain access.
- `AGENTS.md` — working notes and deferred decisions for agents/contributors.
