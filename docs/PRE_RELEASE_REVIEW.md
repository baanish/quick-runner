# QuickRunner (`qr`) — Release Review Notes

This file is an archival companion to `CHANGELOG.md`, not a live blocker list.
The original pre-release review identified real issues, but many of the highest
severity items have already been fixed on the current branch. Public-facing docs
should treat those items as historical context, not present-day release blockers.

## Current release sources of truth

- `README.md` — shipped CLI behavior and install/config guidance
- `CHANGELOG.md` — user-visible changes and fixes in the current release line
- `cargo test`
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --locked -- -D warnings`

## Historical findings that are already fixed

The following items appeared in the original audit and are now resolved:

- `qr do` no longer has an allow-list auto-approve path; every AI-generated
  command is shown in full and gated by a single `Run this command? [y/N]`
  confirmation that defaults to No.
- The repo now ships the public release basics: `LICENSE`, `CHANGELOG`,
  `CONTRIBUTING`, `AGENTS.md`, CI, and an explicit MSRV / `rust-version`.
- Project cache, shell rc updates, and learned project profiles are written
  atomically.
- Corrupt cache/config recovery is in place for the documented recovery paths.
- Stats recording is best-effort, with SQLite tuned so stats failures do not
  flip a successful command into a failed one.
- The `qr go` multi-match picker works through the shell wrapper and restores
  the terminal correctly on exit.
- Public docs have been updated to reflect the `~/.qr/` config location, the
  `qr init` flow, the current `qr run` flag syntax, and the shipped `qr do`
  confirmation model.

## How to use this document

Use this page as a short release-review note. For historical deep-dive audit
details, rely on git history and the changelog entries that recorded the fixes.
