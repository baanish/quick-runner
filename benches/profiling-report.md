# Profiling Report

Method: manual instrumentation and Criterion decomposition. `cargo flamegraph` was not installed in this environment, and macOS `sample` could not inspect processes under the sandbox, so hotspot ranking is based on benchmark medians plus direct code-path analysis from the benchmarked production functions.

## Scope

- `qr scan` was profiled using the `scan_bench` fixture at `25`, `100`, and `300` project directories.
- `qr go` was profiled using the `go_bench` fixture for both cache-hit and cache-miss paths.

## Top 5 Hottest Functions

1. `quick_runner::scanner::scan_projects`
   Evidence: `27-28 ms` median for the `300`-project scan baseline, dwarfing every other public operation.
2. `quick_runner::scanner::write_project_cache`
   Evidence: `630 us` baseline median at `5000` projects; scan always pays this cost at the end of a run.
3. `quick_runner::commands::go::execute`
   Evidence: `156 us` cache-hit and `280 us` cache-miss baseline medians, including cache load + ranking.
4. `quick_runner::commands::go::rank_matches`
   Evidence: `82 us` median at `5000` candidates, showing the ranking stage itself was a substantial share of `qr go`.
5. `quick_runner::stats_db::StatsDb::record`
   Evidence: `735 us` baseline median for the write-heavy stats path, making it the hottest non-scan operation outside cache I/O.

## Memory Allocation Patterns

- `qr go` allocated aggressively in `rank_matches` by cloning `ProjectEntry` values into the scored vector and lowercasing candidate strings repeatedly.
- Cache writes allocated a full pretty-printed JSON buffer (`serde_json::to_vec_pretty`) on every scan, which inflated both CPU time and output size.
- Scan paths build many temporary `PathBuf` values via repeated `join()` calls when checking `.git` and project-marker files.
- Config loading allocated and parsed the default TOML even when a real config file existed, then threw that parsed value away.
- Cache reads and `qr go` misses are dominated by JSON deserialization into a fresh `Vec<ProjectEntry>`.

## Surprising Findings

- The gap between a `qr go` cache hit and miss was mostly cache load + parse overhead, not fuzzy matching itself.
- Shell detection and wrapper generation were far below the noise floor for the rest of the CLI; they were not worth optimization effort.
- Compact JSON cache writes looked promising immediately in the isolated cache benches and also reduced scan end-to-end latency once the full suite settled.
- Config parsing was already cheap in absolute terms, but the wasted default parse showed up clearly enough to be worth removing.

## Optimization Targets Identified

- Remove avoidable JSON formatting overhead from project-cache writes.
- Avoid redundant parsing in config loads when a config file is already present.
- Reduce clone/lowercase churn in `go` ranking and only clone the finalists.
- Leave shell helpers alone; they are already negligible.
- Treat SQLite changes conservatively unless a full-suite benchmark proves a win.
