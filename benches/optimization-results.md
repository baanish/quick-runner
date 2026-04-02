# Optimization Results

Final suite run: `cargo bench` on the optimized tree after keeping only the changes with benchmark support.

## Retained Optimizations

### 1. Skip parsing the default config when a real config file exists

Why: `AppConfig::load_from_env_with_path` parsed `DEFAULT_CONFIG` first and then replaced it with the on-disk config, so every file-backed config load paid for an unnecessary parse.

Files:
- `src/config.rs`

Before/after:

| Benchmark | Before | After | Result |
| --- | ---: | ---: | --- |
| `config_load_from_path` | `16.171 us` | `13.458 us` | `16.8% faster` |
| `config_load_with_env_overrides` | `16.419 us` | `13.775 us` | `16.1% faster` |

### 2. Write the project cache as compact JSON instead of pretty-printed JSON

Why: the cache file is internal application state, and pretty-printing added serialization work plus larger reads on the next `qr go`.

Files:
- `src/scanner.rs`

Before/after:

| Benchmark | Before | After | Result |
| --- | ---: | ---: | --- |
| `cache_write/100` | `55.407 us` | `50.305 us` | `9.2% faster` |
| `cache_write/1000` | `150.22 us` | `86.371 us` | `42.5% faster` |
| `cache_read/1000` | `139.82 us` | `126.87 us` | `9.3% faster` |
| `cache_read/5000` | `665.35 us` | `593.26 us` | `10.8% faster` |
| `scan_projects/medium/100` | `7.1623 ms` | `6.8468 ms` | `4.4% faster` |

### 3. Clone only the final `go` matches instead of every scored candidate

Why: `rank_matches` cloned `ProjectEntry` for every scored candidate and lowercased candidate strings repeatedly. Keeping borrowed entries through scoring cut most of the churn and only clones the returned top matches.

Files:
- `src/commands/go.rs`

Before/after:

| Benchmark | Before | After | Result |
| --- | ---: | ---: | --- |
| `go_rank_matches/1000` | `17.023 us` | `4.789 us` | `71.9% faster` |
| `go_rank_matches/5000` | `81.922 us` | `21.243 us` | `74.1% faster` |
| `go_execute/cache_hit/service-0042` | `156.04 us` | `130.08 us` | `16.6% faster` |
| `go_execute/cache_miss/does-not-exist` | `279.81 us` | `246.37 us` | `11.9% faster` |

## Full-Suite Final Snapshot

| Area | Final median |
| --- | ---: |
| `scan_projects/small/25` | `1.2659 ms` |
| `scan_projects/medium/100` | `6.8468 ms` |
| `scan_projects/large/300` | `27.262 ms` |
| `go_rank_matches/100` | `19.256 us` |
| `go_rank_matches/1000` | `4.789 us` |
| `go_rank_matches/5000` | `21.243 us` |
| `go_execute/cache_hit/service-0042` | `130.08 us` |
| `go_execute/cache_miss/does-not-exist` | `246.37 us` |
| `config_parse_default_toml` | `2.5598 us` |
| `config_load_from_path` | `13.458 us` |
| `config_load_with_env_overrides` | `13.775 us` |
| `cache_write/100` | `50.305 us` |
| `cache_write/1000` | `86.371 us` |
| `cache_write/5000` | `380.62 us` |
| `cache_read/100` | `20.223 us` |
| `cache_read/1000` | `126.87 us` |
| `cache_read/5000` | `593.26 us` |
| `stats_record_in_memory` | `608.87 us` |
| `stats_summary/10` | `58.331 us` |
| `stats_summary/100` | `66.277 us` |
| `stats_summary/1000` | `137.57 us` |
| `shell_detect_zsh` | `5.8523 ns` |
| `shell_detect_fish` | `6.2107 ns` |
| `shell_wrapper_zsh` | `40.739 ns` |
| `shell_wrapper_fish` | `43.141 ns` |

## Final End-To-End Timing Snapshot

Measured with `cargo test --test perf_timing -- --nocapture` on the optimized tree.

| Flow | Final latency |
| --- | ---: |
| `scan_timing_flow_runs_against_library_api` | `207.333 us` |
| `go_timing_flow_hits_cache_end_to_end` | `42.917 us` |
| `stats_db_round_trip_has_measurable_latency` | `1.1515 ms` |

## Reverted Candidate

- `src/stats_db.rs`: prepared-statement caching was tried and then reverted because the full-suite results were not consistently better enough to justify keeping the change.
