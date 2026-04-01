# QuickRunner PRD

## Overview

QuickRunner (`qr`) is a blazing-fast, AI-augmented CLI tool that takes natural language input and performs common developer shell actions. It prioritizes speed above all else — both in execution and interaction design.

## Goals

- **Speed**: Sub-100ms for non-AI operations, sub-500ms for AI-assisted ones
- **Minimal friction**: Single-keystroke selection, no unnecessary confirmations
- **Enterprise-friendly**: Open source (MIT), no vendor lock-in on core features
- **Portable**: Works on macOS and Linux out of the box

## Tech Stack

| Component | Choice | Rationale |
|---|---|---|
| Language | **Rust** | Speed, safety, single binary distribution, great CLI ecosystem (clap, crossterm) |
| AI Backend | **OpenAI-compatible + Anthropic-compatible** | User wires in any endpoint (FirePass, Cerebras, local, whatever) |
| Config format | TOML | Rust-native, human-readable |
| Config override | Environment variables | Every config key has a `QR_` env var equivalent for cloud shells |

### Why Rust over Zig/Go

- Richer CLI ecosystem (clap, dialoguer, indicatif, crossterm)
- Better package management (cargo vs. Zig's evolving build system)
- Easier to attract contributors (enterprise-friendly goal)
- Go would also work but Rust edges it on raw binary speed and startup time

## Command: `qr`

Confirm `qr` is not commonly aliased. Quick check: it's not a standard Unix command. Some systems alias it for QRencode but that's rare and typically `qrencode` instead.

### Single-letter shortcuts

Every subcommand has a 1-letter alias for speed:

| Full | Short | Command |
|---|---|---|
| `qr go` | `qr g` | Smart CD |
| `qr run` | `qr r` | Script runner |
| `qr alias` | `qr a` | Alias manager |
| `qr stats` | `qr s` | Stats |
| `qr scan` | `qr x` | Rescan projects |
| `qr init` | `qr i` | First-time setup |
| `qr do` | `qr d` | Natural language (v2) |

## Features

### 1. Smart CD — `qr go <project>` / `qr g <project>`

Navigate to a project directory by fuzzy name.

```
$ qr go orion
→ cd /Users/aanish/Development/orion-app
```

**How it works:**
- On setup, user configures one or more project root directories (e.g., `~/Development`, `/Volumes/Delos/Development`)
- An hourly cron job (or on-demand `qr scan`) walks those directories up to `scan_depth`
- Detection heuristic (in priority order):
  1. `.git` present → project. Extract canonical name from git remote origin URL if available, fall back to directory name
  2. No `.git` but contains a project marker (`Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, etc.) → project, use directory name
  3. Neither → not a project, skip
- Scan depth of 2 gives monorepo support for free (root repo + nested packages)
- `qr go <name>` does fuzzy matching against the cache
- If multiple matches, triggers the interactive picker (see Feature 4)

**Config example:**
```toml
[projects]
roots = ["~/Development", "/Volumes/Delos/Development"]
scan_depth = 2  # how deep to look for project dirs
```

**Edge cases:**
- Exact match → go immediately, no prompt
- Single fuzzy match → go immediately
- Multiple matches → interactive picker
- No match → "No project matching '<name>' found. Run `qr scan` to refresh."

### 2. Script Runner — `qr run <script> [mode]` / `qr r <script> [mode]`

Run scripts with three output modes:

| Mode | Command | Behavior |
|---|---|---|
| **watch** | `qr run watch <script>` | Run script, report only exit status (pass/fail) |
| **log** | `qr run log <script>` | Run script, write full output to `./qr-log-<timestamp>.log` |
| **output** | `qr run output <script>` | Run script, stream output to terminal (default passthrough) |

Default mode (bare `qr run <script>`) should be `output` — least surprising behavior.

**Details:**
- `watch`: Suppress stdout/stderr, show spinner, then ✅ exit 0 or ❌ exit N
- `log`: Suppress terminal output, write to log file, show log path on completion
- `output`: Just exec the script with inherited stdio (thin wrapper, near-zero overhead)

### 3. Alias Manager — `qr alias` / `qr a`

Manage shell aliases without manually editing dotfiles.

| Command | Behavior |
|---|---|
| `qr alias add <name> <command>` | Add alias to shell rc file |
| `qr alias list` | Show all aliases from rc file |
| `qr alias remove <name>` | Remove alias from rc file |

**`qr alias add` flow:**
1. Detect shell (zsh → `.zshrc`, bash → `.bashrc`, fish → `config.fish`)
2. Read the rc file
3. Check if alias `<name>` already exists
4. If exists → show current definition, ask "Edit? (y/n)"
5. If not → append `alias <name>='<command>'` to end of file
6. Run `source ~/.zshrc` (or equivalent) to reload

**Important:** Source-ing from a subprocess won't affect the parent shell. Options:
- Print instruction: "Run `source ~/.zshrc` or open a new terminal"
- Or use `exec $SHELL` to replace the current shell (if running interactively)
- Document this limitation clearly

### 4. Interactive Picker

When any feature has multiple options, present a numbered list (1-9) with **instant keypress selection** — no Enter key required.

```
Multiple projects match "app":
  1) orion-app
  2) quick-runner
  3) webapp-template
