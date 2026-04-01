use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::config::AppConfig;

const PROJECT_MARKERS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "Makefile",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectEntry {
    pub name: String,
    pub path: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCache {
    pub scanned_at_unix_ms: u128,
    pub projects: Vec<ProjectEntry>,
}

pub fn scan_projects(config: &AppConfig) -> Result<ProjectCache> {
    config.ensure_parent_dirs()?;

    let mut seen = HashSet::new();
    let mut projects = Vec::new();

    for root in config.project_root_paths() {
        if !root.exists() {
            continue;
        }

        for entry in WalkDir::new(&root)
            .follow_links(false)
            .min_depth(0)
            .max_depth(config.projects.scan_depth)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_none_or(|name| !name.starts_with('.'))
            })
            .filter(|entry| entry.file_type().is_dir())
        {
            if let Some(project) = detect_project(entry.path())? {
                if seen.insert(project.path.clone()) {
                    projects.push(project);
                }
            }
        }
    }

    projects.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));

    let cache = ProjectCache {
        scanned_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        projects,
    };

    fs::write(config.cache_path(), serde_json::to_vec_pretty(&cache)?)?;
    Ok(cache)
}

pub fn load_or_scan_projects(config: &AppConfig) -> Result<ProjectCache> {
    let path = config.cache_path();
    if path.exists() {
        let raw = fs::read(&path)
            .with_context(|| format!("Failed to read project cache {}", path.display()))?;
        let cache: ProjectCache =
            serde_json::from_slice(&raw).context("Failed to parse project cache")?;
        return Ok(cache);
    }
    scan_projects(config)
}

fn detect_project(path: &Path) -> Result<Option<ProjectEntry>> {
    let git_indicator = path.join(".git");
    if git_indicator.exists() {
        let name = git_remote_name(path).unwrap_or_else(|| {
            path.file_name()
                .map(|part| part.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string())
        });
        return Ok(Some(ProjectEntry {
            name,
            path: path.display().to_string(),
            source: "git".to_string(),
        }));
    }

    if PROJECT_MARKERS
        .iter()
        .any(|marker| path.join(marker).is_file())
    {
        let name = path
            .file_name()
            .map(|part| part.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());

        return Ok(Some(ProjectEntry {
            name,
            path: path.display().to_string(),
            source: "marker".to_string(),
        }));
    }

    Ok(None)
}

fn git_remote_name(project_dir: &Path) -> Option<String> {
    let git_config = git_config_path(project_dir)?;
    let raw = fs::read_to_string(git_config).ok()?;
    let mut in_origin = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[remote ") {
            in_origin = trimmed == r#"[remote "origin"]"#;
            continue;
        }
        if in_origin && trimmed.starts_with("url = ") {
            return Some(canonical_name_from_remote(
                trimmed.trim_start_matches("url = ").trim(),
            ));
        }
    }

    None
}

fn git_config_path(project_dir: &Path) -> Option<PathBuf> {
    let git_indicator = project_dir.join(".git");
    if git_indicator.is_dir() {
        return Some(git_indicator.join("config"));
    }
    if git_indicator.is_file() {
        let raw = fs::read_to_string(&git_indicator).ok()?;
        let gitdir = raw.trim().strip_prefix("gitdir:")?.trim();
        let base = if Path::new(gitdir).is_absolute() {
            PathBuf::from(gitdir)
        } else {
            project_dir.join(gitdir)
        };
        return Some(base.join("config"));
    }
    None
}

fn canonical_name_from_remote(remote: &str) -> String {
    remote
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(remote)
        .trim_end_matches(".git")
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;
    use crate::config::AppConfig;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn scanner_uses_git_remote_name_when_available() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("workspace");
        fs::create_dir_all(project.join(".git")).unwrap();
        fs::write(
            project.join(".git/config"),
            r#"[remote "origin"]
    url = git@github.com:baanish/orion-app.git
"#,
        )
        .unwrap();

        let detected = detect_project(&project).unwrap().unwrap();
        assert_eq!(detected.name, "orion-app");
        assert_eq!(detected.source, "git");
    }

    #[test]
    fn scanner_uses_marker_when_git_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("api");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("package.json"), "{}").unwrap();

        let detected = detect_project(&project).unwrap().unwrap();
        assert_eq!(detected.name, "api");
        assert_eq!(detected.source, "marker");
    }

    #[test]
    fn scan_projects_writes_cache_file() {
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("dev");
        let cfg_dir = tmp.path().join("cfg");
        fs::create_dir_all(root.join("proj/.git")).unwrap();
        fs::write(root.join("proj/.git/config"), "").unwrap();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &cfg_dir);
        }

        let config = AppConfig::load_from_env_with_path(cfg_dir.join("config.toml")).unwrap();
        let mut config = config;
        config.projects.roots = vec![root.display().to_string()];
        config.stats.db_path = cfg_dir.join("stats.db").display().to_string();

        let cache = scan_projects(&config).unwrap();
        assert_eq!(cache.projects.len(), 1);
        assert!(config.cache_path().exists());

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn scan_projects_skips_dot_directories_with_project_markers() {
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("dev");
        let cfg_dir = tmp.path().join("cfg");
        fs::create_dir_all(root.join(".next")).unwrap();
        fs::write(root.join(".next/package.json"), "{}").unwrap();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &cfg_dir);
        }

        let config = AppConfig::load_from_env_with_path(cfg_dir.join("config.toml")).unwrap();
        let mut config = config;
        config.projects.roots = vec![root.display().to_string()];
        config.stats.db_path = cfg_dir.join("stats.db").display().to_string();

        let cache = scan_projects(&config).unwrap();
        assert!(cache.projects.is_empty());

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
}
