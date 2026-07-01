use anyhow::Result;

use crate::{
    config::{AppConfig, cache_file_path, config_file_path, legacy_config_dir},
    scanner::read_project_cache,
};

/// Report the health of `config.toml` and the project cache, and where they
/// live. This command deliberately works even when `config.toml` is missing or
/// unparseable — it is a recovery aid — so it never fails just because the
/// config does.
pub fn run() -> Result<()> {
    println!("QuickRunner doctor");
    println!("──────────────────");

    let config_path = config_file_path();
    let config = if !config_path.exists() {
        println!(
            "config: {} — not found (run `qr init`)",
            config_path.display()
        );
        None
    } else {
        match AppConfig::load() {
            Ok(config) => {
                println!("config: {} — ok", config_path.display());
                Some(config)
            }
            Err(error) => {
                println!("config: {} — INVALID: {error:#}", config_path.display());
                println!("  fix it with `qr config`, or move it aside and run `qr init`");
                None
            }
        }
    };

    let cache_path = cache_file_path();
    if !cache_path.exists() {
        println!(
            "cache:  {} — not built (run `qr scan`)",
            cache_path.display()
        );
    } else {
        match read_project_cache(&cache_path) {
            Ok(cache) => println!(
                "cache:  {} — ok ({} projects)",
                cache_path.display(),
                cache.projects.len()
            ),
            Err(error) => println!(
                "cache:  {} — INVALID: {error:#} (run `qr scan` to rebuild)",
                cache_path.display()
            ),
        }
    }

    if let Some(config) = config {
        println!("roots:  {}", config.projects.roots.join(", "));
    }

    if let Some(legacy_path) = legacy_config_dir() {
        if legacy_path.exists() {
            let has_files = std::fs::read_dir(&legacy_path)
                .map(|entries| entries.filter_map(Result::ok).any(|e| e.path().is_file()))
                .unwrap_or(false);
            if has_files {
                println!(
                    "legacy: {} — still has files (config was moved to ~/.qr; safe to remove after verifying)",
                    legacy_path.display()
                );
            }
        }
    }

    Ok(())
}