→ Press 1-9:
```

**Implementation:**
- Use raw terminal mode (crossterm) to capture single keypress
- Max 9 options per page; if more, paginate or filter
- ESC or q to cancel
- Visual highlight on the selected option before executing

### 5. Stats for Nerds — inline + `qr stats`

Every command prints a compact stats line on completion:

```
$ qr g orion
→ cd /Users/aanish/Development/orion-app
⚡ 12ms | no AI

$ qr do "find TODOs"
→ grep -rn "TODO" . --include="*.rs"
⚡ 342ms | 1.2k tok (in: 800 / out: 400) | FirePass | ~$0.001
```

`qr stats` shows the aggregate view:

```
$ qr stats
QuickRunner Stats
─────────────────
Total runs:        142
AI-assisted runs:   38
Tokens used:     12,450 (in: 8,200 / out: 4,250)
Total AI time:     14.2s (avg: 374ms)
Est. cost:         $0.03
Provider:          FirePass
─────────────────
```

**Storage:** Local SQLite database at `~/.config/qr/stats.db`

**Tracked per invocation:**
- Timestamp
- Command type (go, run, alias, etc.)
- Whether AI was used
- Token count (prompt + completion)
- Latency (ms)
- Provider used
- Estimated cost

## AI Integration Points

AI is used sparingly — only where it adds clear value over deterministic logic.

The AI layer supports two protocol families:
- **OpenAI-compatible** (`/v1/chat/completions`) — covers FirePass, Cerebras, Groq, local Ollama, vLLM, etc.
- **Anthropic-compatible** (`/v1/messages`) — covers Anthropic direct, AWS Bedrock, etc.

User configures the base URL + API key. No provider-specific code.

| Feature | AI Role |
|---|---|
| `qr go` | Fuzzy matching can be deterministic (no AI needed for v1) |
| `qr run` | No AI needed |
| `qr alias` | Optional: natural language → alias (e.g., `qr alias add "shortcut to rebuild docker"`) |
| Future: `qr do <natural language>` | Translate intent to shell command (the big AI feature) |

### Future: `qr do <task>`

The natural language → shell command feature. Not in v1, but the architecture should support it.

```
$ qr do "find all TODO comments in this project"
→ grep -rn "TODO" . --include="*.rs" --include="*.ts"
Execute? [Y/n]
```

## Project Structure

```
quick-runner/
├── Cargo.toml
├── LICENSE                 # MIT or Apache 2.0
├── README.md
├── src/
│   ├── main.rs            # Entry point, CLI arg parsing
│   ├── commands/
│   │   ├── mod.rs
│   │   ├── go.rs          # Smart CD
│   │   ├── run.rs         # Script runner
│   │   ├── alias.rs       # Alias manager
│   │   └── stats.rs       # Stats display
│   ├── picker.rs          # Interactive 1-9 selector
│   ├── config.rs          # TOML config loading
│   ├── scanner.rs         # Project directory scanner
│   ├── ai/
│   │   ├── mod.rs
│   │   ├── client.rs      # HTTP client for AI providers
│   │   └── providers.rs   # FirePass, Cerebras adapters
│   ├── stats_db.rs        # SQLite stats tracking
│   └── shell.rs           # Shell detection, rc file ops
├── config/
│   └── default.toml       # Default config template
└── tests/
    └── ...
