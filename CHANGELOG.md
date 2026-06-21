# Changelog

All notable changes to QuickRunner are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Security
- `qr do` never auto-runs an AI-generated command: every command requires an
  explicit `y` (default no), and commands using shell features (pipes,
  redirection, multiple commands) get an extra warning. Removed `rm` and `find`
  from the default auto-approve list.
- `qr init` can store the API key in the OS keychain instead of `config.toml`;
  keys are resolved from env var → config → keychain.

### Added
- `qr doctor` — reports the health and location of `config.toml` and the project
  cache, and works even when the config is missing or invalid.
- MIT `LICENSE` and CI (build + test on Linux and macOS).

### Changed / Fixed
- Project cache and shell rc files are written atomically (temp file + rename,
  following symlinks, preserving permissions).
- `qr go` recovers from a corrupt project cache instead of hard-failing.
- A corrupt `config.toml` no longer bricks `qr config` / `qr doctor` / `qr learn`.
- Stats recording is best-effort with a SQLite busy timeout, so a stats failure
  never turns a successful command into a failure.
- The multi-match picker works through the shell wrapper (drawn on stderr, gated
  on stdin/stderr TTYs) and always restores the terminal on exit.

## [0.1.0]

Initial implementation: `qr go`, `qr run`, `qr alias`, `qr stats`, `qr scan`,
`qr init`, `qr do`, and `qr learn`.
