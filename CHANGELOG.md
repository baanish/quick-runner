# Changelog

All notable changes to QuickRunner are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- `qr cost [--refresh]` — fetch a slim AI token-price snapshot from models.dev
  and show the resolved price for the configured model.
- `qr do` shows a real estimated cost (e.g. `~$0.0029`) or `cost n/a` instead of
  a hardcoded `$0.000`, priced from an optional `[ai].cost` override or the
  models.dev snapshot (the maker is taken from the model-slug namespace, with a
  median fallback); `qr init` fetches the snapshot best-effort (`--no-prices` to
  skip).
- `qr doctor` — reports the health and location of `config.toml` and the project
  cache, working even when the config is missing or invalid.
- Repo: MIT `LICENSE`, `CHANGELOG`, `CONTRIBUTING`, `AGENTS.md`, an MSRV
  (`rust-version = 1.85`), and CI (build + test on Linux and macOS, plus
  `fmt --check` and `clippy -D warnings`).

### Changed
- **Breaking:** `qr run` takes the mode as a flag (`--watch` / `--log` /
  `--output`) instead of a positional token, so a script whose command starts
  with `watch`/`log`/`output` is no longer mis-parsed.
- README and PRD updated to match shipped behavior (platform-specific config
  paths, the now-implemented `qr do`/`qr learn`, key storage, cron prompt).

### Fixed
- Project cache and shell rc files are written atomically (temp file + rename,
  following symlinks, preserving permissions).
- `qr go` recovers from a corrupt project cache; a corrupt `config.toml` no
  longer bricks `qr config` / `qr doctor` / `qr learn`.
- Stats recording is best-effort with a SQLite busy timeout, so a stats failure
  never turns a successful command into a failure.
- The multi-match picker works through the shell wrapper (drawn on stderr) and
  always restores the terminal on exit.
- Alias commands and the cron binary path are shell-escaped; `qr go` fuzzy
  matching is case-insensitive; a trailing-slash git remote no longer yields an
  empty project name; signal-killed children report `128 + signal` instead of a
  flat exit 1.
- `cargo test` no longer overwrites the developer's real project cache.

### Security
- `qr do` never auto-runs an AI-generated command: every command needs an
  explicit `y` (default no), commands using shell features (pipes, redirection,
  multiple commands) get an extra warning, and `rm`/`find` were removed from the
  default auto-approve list.
- The AI key can be stored in the OS keychain (`qr init` opt-in) instead of
  `config.toml`; keys resolve from env var → config → keychain.

## [0.1.0]

Initial implementation: `qr go`, `qr run`, `qr alias`, `qr stats`, `qr scan`,
`qr init`, `qr do`, and `qr learn`.
