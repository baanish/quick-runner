# Baseline Benchmark Results

Environment: local `cargo bench` run on the pre-optimization tree using Criterion 0.5.1.

## Criterion Baseline

| Area | Benchmark | Baseline median |
| --- | --- | ---: |
| Scan | `scan_projects/small/25` | `1.3552 ms` |
| Scan | `scan_projects/medium/100` | `7.1623 ms` |
| Scan | `scan_projects/large/300` | `28.456 ms` |
| Go ranking | `go_rank_matches/100` | `21.859 us` |
| Go ranking | `go_rank_matches/1000` | `17.023 us` |
| Go ranking | `go_rank_matches/5000` | `81.922 us` |
| Go execute | `go_execute/cache_hit/service-0042` | `156.04 us` |
| Go execute | `go_execute/cache_miss/does-not-exist` | `279.81 us` |
| Config | `config_parse_default_toml` | `2.5533 us` |
| Config | `config_load_from_path` | `16.171 us` |
| Config | `config_load_with_env_overrides` | `16.419 us` |
| Cache write | `cache_write/100` | `55.407 us` |
| Cache write | `cache_write/1000` | `150.22 us` |
| Cache write | `cache_write/5000` | `629.99 us` |
| Cache read | `cache_read/100` | `21.708 us` |
| Cache read | `cache_read/1000` | `139.82 us` |
| Cache read | `cache_read/5000` | `665.35 us` |
| Stats DB | `stats_record_in_memory` | `735.19 us` |
| Stats DB | `stats_summary/10` | `68.940 us` |
| Stats DB | `stats_summary/100` | `80.978 us` |
| Stats DB | `stats_summary/1000` | `141.23 us` |
| Shell | `shell_detect_zsh` | `6.2418 ns` |
| Shell | `shell_detect_fish` | `6.5810 ns` |
| Shell | `shell_wrapper_zsh` | `42.403 ns` |
| Shell | `shell_wrapper_fish` | `42.993 ns` |

## End-To-End Timing Tests

Measured with `cargo test --test perf_timing -- --nocapture` on the pre-optimization tree.

| Flow | Baseline latency |
| --- | ---: |
| `scan_timing_flow_runs_against_library_api` | `681.708 us` |
| `go_timing_flow_hits_cache_end_to_end` | `644.042 us` |
| `stats_db_round_trip_has_measurable_latency` | `5.086708 ms` |

## Notes

- `scan_projects` was the slowest public-facing CLI operation by a wide margin.
- `go` cache misses cost about `1.8x` a cache hit even before any interaction/UI path, which pointed to cache loading and ranking as the main work.
- Shell helpers were effectively free relative to file-system, JSON, and SQLite work.
