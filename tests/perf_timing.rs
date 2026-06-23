use std::fs;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use quick_runner::{
    commands::go,
    config::AppConfig,
    scanner::{ProjectCache, ProjectEntry, scan_projects, write_project_cache},
    stats_db::{CommandStats, StatsDb},
};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn scan_timing_flow_runs_against_library_api() {
    let _guard = env_lock().lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("workspace");
    let cfg_dir = tmp.path().join("cfg");

    fs::create_dir_all(root.join("demo/.git")).unwrap();
    fs::write(root.join("demo/.git/config"), "").unwrap();

    unsafe {
        std::env::set_var("QR_CONFIG_DIR", &cfg_dir);
    }

    let mut config = AppConfig::load_from_env_with_path(cfg_dir.join("config.toml")).unwrap();
    config.projects.roots = vec![root.display().to_string()];
    config.stats.db_path = cfg_dir.join("stats.db").display().to_string();

    // Regression guard (test isolation): the cache must resolve inside the temp
    // config dir, never the developer's real one. QR_CONFIG_DIR is honored on every
    // OS; XDG_CONFIG_HOME is a no-op on macOS, which previously let `cargo test`
    // overwrite the real ~/Library/Application Support/qr cache.
    assert!(
        config.cache_path().starts_with(&cfg_dir),
        "cache path {:?} escaped the temp config dir {:?}",
        config.cache_path(),
        cfg_dir
    );

    let started = Instant::now();
    let cache = scan_projects(&config).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(cache.projects.len(), 1);
    assert!(elapsed.as_nanos() > 0);
    eprintln!("scan_timing_flow_runs_against_library_api={elapsed:?}");

    unsafe {
        std::env::remove_var("QR_CONFIG_DIR");
    }
}

#[test]
fn go_timing_flow_hits_cache_end_to_end() {
    let _guard = env_lock().lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join("cfg");

    unsafe {
        std::env::set_var("QR_CONFIG_DIR", &cfg_dir);
    }

    let mut config = AppConfig::load_from_env_with_path(cfg_dir.join("config.toml")).unwrap();
    config.stats.db_path = cfg_dir.join("stats.db").display().to_string();
    assert!(
        config.cache_path().starts_with(&cfg_dir),
        "cache path {:?} escaped the temp config dir {:?}",
        config.cache_path(),
        cfg_dir
    );
    config.ensure_parent_dirs().unwrap();
    write_project_cache(
        &config.cache_path(),
        &ProjectCache {
            scanned_at_unix_ms: 7,
            projects: vec![ProjectEntry {
                name: "quick-runner".into(),
                path: "/tmp/quick-runner".into(),
                source: "git".into(),
            }],
        },
    )
    .unwrap();

    let started = Instant::now();
    let result = go::execute(&config, "quick-runner").unwrap();
    let elapsed = started.elapsed();

    assert_eq!(result.path, "/tmp/quick-runner");
    assert!(elapsed.as_nanos() > 0);
    eprintln!("go_timing_flow_hits_cache_end_to_end={elapsed:?}");

    unsafe {
        std::env::remove_var("QR_CONFIG_DIR");
    }
}

#[test]
fn stats_db_round_trip_has_measurable_latency() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("stats.db");
    let started = Instant::now();

    let db = StatsDb::open(&db_path).unwrap();
    db.record(&CommandStats {
        command_type: "go".into(),
        ai_used: true,
        input_tokens: 21,
        output_tokens: 13,
        latency_ms: 8,
        provider: "FirePass".into(),
        estimated_cost_usd: 0.0001,
        cost_known: true,
    })
    .unwrap();
    let summary = db.summary().unwrap();
    let elapsed = started.elapsed();

    assert_eq!(summary.total_runs, 1);
    assert!(elapsed.as_nanos() > 0);
    eprintln!("stats_db_round_trip_has_measurable_latency={elapsed:?}");
}
