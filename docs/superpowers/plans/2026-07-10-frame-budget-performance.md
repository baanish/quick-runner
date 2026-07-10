# Frame-Budget Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the measured multi-second telemetry tail and pathological scanner traversal while adding process-level performance coverage and a smaller optimized release profile.

**Architecture:** Keep the current command and project-detection semantics. Telemetry writes get a dedicated short lock-wait policy; explicit stats reads retain the existing policy. The scanner still recognizes `.git` plus all six marker files, but `WalkDir` stops descending into hidden directories and `node_modules`. Criterion gains executable-level and pathological-tree fixtures, and release binaries use thin LTO, one codegen unit, and symbol stripping without changing panic behavior.

**Tech Stack:** Rust 2024, rusqlite, walkdir, Criterion, Cargo release profiles

---

## Scope decisions

- Preserve all project markers: `Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, `requirements.txt`, and `Makefile`.
- Keep the current synchronous rescan when the project cache is missing, empty, or corrupt.
- Prune hidden descendant directories and `node_modules`; do not add broad names such as `target`, `build`, or `vendor`, which could be legitimate project directories.
- Do not add stats schema migration scaffolding or change the `command_runs` schema.
- Do not use `panic = "abort"`; release tuning must not change failure semantics.

### Task 1: Fast-fail best-effort telemetry writes

**Files:**
- Modify: `src/stats_db.rs`
- Modify: `src/main.rs`
- Test: `src/stats_db.rs`

- [ ] **Step 1: Write a failing lock-contention test**

Add a unit test that creates the schema, holds `BEGIN IMMEDIATE` on one connection, opens a telemetry connection, attempts a record, and asserts the attempt returns an error in well under the existing three-second timeout. The test must call a new `StatsDb::open_for_telemetry` API so it fails before implementation.

```rust
#[test]
fn telemetry_record_fails_fast_when_another_writer_holds_the_database() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("stats.db");
    drop(StatsDb::open(&path).unwrap());

    let lock = Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();

    let started = std::time::Instant::now();
    let result = StatsDb::open_for_telemetry(&path)
        .and_then(|db| db.record(&CommandStats::default()));

    assert!(result.is_err());
    assert!(started.elapsed() < std::time::Duration::from_millis(250));
}
```

- [ ] **Step 2: Run the test and verify RED**

Run: `cargo test stats_db::tests::telemetry_record_fails_fast_when_another_writer_holds_the_database -- --exact`

Expected: compilation fails because `open_for_telemetry` does not exist.

- [ ] **Step 3: Implement the short telemetry timeout**

Refactor `StatsDb::open` through one internal constructor. Keep the existing three-second timeout for explicit opens, and add `open_for_telemetry` with a 5 ms timeout. Do not change WAL, schema initialization, or the schema itself.

```rust
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const TELEMETRY_BUSY_TIMEOUT: Duration = Duration::from_millis(5);

pub fn open(path: &Path) -> Result<Self> {
    Self::open_with_busy_timeout(path, DEFAULT_BUSY_TIMEOUT)
}

pub fn open_for_telemetry(path: &Path) -> Result<Self> {
    Self::open_with_busy_timeout(path, TELEMETRY_BUSY_TIMEOUT)
}
```

Update `record_stats` in `src/main.rs` to use `open_for_telemetry`.

- [ ] **Step 4: Verify GREEN**

Run the focused test again, then run `cargo test stats_db::tests --lib`.

### Task 2: Prune traversal without removing project detection

**Files:**
- Modify: `src/scanner.rs`
- Test: `src/scanner.rs`

- [ ] **Step 1: Write failing hidden-tree and dependency-tree tests**

Extend scanner coverage with:

1. A hidden directory containing a visible child with `package.json`; the child must not become a project.
2. An application containing `node_modules/dependency/package.json`; the dependency must not become a project.

Set `scan_depth` high enough that the current iterator discovers both children, then assert only the intended application project is present.

- [ ] **Step 2: Run both tests and verify RED**

Run the two focused scanner tests. Expected: the hidden child and dependency are incorrectly returned as projects.

- [ ] **Step 3: Add traversal pruning**

Use `WalkDir::into_iter().filter_entry(...)` before `filter_map`. Descend into the configured root, but reject descendant directories when the name starts with `.` or equals `node_modules`.

```rust
fn should_descend(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return true;
    }
    entry.file_name().to_str().is_none_or(|name| {
        !name.starts_with('.') && name != "node_modules"
    })
}
```

Do not modify `PROJECT_MARKERS` or `detect_project`.

- [ ] **Step 4: Verify GREEN and existing scanner behavior**

Run the focused tests, then `cargo test scanner::tests --lib`.

### Task 3: Add executable-level and pathological-tree benchmarks

**Files:**
- Create: `benches/cli_bench.rs`
- Modify: `benches/common/mod.rs`
- Modify: `benches/scan_bench.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Add a Criterion binary benchmark target**

Configure `cli_bench` with `harness = false`. Use `env!("CARGO_BIN_EXE_qr")`, isolated config directories, inherited-null stdio, and stats disabled. Benchmark:

- `qr config path`
- exact `qr go --print-path` over 1,000 cached projects
- no-match `qr go --print-path` over 5,000 cached projects
- `qr run --output true` on Unix

- [ ] **Step 2: Add pathological scan fixtures**

Add fixtures for a large hidden `.git`-like subtree and a `node_modules` dependency tree. Benchmark them separately from the existing small/medium/large synthetic project counts so regressions in pruning remain visible.

- [ ] **Step 3: Compile and smoke-run the benchmarks**

Run:

```text
cargo bench --bench cli_bench --no-run
cargo bench --bench scan_bench --no-run
```

Then run each benchmark with Criterion's quick mode or a reduced sample size and confirm every command/fixture executes successfully.

### Task 4: Tune and measure the release profile

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add release settings**

```toml
[profile.release]
lto = "thin"
codegen-units = 1
strip = "symbols"
```

- [ ] **Step 2: Build and record the artifact**

Run `cargo build --release --locked`, record the binary size, and measure `config path`, exact cached `go`, and no-match cached `go` using the same isolated fixtures as the audit.

- [ ] **Step 3: Verify the complete change**

Run:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo bench --bench cli_bench --no-run
cargo bench --bench scan_bench --no-run
```

Confirm `git diff --check` and inspect the complete diff before publishing.
