# Changelog

All notable changes to QuickRunner are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed
- Bare `qr go` / `qr g` now opens a lightweight live-filter picker for cached projects while `qr go <project>` keeps the existing direct lookup behavior.

## [0.1.0] - 2026-07-02

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
- **Breaking:** global config, cache, stats DB, and price table now live under
  `~/.qr/` instead of the platform config directory (`~/.config/qr/` on Linux,
  `~/Library/Application Support/qr/` on macOS). Existing installs are migrated
  automatically on first run.
- **Breaking:** `qr run` takes the mode as a flag (`--watch` / `--log` /
  `--output`) instead of a positional token, so a script whose command starts
  with `watch`/`log`/`output` is no longer mis-parsed.
- **Breaking:** `qr do` uses a single `Run this command? [y/N]` confirmation
  (default No) for every AI-generated command. The `[do].auto_approve` command
  allow-list and the tiered "shell features" / "not in allowlist" warnings were
  removed — an allow-listed program name doesn't bound what it does, so the
  boundary is the default-No prompt plus the full command preview. An existing
  `auto_approve` setting is ignored.
- `qr learn` honors an `entry_points` override in `.qr.toml` (previously a
  silent no-op) and writes `.qr/profile.json` atomically.
- Only the current user's `~` is expanded in configured paths; `~user` is left
  literal (documented on `expand_path`).
- README and PRD updated to match shipped behavior (platform-specific config
  paths, the now-implemented `qr do`/`qr learn`, key storage, cron prompt, and
  the single-prompt `qr do` confirmation).

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
- `qr alias add` keeps a quoted command intact through `qr alias list` (the
  parser decodes the shell quoting instead of trimming it), and a fish alias
  whose command contains `=` is parsed correctly.
- `qr init` fails with a clear error on a closed/empty stdin instead of looping
  forever.
- AI price snapshot: a model with a negative or non-finite cost is skipped, and
  the median can no longer overflow to infinity (which produced an unloadable
  price table).
- Git remote name parsing accepts `url=` without spaces, strips a
  `?query`/`#fragment` suffix, and no longer attributes a non-origin section's
  URL to origin; a trailing-slash `go.mod` module path no longer yields an empty
  project name.
- `atomic::write` uses a per-writer temp filename, so concurrent writers to the
  same path can't clobber each other, and it writes *through* a dangling symlink
  to create its target (matching `fs::write`) instead of replacing the symlink
  with a regular file — a dotfiles-managed rc symlink survives a temporarily
  missing target.
- Oversized token/latency counts saturate instead of wrapping negative, and the
  `qr stats` aggregates can't overflow.
- The AI provider response body is read through a size cap, and token-usage
  counts are parsed robustly (integer or float, clamped).

### Security
- `qr alias add` validates the alias name and rejects newlines in the command,
  so a crafted name (e.g. `x; reboot #`) or an embedded newline can no longer
  inject a command into your shell rc file.
- `qr do` never auto-runs an AI-generated command: every command is shown in
  full and runs only after an explicit `y` to a single prompt that defaults to
  No. There is no command allow-list — an allow-listed program name does not
  bound what it does.
- The AI key can be stored in the OS keychain (`qr init` opt-in) instead of
  `config.toml`; keys resolve from env var → config → keychain.

Initial public release.
