# QuickRunner

QuickRunner (`qr`) is a fast Rust CLI for common developer shell workflows: jumping to projects, running scripts, managing aliases, scanning project roots, tracking lightweight command stats, and an AI router (`qr do`) that turns natural language into a shell command or a hand-off to a coding agent.

## Features

- `qr go <project>` / `qr g`: fuzzy project lookup backed by a cached scanner (interactive picker on multiple matches)
- `qr run [--watch|--log|--output] <script>` / `qr r`: script runner with watch, log, and passthrough modes
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
cargo install quick-runner
qr init                  # config + shell wrapper + initial scan
exec $SHELL              # reload so the `qr go` wrapper takes effect
```

From a source checkout:

```bash
cargo install --path .   # or: cargo build --release  (binary at target/release/qr)
```

`qr init` appends a wrapper function to your shell rc file so `qr go` can change the parent shell's directory — a child process can't do that on its own. The wrapper calls `qr go --print-path` and runs the `cd` in your shell.

## Config

Run `qr config path` to print the exact location:

- `~/.qr/config.toml` on all platforms

On first run after upgrading, QuickRunner automatically migrates an existing config from the legacy location (`~/.config/qr/` on Linux, `~/Library/Application Support/qr/` on macOS) into `~/.qr/`. Set `QR_CONFIG_DIR` to override the directory (used in tests and CI).

Defaults come from [`config/default.toml`](config/default.toml). Common runtime settings also have `QR_*` environment overrides for automation and CI — e.g. `QR_PROJECT_ROOTS` (colon-separated), `QR_SCAN_DEPTH`, `QR_AI_MODEL`, and `QR_STATS_ENABLED`. See [`src/config.rs`](src/config.rs) for the supported override list.

### AI key

`qr do` needs an API key. It is resolved in this order: a custom env var (`api_key_env`), the protocol's well-known env var (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`), the `api_key` in `config.toml`, then the OS keychain. `qr init` offers to store the key in the OS keychain (recommended) so it stays out of `config.toml`. `qr learn` is local and does not make a live model call.

## Notes

- The stats line is printed on `stderr`, so `qr go --print-path` keeps stdout clean for the shell wrapper's `cd`.
- `qr do` always previews the full AI-generated command and runs it only after `Run this command? [y/N]`; bare Enter is No.
- Recording stats writes to SQLite on each command (best-effort — a failure never fails the command). Disable with `stats.enabled = false`.
