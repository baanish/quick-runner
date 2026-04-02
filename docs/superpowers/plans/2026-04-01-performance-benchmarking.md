# QuickRunner Performance Benchmarking And Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add repeatable benchmark and profiling coverage for `qr`, use the measurements to drive scoped optimizations, and preserve existing CLI behavior.

**Architecture:** Expose the current binary-only modules through a small library crate so Criterion benches and end-to-end timing tests can call the same production code paths as the CLI. Build benchmark fixtures around temporary directories, cache files, and SQLite databases, then optimize only the hot paths that show measurable improvement.

**Tech Stack:** Rust 2024, Criterion, rusqlite, tempfile, std timing APIs, existing `cargo test` / `cargo bench` workflow

---

### Task 1: Expose Benchmarkable Surfaces

**Files:**
- Create: `src/lib.rs`
- Modify: `src/main.rs`
- Test: `tests/cli.rs`

- [ ] Add a library entrypoint that re-exports the existing internal modules used by benches and integration tests without changing CLI behavior.
- [ ] Update `src/main.rs` to consume the library modules instead of declaring duplicate private modules.
- [ ] Run `cargo test` to confirm the refactor is behavior-preserving before any benchmark work starts.

### Task 2: Add Benchmark Fixtures And Regression Coverage

**Files:**
- Modify: `Cargo.toml`
- Create: `benches/common/mod.rs`
- Create: `benches/scan_bench.rs`
- Create: `benches/go_bench.rs`
- Create: `benches/config_bench.rs`
- Create: `benches/cache_bench.rs`
- Create: `benches/stats_db_bench.rs`
- Create: `benches/shell_bench.rs`
- Create: `tests/perf_timing.rs`

- [ ] Add `criterion` as a dev-dependency and configure explicit bench targets so `cargo bench` runs the new suite.
- [ ] Write failing smoke coverage proving the benches compile against the real production APIs.
- [ ] Create shared tempdir fixture builders for realistic project trees, cache files, configs, and SQLite databases.
- [ ] Add Criterion coverage for every requested public-facing operation:
- [ ] `qr scan` style project scanning across multiple directory counts and depths.
- [ ] `qr go` lookup and fuzzy ranking for exact hits, cache hits, and misses.
- [ ] Config load/parse with defaults, file-backed config, and env overrides.
- [ ] Project cache JSON read/write.
- [ ] Stats DB inserts and summary reads.
- [ ] Shell detection and wrapper generation.
- [ ] Add integration-style latency tests that execute representative end-to-end flows and record wall-clock durations without asserting on fragile absolute thresholds.

### Task 3: Capture Baseline Numbers

**Files:**
- Create: `benches/baseline-results.md`

- [ ] Run the full benchmark suite on the unoptimized code.
- [ ] Record the relevant baseline numbers in `benches/baseline-results.md`, grouped by operation and fixture size.
- [ ] Run the timing tests and capture their observed latencies in the same document or linked notes.

### Task 4: Profile Hot Paths

**Files:**
- Create: `benches/profiling-report.md`

- [ ] Profile `qr scan` with a realistic synthetic directory tree and capture where time is spent.
- [ ] Profile `qr go` separately for cache-hit and cache-miss paths.
- [ ] Summarize the top five hottest functions, allocation behavior, surprises, and concrete optimization targets in `benches/profiling-report.md`.

### Task 5: Optimize Scan, Lookup, Config, Cache, SQLite, And Shell Paths

**Files:**
- Modify: `src/scanner.rs`
- Modify: `src/commands/go.rs`
- Modify: `src/config.rs`
- Modify: `src/stats_db.rs`
- Modify: `src/shell.rs`
- Modify: any supporting benchmark fixture files created above

- [ ] For each optimization candidate, start by adding or updating a failing or insufficient benchmark/test that isolates the target behavior.
- [ ] Make one measurable optimization at a time, keeping the diff tightly scoped.
- [ ] Re-run only the relevant benchmark immediately after each change.
- [ ] If an optimization does not measurably help, revert it before moving on.
- [ ] Prefer low-risk changes first: fewer allocations, better statement reuse, less repeated parsing, smarter scan filtering, reduced cloning.
- [ ] Consider higher-impact changes like prepared statements or WAL mode only if the benchmark data shows a clear benefit and functionality stays unchanged.

### Task 6: Final Comparison And Verification

**Files:**
- Create: `benches/optimization-results.md`

- [ ] Re-run the full benchmark suite after the accepted optimizations.
- [ ] Record before/after comparisons for each retained optimization in `benches/optimization-results.md`.
- [ ] Run `cargo test` and confirm all 16 existing tests still pass.
- [ ] Summarize any remaining bottlenecks or deferred work without changing the public CLI interface.
