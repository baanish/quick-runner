//! Shared fixtures for the benches. Each bench binary uses only a subset, so the
//! unused helpers are expected rather than dead code.
#![allow(dead_code)]

use std::{
    fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use quick_runner::{
    config::AppConfig,
    scanner::{ProjectCache, ProjectEntry, write_project_cache},
    stats_db::{CommandStats, StatsDb},
};
use tempfile::TempDir;

pub fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub struct ScanFixture {
    pub _tmp: TempDir,
    pub config: AppConfig,
}

pub fn scan_fixture(project_count: usize, nested_dirs: usize) -> ScanFixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("workspace");
    let cfg_dir = tmp.path().join("cfg");
    let _guard = env_lock().lock().unwrap();

    unsafe {
        std::env::set_var("QR_CONFIG_DIR", &cfg_dir);
        std::env::set_var("HOME", tmp.path());
    }

    for index in 0..project_count {
        let mut project_dir = root.join(format!("project-{index:04}"));
        for depth in 0..nested_dirs {
            project_dir = project_dir.join(format!("layer-{depth}"));
        }
        fs::create_dir_all(project_dir.join(".git")).unwrap();
        fs::write(project_dir.join(".git/config"), "").unwrap();
    }

    let mut config = AppConfig::load_from_env_with_path(cfg_dir.join("config.toml")).unwrap();
    config.projects.roots = vec![root.display().to_string()];
    config.projects.scan_depth = nested_dirs + 2;
    config.stats.db_path = cfg_dir.join("stats.db").display().to_string();

    ScanFixture { _tmp: tmp, config }
}

pub struct GoFixture {
    pub _tmp: TempDir,
    pub config: AppConfig,
    pub entries: Vec<ProjectEntry>,
}

pub fn go_fixture(project_count: usize) -> GoFixture {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join("cfg");
    let _guard = env_lock().lock().unwrap();
    let entries = sample_projects(project_count);
    let cache = ProjectCache {
        scanned_at_unix_ms: 1,
        projects: entries.clone(),
    };

    unsafe {
        std::env::set_var("QR_CONFIG_DIR", &cfg_dir);
        std::env::set_var("HOME", tmp.path());
    }

    let mut config = AppConfig::load_from_env_with_path(cfg_dir.join("config.toml")).unwrap();
    config.projects.roots = vec![tmp.path().join("workspace").display().to_string()];
    config.stats.db_path = cfg_dir.join("stats.db").display().to_string();
    config.ensure_parent_dirs().unwrap();
    write_project_cache(&config.cache_path(), &cache).unwrap();

    GoFixture {
        _tmp: tmp,
        config,
        entries,
    }
}

pub fn sample_projects(project_count: usize) -> Vec<ProjectEntry> {
    (0..project_count)
        .map(|index| ProjectEntry {
            name: format!("service-{index:04}"),
            path: format!("/tmp/workspace/service-{index:04}"),
            source: if index % 2 == 0 { "git" } else { "marker" }.into(),
        })
        .collect()
}

pub fn sample_cache(project_count: usize) -> ProjectCache {
    ProjectCache {
        scanned_at_unix_ms: 1_717_171_717,
        projects: sample_projects(project_count),
    }
}

pub fn config_file_with_contents(contents: &str) -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    fs::write(&path, contents).unwrap();
    (tmp, path)
}

pub fn seeded_stats_db(run_count: usize) -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("stats.db");
    let db = StatsDb::open(&path).unwrap();
    for index in 0..run_count {
        db.record(&CommandStats {
            command_type: if index % 2 == 0 { "go" } else { "scan" }.into(),
            ai_used: index % 3 == 0,
            input_tokens: (index * 3) as u64,
            output_tokens: (index * 2) as u64,
            latency_ms: (10 + index) as u128,
            provider: if index % 3 == 0 { "FirePass" } else { "no AI" }.into(),
            estimated_cost_usd: index as f64 / 10_000.0,
            cost_known: true,
        })
        .unwrap();
    }
    drop(db);
    (tmp, path)
}