```

## Config Location

`~/.config/qr/config.toml`

```toml
[general]
default_run_mode = "output"

[projects]
roots = ["~/Development"]
scan_depth = 2
scan_interval_hours = 1

[ai]
protocol = "openai"  # "openai" or "anthropic"
base_url = "https://api.fireworks.ai/inference/v1"  # any OpenAI-compatible endpoint
model = "accounts/fireworks/models/llama-v3p1-70b-instruct"
api_key_env = "QR_API_KEY"  # read from env var, never stored in config

[ai.fallback]
protocol = "openai"
base_url = "https://api.cerebras.ai/v1"
model = "llama3.1-70b"
api_key_env = "QR_FALLBACK_API_KEY"

[stats]
enabled = true
db_path = "~/.config/qr/stats.db"
```

## Environment Variable Overrides

Every config key can be overridden via `QR_` prefixed env vars. This makes QuickRunner work in cloud shells, containers, and CI where dotfiles don't exist.

| Config Key | Env Var | Example |
|---|---|---|
| `general.default_run_mode` | `QR_DEFAULT_RUN_MODE` | `QR_DEFAULT_RUN_MODE=watch` |
| `projects.roots` | `QR_PROJECT_ROOTS` | `QR_PROJECT_ROOTS=/workspace:/home/dev/projects` (colon-separated) |
| `projects.scan_depth` | `QR_SCAN_DEPTH` | `QR_SCAN_DEPTH=3` |
| `ai.protocol` | `QR_AI_PROTOCOL` | `QR_AI_PROTOCOL=anthropic` |
| `ai.base_url` | `QR_AI_BASE_URL` | `QR_AI_BASE_URL=http://localhost:11434/v1` |
| `ai.model` | `QR_AI_MODEL` | `QR_AI_MODEL=llama3.1-70b` |
| `ai.api_key_env` | (already an env var ref) | — |
| `ai.fallback.base_url` | `QR_AI_FALLBACK_BASE_URL` | — |
| `ai.fallback.model` | `QR_AI_FALLBACK_MODEL` | — |
| `stats.enabled` | `QR_STATS_ENABLED` | `QR_STATS_ENABLED=false` |
| `stats.db_path` | `QR_STATS_DB_PATH` | `QR_STATS_DB_PATH=/tmp/qr-stats.db` |

Precedence: env var > config file > built-in default.

If no config file exists and no env vars are set, `qr` still works for non-AI commands with sensible defaults.

## Installation

```bash
# From source
cargo install --git https://github.com/baanish/quick-runner

# Or build locally
git clone https://github.com/baanish/quick-runner
cd quick-runner
cargo build --release
# Binary at ./target/release/qr
```

### Shell Integration

For `qr go` to actually change the parent shell's directory, we need a shell function wrapper:

```bash
# Add to .zshrc
qr() {
  if [ "$1" = "go" ]; then
    local dir=$(command qr go "${@:2}" --print-path)
    if [ -n "$dir" ]; then
      cd "$dir"
    fi
  else
    command qr "$@"
  fi
}
```

This is critical — a subprocess can't change the parent's cwd. The `qr` binary outputs the path, and the shell function does the actual `cd`.

## v1 Scope

| Feature | In v1? |
|---|---|
| `qr go <project>` | ✅ |
| `qr run <mode> <script>` | ✅ |
| `qr alias add/list/remove` | ✅ |
| Interactive picker (1-9) | ✅ |
| `qr stats` | ✅ |
| `qr scan` (manual rescan) | ✅ |
| `qr init` (first-time setup) | ✅ |
| `qr do <natural language>` | ❌ v2 |
| AI-powered fuzzy matching | ❌ v2 (deterministic fuzzy in v1) |
| Plugin system | ❌ v2+ |

## Success Metrics

- `qr go` resolves in <50ms (cached, no AI)
- `qr run watch` adds <10ms overhead vs. raw execution
- Binary size <10MB
- Zero runtime dependencies (single static binary)
- Works on macOS arm64 + Linux x86_64 at minimum

## Open Questions

All resolved:
- **License**: MIT
- **Shell function installer**: `qr init` appends the shell wrapper by default, with `--no-shell-wrapper` to skip
- **Cron setup**: `qr init` sets up the hourly scan cron by default, with `--no-cron` to skip
