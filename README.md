# QuickRunner

QuickRunner (`qr`) is a fast Rust CLI for common developer shell workflows: jumping to projects, running scripts, managing aliases, scanning project roots, tracking lightweight command stats, and an AI router (`qr do`) that turns natural language into a shell command or a hand-off to a coding agent. Every config key can be overridden via `QR_` environment variables.

## Features

- `qr go <project>` / `qr g`: fuzzy project lookup backed by a cached scanner (interactive picker on multiple matches)
- `qr run [watch|log|output] <script>` / `qr r`: script runner with watch, log, and passthrough modes
- `qr alias add|list|remove` / `qr a`: shell alias management
- `qr stats` / `qr s`: aggregated command stats from a local SQLite database
- `qr scan` / `qr x`: manual project rescan
- `qr do <task>` / `qr d`: natural language → a shell command (run only after explicit confirmation) or a suggested coding-agent hand-off
- `qr learn` / `qr l`: profile the current project (language, package manager, scripts) into `./.qr/profile.json`
- `qr config` / `qr c`: open `config.toml` in your editor (`qr config path` prints its location)
- `qr doctor`: report the health and location of the config and project cache
- `qr init` / `qr i`: creates config, installs the shell wrapper, optionally stores your AI key in the OS keychain, prompts to install an hourly rescan cron (default no), and runs an initial scan

## Install

```bash
cargo install --path .   # or: cargo build --release  (binary at target/release/qr)
qr init                  # config + shell wrapper + initial scan
exec $SHELL              # reload so the `qr go` wrapper takes effect
```

`qr init` appends a wrapper function to your shell rc file so `qr go` can change the parent shell's directory — a child process can't do that on its own. The wrapper calls `qr go --print-path` and runs the `cd` in your shell.

## Config

Run `qr config path` to print the exact location. It is platform-specific:

- macOS: `~/Library/Application Support/qr/config.toml`
- Linux: `~/.config/qr/config.toml` (or `$XDG_CONFIG_HOME/qr/`)

Defaults come from [`config/default.toml`](config/default.toml). Environment variables (`QR_*`) override every config key — e.g. `QR_PROJECT_ROOTS` (colon-separated), `QR_SCAN_DEPTH`, `QR_AI_MODEL`, `QR_STATS_ENABLED`; see `config/default.toml` for the full list.

### AI key

`qr do` / `qr learn` need an API key. It is resolved in this order: a custom env var (`api_key_env`), the protocol's well-known env var (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`), the `api_key` in `config.toml`, then the OS keychain. `qr init` offers to store the key in the OS keychain (recommended) so it stays out of `config.toml`.

## Notes

- The stats line is printed on `stderr`, so `qr go --print-path` keeps stdout clean for the shell wrapper's `cd`.
- `qr do` never runs a command on a bare Enter: you must explicitly type `y`, and commands using shell features (pipes, redirection, multiple commands) get an extra warning.
- Recording stats writes to SQLite on each command (best-effort — a failure never fails the command). Disable with `stats.enabled = false`.
