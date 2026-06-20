# QuickRunner

QuickRunner (`qr`) is a fast Rust CLI for common developer shell workflows: jumping to projects, running scripts, managing aliases, scanning project roots, and tracking lightweight command stats. The v1 implementation follows the PRD in this repo, including shell wrapper setup, cron-based rescans, config overrides via `QR_` environment variables, and an architecture-only AI layer for future `qr do` support.

## Features

- `qr go <project>` / `qr g <project>`: fuzzy project lookup backed by a cached scanner
- `qr run [watch|log|output] <script>` / `qr r ...`: script runner with watch, log, and passthrough modes
- `qr alias add|list|remove` / `qr a ...`: shell alias management
- `qr stats` / `qr s`: aggregated command stats from a local SQLite database
- `qr scan` / `qr x`: manual project rescan
- `qr init` / `qr i`: creates config, installs the shell wrapper, prompts to install an hourly rescan cron (default no), and runs an initial scan

## Install

```bash
cargo build --release
./target/release/qr --help
```

## Config

QuickRunner reads `~/.config/qr/config.toml` and falls back to built-in defaults from [`config/default.toml`](config/default.toml).

Environment variables override every config key:

- `QR_DEFAULT_RUN_MODE`
- `QR_PROJECT_ROOTS` as colon-separated paths
- `QR_SCAN_DEPTH`
- `QR_SCAN_INTERVAL_HOURS`
- `QR_AI_PROTOCOL`
- `QR_AI_BASE_URL`
- `QR_AI_MODEL`
- `QR_AI_API_KEY_ENV`
- `QR_AI_FALLBACK_PROTOCOL`
- `QR_AI_FALLBACK_BASE_URL`
- `QR_AI_FALLBACK_MODEL`
- `QR_AI_FALLBACK_API_KEY_ENV`
- `QR_STATS_ENABLED`
- `QR_STATS_DB_PATH`

## Shell Integration

`qr go` cannot change the parent shell directory on its own, so `qr init` appends a wrapper function to your shell rc file. The wrapper calls `qr go --print-path` and performs the `cd` in the parent shell.

## Notes

- Stats are printed after each command on `stderr` so `--print-path` stays scriptable.
- The AI client and protocol abstraction are present for v1, but `qr do` intentionally returns a v2 message.
