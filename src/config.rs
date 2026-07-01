use std::{
    env, fs,
    path::{Component, Path, PathBuf},
    sync::Once,
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::ai::providers::{AiProtocol, ProviderConfig};
use crate::atomic;

const DEFAULT_CONFIG: &str = include_str!("../config/default.toml");
const LEGACY_CODEX_AGENT: &str = "codex exec";
const LEGACY_CLAUDE_AGENT: &str = "claude --dangerously-skip-permissions -p";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub general: GeneralConfig,
    pub projects: ProjectsConfig,
    pub ai: AiConfig,
    pub stats: StatsConfig,
    #[serde(rename = "do", default)]
    pub do_config: DoConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    pub default_run_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectsConfig {
    pub roots: Vec<String>,
    pub scan_depth: usize,
    pub scan_interval_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    pub protocol: AiProtocol,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    pub api_key_env: String,
    #[serde(default)]
    pub fallback: Option<FallbackAiConfig>,
    /// Optional per-million-token price override for cost estimates. When unset,
    /// the models.dev snapshot is used. Authoritative for custom/proxy endpoints.
    #[serde(default)]
    pub cost: Option<crate::pricing::Price>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackAiConfig {
    pub protocol: AiProtocol,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    pub api_key_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsConfig {
    pub enabled: bool,
    pub db_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoConfig {
    #[serde(default)]
    pub agents: AgentConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_codex_agent")]
    pub codex: String,
    #[serde(default = "default_claude_agent")]
    pub claude: String,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        Self::load_from_env_with_path(config_file_path())
    }

    pub fn load_from_str(raw: &str) -> Result<Self> {
        toml::from_str(raw).context("Failed to parse config.toml")
    }

    pub fn load_from_env_with_path(path: PathBuf) -> Result<Self> {
        let mut config = Self::load_file_without_env(&path)?;
        apply_env_overrides(&mut config)?;
        Ok(config)
    }

    /// Parse `config.toml` on disk without applying `QR_*` env overrides.
    pub fn load_file_without_env(path: &Path) -> Result<Self> {
        if path.exists() {
            let raw = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file {}", path.display()))?;
            let mut config = Self::load_from_str(&raw)?;
            migrate_legacy_agent_defaults(path, &raw, &mut config)?;
            Ok(config)
        } else {
            Self::load_from_str(DEFAULT_CONFIG)
        }
    }

    pub fn ensure_parent_dirs(&self) -> Result<()> {
        for path in [self.stats_db_path(), cache_file_path(), config_file_path()] {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
        }
        Ok(())
    }

    pub fn stats_db_path(&self) -> PathBuf {
        if self.stats.db_path.is_empty() || self.stats.db_path == "__default__" {
            config_dir().join("stats.db")
        } else {
            expand_path(&self.stats.db_path)
        }
    }

    pub fn cache_path(&self) -> PathBuf {
        cache_file_path()
    }

    pub fn project_root_paths(&self) -> Vec<PathBuf> {
        self.projects
            .roots
            .iter()
            .map(|root| expand_path(root))
            .collect()
    }

    pub fn ai_primary_provider(&self) -> ProviderConfig {
        ProviderConfig {
            protocol: self.ai.protocol,
            base_url: self.ai.base_url.clone(),
            model: self.ai.model.clone(),
            api_key: self.ai.api_key.clone(),
            api_key_env: self.ai.api_key_env.clone(),
        }
    }

    pub fn ai_fallback_provider(&self) -> Option<ProviderConfig> {
        self.ai.fallback.as_ref().map(|fallback| ProviderConfig {
            protocol: fallback.protocol,
            base_url: fallback.base_url.clone(),
            model: fallback.model.clone(),
            api_key: fallback.api_key.clone(),
            api_key_env: fallback.api_key_env.clone(),
        })
    }
}

/// Legacy config location before ~/.qr (XDG config dir on Linux, Application
/// Support on macOS).
pub fn legacy_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|base| base.join("qr"))
}

pub fn config_dir() -> PathBuf {
    if let Ok(override_dir) = std::env::var("QR_CONFIG_DIR") {
        return PathBuf::from(override_dir);
    }
    let dir = dirs::home_dir()
        .map(|home| home.join(".qr"))
        .unwrap_or_else(|| PathBuf::from(".qr"));

    static MIGRATE_ONCE: Once = Once::new();
    MIGRATE_ONCE.call_once(|| {
        if let Err(error) = migrate_legacy_config(&dir, legacy_config_dir().as_deref()) {
            eprintln!("qr: warning: failed to migrate legacy config: {error:#}");
        }
    });

    dir
}

/// Move config files from the legacy directory into `new_dir`. Returns `true`
/// when at least one file was migrated. Migration is retried on later runs until
/// the legacy directory is empty and the `.migrated-from-legacy` marker is written.
pub fn migrate_legacy_config(new_dir: &Path, legacy_dir: Option<&Path>) -> Result<bool> {
    let Some(legacy_dir) = legacy_dir else {
        return Ok(false);
    };

    if new_dir.join(".migrated-from-legacy").exists() {
        return Ok(false);
    }

    if !legacy_dir.is_dir() {
        return Ok(false);
    }

    let mut legacy_files: Vec<_> = fs::read_dir(legacy_dir)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_file() || kind.is_symlink())
        })
        .collect();

    if legacy_files.is_empty() {
        return Ok(false);
    }

    fs::create_dir_all(new_dir)?;

    // Move data files before config.toml so a mid-loop failure can be retried.
    legacy_files.sort_by_key(|entry| entry.file_name() == "config.toml");

    let mut moved_any = false;
    for entry in &legacy_files {
        let src = entry.path();
        let dest = new_dir.join(entry.file_name());
        if dest.exists() {
            if src.exists() {
                fs::remove_file(&src)?;
                moved_any = true;
            }
            continue;
        }
        if fs::rename(&src, &dest).is_err() {
            fs::copy(&src, &dest)?;
            fs::remove_file(&src)?;
        }
        moved_any = true;
    }

    if fs::read_dir(legacy_dir)?.next().is_some() {
        anyhow::bail!(
            "legacy config directory {} still has files after migration attempt",
            legacy_dir.display()
        );
    }

    let _ = fs::remove_dir(legacy_dir);

    let config_path = new_dir.join("config.toml");
    if config_path.exists() {
        rewrite_legacy_paths_in_config(&config_path, legacy_dir, new_dir)?;
    }

    fs::write(
        new_dir.join(".migrated-from-legacy"),
        legacy_dir.display().to_string(),
    )?;

    Ok(moved_any)
}

fn rewrite_legacy_paths_in_config(
    config_path: &Path,
    legacy_dir: &Path,
    new_dir: &Path,
) -> Result<()> {
    let raw = fs::read_to_string(config_path)?;
    let config = AppConfig::load_from_str(&raw)?;

    if config.stats.db_path.is_empty() || config.stats.db_path == "__default__" {
        return Ok(());
    }

    let db_path = expand_path(&config.stats.db_path);
    let Some(new_value) = migrated_stats_db_path(&db_path, legacy_dir, new_dir) else {
        return Ok(());
    };

    if let Some(updated) = rewrite_stats_db_path_in_toml(&raw, &new_value)? {
        atomic::write_private(config_path, updated.as_bytes())?;
    }

    Ok(())
}

fn migrated_stats_db_path(db_path: &Path, legacy_dir: &Path, new_dir: &Path) -> Option<String> {
    let default_legacy_db = legacy_dir.join("stats.db");
    if db_path == default_legacy_db {
        return Some("__default__".into());
    }
    if !path_is_under(db_path, legacy_dir) {
        return None;
    }
    let relative = db_path.strip_prefix(legacy_dir).ok()?;
    let relative = relative
        .strip_prefix(Component::RootDir)
        .unwrap_or(relative);
    Some(path_for_config(&new_dir.join(relative)))
}

fn path_for_config(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            let rest = rest.to_string_lossy();
            return if rest.is_empty() {
                "~".into()
            } else if rest.starts_with('/') {
                format!("~{rest}")
            } else {
                format!("~/{rest}")
            };
        }
    }
    path.display().to_string()
}

fn migrate_legacy_agent_defaults(path: &Path, raw: &str, config: &mut AppConfig) -> Result<()> {
    let mut new_codex = None;
    let mut new_claude = None;

    if config.do_config.agents.codex == LEGACY_CODEX_AGENT {
        let value = default_codex_agent();
        config.do_config.agents.codex = value.clone();
        new_codex = Some(value);
    }
    if config.do_config.agents.claude == LEGACY_CLAUDE_AGENT {
        let value = default_claude_agent();
        config.do_config.agents.claude = value.clone();
        new_claude = Some(value);
    }

    if new_codex.is_none() && new_claude.is_none() {
        return Ok(());
    }

    if let Some(updated) =
        rewrite_agent_defaults_in_toml(raw, new_codex.as_deref(), new_claude.as_deref())?
    {
        fs::write(path, updated)?;
    }

    Ok(())
}

fn rewrite_agent_defaults_in_toml(
    raw: &str,
    new_codex: Option<&str>,
    new_claude: Option<&str>,
) -> Result<Option<String>> {
    let mut in_agents = false;
    let mut changed = false;
    let mut lines: Vec<String> = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed == "[do.agents]" {
            in_agents = true;
            lines.push(line.to_string());
            continue;
        }
        if in_agents && trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_agents = false;
        }
        if in_agents {
            if let Some(value) = new_codex {
                if trimmed.starts_with("codex") {
                    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                    lines.push(format!("{indent}codex = \"{value}\""));
                    changed = true;
                    continue;
                }
            }
            if let Some(value) = new_claude {
                if trimmed.starts_with("claude") {
                    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                    lines.push(format!("{indent}claude = \"{value}\""));
                    changed = true;
                    continue;
                }
            }
        }
        lines.push(line.to_string());
    }

    if !changed {
        return Ok(None);
    }

    let mut result = lines.join("\n");
    if raw.ends_with('\n') {
        result.push('\n');
    }
    Ok(Some(result))
}

fn rewrite_stats_db_path_in_toml(raw: &str, new_value: &str) -> Result<Option<String>> {
    let mut in_stats = false;
    let mut changed = false;
    let mut lines: Vec<String> = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed == "[stats]" {
            in_stats = true;
            lines.push(line.to_string());
            continue;
        }
        if in_stats && trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_stats = false;
        }
        if in_stats && trimmed.starts_with("db_path") {
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            lines.push(format!("{indent}db_path = \"{new_value}\""));
            changed = true;
            continue;
        }
        lines.push(line.to_string());
    }

    if !changed {
        return Ok(None);
    }

    let mut result = lines.join("\n");
    if raw.ends_with('\n') {
        result.push('\n');
    }
    Ok(Some(result))
}

fn path_is_under(path: &Path, parent: &Path) -> bool {
    match (path.canonicalize(), parent.canonicalize()) {
        (Ok(path), Ok(parent)) => path.starts_with(parent),
        _ => path.starts_with(parent),
    }
}

pub fn config_file_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn cache_file_path() -> PathBuf {
    config_dir().join("projects-cache.json")
}

/// Expand a leading `~` to the **current user's** home directory. Only the bare
/// `~` / `~/…` form is expanded; `~user` (another user's home) is intentionally
/// left as a literal path. Resolving `~user` would require an `/etc/passwd`-style
/// lookup we deliberately don't do — use an absolute path for another user's home.
pub fn expand_path(value: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(value).to_string())
}

pub fn write_default_config_if_missing(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic::write_private(path, DEFAULT_CONFIG.as_bytes())?;
    Ok(true)
}

pub fn default_config_str() -> &'static str {
    DEFAULT_CONFIG
}

fn default_codex_agent() -> String {
    "codex --sandbox workspace-write --ask-for-approval on-request -c approvals_reviewer=auto_review exec".into()
}

fn default_claude_agent() -> String {
    "claude --permission-mode auto -p".into()
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            codex: default_codex_agent(),
            claude: default_claude_agent(),
        }
    }
}

fn apply_env_overrides(config: &mut AppConfig) -> Result<()> {
    if let Ok(value) = env::var("QR_DEFAULT_RUN_MODE") {
        config.general.default_run_mode = value;
    }
    if let Ok(value) = env::var("QR_PROJECT_ROOTS") {
        config.projects.roots = value
            .split(':')
            .filter(|part| !part.trim().is_empty())
            .map(ToOwned::to_owned)
            .collect();
    }
    if let Ok(value) = env::var("QR_SCAN_DEPTH") {
        config.projects.scan_depth = parse_env(&value, "QR_SCAN_DEPTH")?;
    }
    if let Ok(value) = env::var("QR_SCAN_INTERVAL_HOURS") {
        config.projects.scan_interval_hours = parse_env(&value, "QR_SCAN_INTERVAL_HOURS")?;
    }
    if let Ok(value) = env::var("QR_AI_PROTOCOL") {
        config.ai.protocol = value.parse().map_err(anyhow::Error::msg)?;
    }
    if let Ok(value) = env::var("QR_AI_BASE_URL") {
        config.ai.base_url = value;
    }
    if let Ok(value) = env::var("QR_AI_MODEL") {
        config.ai.model = value;
    }
    if let Ok(value) = env::var("QR_AI_API_KEY") {
        config.ai.api_key = value;
    }
    if let Ok(value) = env::var("QR_AI_API_KEY_ENV") {
        config.ai.api_key_env = value;
    }
    if let Ok(value) = env::var("QR_AI_FALLBACK_PROTOCOL") {
        fallback_config_mut(&mut config.ai).protocol = value.parse().map_err(anyhow::Error::msg)?;
    }
    if let Ok(value) = env::var("QR_AI_FALLBACK_BASE_URL") {
        fallback_config_mut(&mut config.ai).base_url = value;
    }
    if let Ok(value) = env::var("QR_AI_FALLBACK_MODEL") {
        fallback_config_mut(&mut config.ai).model = value;
    }
    if let Ok(value) = env::var("QR_AI_FALLBACK_API_KEY") {
        fallback_config_mut(&mut config.ai).api_key = value;
    }
    if let Ok(value) = env::var("QR_AI_FALLBACK_API_KEY_ENV") {
        fallback_config_mut(&mut config.ai).api_key_env = value;
    }
    if let Ok(value) = env::var("QR_STATS_ENABLED") {
        config.stats.enabled = parse_bool(&value)?;
    }
    if let Ok(value) = env::var("QR_STATS_DB_PATH") {
        config.stats.db_path = value;
    }
    Ok(())
}

fn fallback_config_mut(ai: &mut AiConfig) -> &mut FallbackAiConfig {
    ai.fallback.get_or_insert_with(|| FallbackAiConfig {
        protocol: ai.protocol,
        base_url: ai.base_url.clone(),
        model: ai.model.clone(),
        api_key: ai.api_key.clone(),
        api_key_env: ai.api_key_env.clone(),
    })
}

fn parse_env<T>(raw: &str, key: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    raw.parse()
        .map_err(|err| anyhow!("Invalid {key} value '{raw}': {err}"))
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(anyhow!("Invalid boolean value '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock;

    fn clear_test_env() {
        for key in [
            "QR_DEFAULT_RUN_MODE",
            "QR_PROJECT_ROOTS",
            "QR_SCAN_DEPTH",
            "QR_SCAN_INTERVAL_HOURS",
            "QR_AI_PROTOCOL",
            "QR_AI_BASE_URL",
            "QR_AI_MODEL",
            "QR_AI_API_KEY",
            "QR_AI_API_KEY_ENV",
            "QR_AI_FALLBACK_PROTOCOL",
            "QR_AI_FALLBACK_BASE_URL",
            "QR_AI_FALLBACK_MODEL",
            "QR_AI_FALLBACK_API_KEY",
            "QR_AI_FALLBACK_API_KEY_ENV",
            "QR_STATS_ENABLED",
            "QR_STATS_DB_PATH",
        ] {
            unsafe {
                env::remove_var(key);
            }
        }
    }

    #[test]
    fn load_from_str_parses_default_config() {
        let config = AppConfig::load_from_str(default_config_str()).unwrap();
        assert!(!config.projects.roots.is_empty());
        assert!(config.projects.scan_depth > 0);
    }

    #[test]
    fn config_uses_defaults_when_file_missing() {
        let _guard = test_env_lock().lock().unwrap();
        clear_test_env();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let config = AppConfig::load_from_env_with_path(path).unwrap();

        assert_eq!(config.general.default_run_mode, "output");
        assert_eq!(config.projects.scan_depth, 2);
        assert!(!config.stats.enabled);
    }

    #[test]
    fn env_vars_override_config_values() {
        let _guard = test_env_lock().lock().unwrap();
        clear_test_env();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[general]
default_run_mode = "log"
[projects]
roots = ["~/Code"]
scan_depth = 1
scan_interval_hours = 4
[ai]
protocol = "openai"
base_url = "https://primary"
model = "primary-model"
api_key = "primary-config-key"
api_key_env = "PRIMARY_KEY"
[ai.fallback]
protocol = "anthropic"
base_url = "https://fallback"
model = "fallback-model"
api_key = "fallback-config-key"
api_key_env = "FALLBACK_KEY"
[stats]
enabled = true
db_path = "/tmp/file.db"
"#,
        )
        .unwrap();

        unsafe {
            env::set_var("QR_DEFAULT_RUN_MODE", "watch");
            env::set_var("QR_PROJECT_ROOTS", "/one:/two");
            env::set_var("QR_SCAN_DEPTH", "3");
            env::set_var("QR_SCAN_INTERVAL_HOURS", "8");
            env::set_var("QR_AI_PROTOCOL", "anthropic");
            env::set_var("QR_AI_BASE_URL", "https://override-primary");
            env::set_var("QR_AI_MODEL", "override-primary-model");
            env::set_var("QR_AI_API_KEY", "override-primary-config-key");
            env::set_var("QR_AI_API_KEY_ENV", "OVERRIDE_PRIMARY_KEY");
            env::set_var("QR_AI_FALLBACK_PROTOCOL", "openai");
            env::set_var("QR_AI_FALLBACK_BASE_URL", "https://override-fallback");
            env::set_var("QR_AI_FALLBACK_MODEL", "override-fallback-model");
            env::set_var("QR_AI_FALLBACK_API_KEY", "override-fallback-config-key");
            env::set_var("QR_AI_FALLBACK_API_KEY_ENV", "OVERRIDE_FALLBACK_KEY");
            env::set_var("QR_STATS_ENABLED", "false");
            env::set_var("QR_STATS_DB_PATH", "/tmp/override.db");
        }

        let config = AppConfig::load_from_env_with_path(path).unwrap();

        assert_eq!(config.general.default_run_mode, "watch");
        assert_eq!(config.projects.roots, vec!["/one", "/two"]);
        assert_eq!(config.projects.scan_depth, 3);
        assert_eq!(config.projects.scan_interval_hours, 8);
        assert_eq!(config.ai.protocol, AiProtocol::Anthropic);
        assert_eq!(config.ai.base_url, "https://override-primary");
        assert_eq!(config.ai.model, "override-primary-model");
        assert_eq!(config.ai.api_key, "override-primary-config-key");
        assert_eq!(config.ai.api_key_env, "OVERRIDE_PRIMARY_KEY");
        let fallback = config.ai.fallback.as_ref().unwrap();
        assert_eq!(fallback.protocol, AiProtocol::OpenAi);
        assert_eq!(fallback.base_url, "https://override-fallback");
        assert_eq!(fallback.model, "override-fallback-model");
        assert_eq!(fallback.api_key, "override-fallback-config-key");
        assert_eq!(fallback.api_key_env, "OVERRIDE_FALLBACK_KEY");
        assert!(!config.stats.enabled);
        assert_eq!(config.stats.db_path, "/tmp/override.db");

        clear_test_env();
    }

    #[test]
    fn config_parses_when_ai_fallback_section_is_missing() {
        let _guard = test_env_lock().lock().unwrap();
        clear_test_env();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[general]
default_run_mode = "log"
[projects]
roots = ["~/Code"]
scan_depth = 1
scan_interval_hours = 4
[ai]
protocol = "openai"
base_url = "https://primary"
model = "primary-model"
api_key = "primary-config-key"
api_key_env = "PRIMARY_KEY"
[stats]
enabled = true
db_path = "/tmp/file.db"
"#,
        )
        .unwrap();

        let config = AppConfig::load_from_env_with_path(path).unwrap();

        assert_eq!(config.ai.protocol, AiProtocol::OpenAi);
        assert_eq!(config.ai.base_url, "https://primary");
        assert_eq!(config.ai.model, "primary-model");
        assert_eq!(config.ai.api_key, "primary-config-key");
        assert_eq!(config.ai.api_key_env, "PRIMARY_KEY");
        assert!(config.ai.fallback.is_none());
        clear_test_env();
    }

    #[test]
    fn expand_path_expands_current_user_tilde_only() {
        // `~/…` expands to the current home; `~user` is intentionally left literal,
        // and absolute paths pass through unchanged.
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_path("~/sub/dir"), home.join("sub/dir"));
        assert_eq!(expand_path("~someone/dir"), PathBuf::from("~someone/dir"));
        assert_eq!(expand_path("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn migrate_legacy_config_moves_files_when_new_dir_empty() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let new_dir = root.path().join("new");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("config.toml"), default_config_str()).unwrap();
        fs::write(legacy.join("projects-cache.json"), "[]").unwrap();

        let migrated = migrate_legacy_config(&new_dir, Some(&legacy)).unwrap();
        assert!(migrated);
        assert!(new_dir.join("config.toml").exists());
        assert!(new_dir.join("projects-cache.json").exists());
        assert!(new_dir.join(".migrated-from-legacy").exists());
        assert!(!legacy.join("config.toml").exists());
    }

    #[test]
    fn migrate_legacy_config_skips_when_already_migrated() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let new_dir = root.path().join("new");
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&new_dir).unwrap();
        fs::write(legacy.join("config.toml"), "legacy").unwrap();
        fs::write(new_dir.join(".migrated-from-legacy"), "done").unwrap();

        let migrated = migrate_legacy_config(&new_dir, Some(&legacy)).unwrap();
        assert!(!migrated);
        assert!(legacy.join("config.toml").exists());
    }

    #[test]
    fn migrate_legacy_config_rewrites_stats_db_path_under_legacy_dir() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let new_dir = root.path().join("new");
        fs::create_dir_all(&legacy).unwrap();
        let legacy_db = legacy.join("stats.db");
        fs::write(
            legacy.join("config.toml"),
            format!(
                r#"
[general]
default_run_mode = "output"
[projects]
roots = ["~/Code"]
scan_depth = 2
scan_interval_hours = 1
[ai]
protocol = "openai"
base_url = "https://api.openai.com/v1"
model = "gpt-4o"
api_key = ""
api_key_env = "OPENAI_API_KEY"
[stats]
enabled = true
db_path = "{}"
"#,
                legacy_db.display()
            ),
        )
        .unwrap();

        migrate_legacy_config(&new_dir, Some(&legacy)).unwrap();

        let config = AppConfig::load_from_env_with_path(new_dir.join("config.toml")).unwrap();
        assert_eq!(config.stats.db_path, "__default__");
    }

    #[test]
    fn migrate_legacy_agent_defaults_rewrites_shipped_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[general]
default_run_mode = "output"
[projects]
roots = ["~/Development"]
scan_depth = 2
scan_interval_hours = 1
[ai]
protocol = "openai"
base_url = "https://api.openai.com/v1"
model = "gpt-4o"
api_key = ""
api_key_env = "OPENAI_API_KEY"
[stats]
enabled = false
db_path = "__default__"
[do.agents]
codex = "codex exec"
claude = "claude --dangerously-skip-permissions -p"
"#,
        )
        .unwrap();

        let config = AppConfig::load_from_env_with_path(path.clone()).unwrap();

        assert_eq!(config.do_config.agents.codex, default_codex_agent());
        assert_eq!(config.do_config.agents.claude, default_claude_agent());

        let updated = fs::read_to_string(&path).unwrap();
        assert!(updated.contains(&format!("codex = \"{}\"", default_codex_agent())));
        assert!(updated.contains(&format!("claude = \"{}\"", default_claude_agent())));
        assert!(!updated.contains(LEGACY_CODEX_AGENT));
        assert!(!updated.contains(LEGACY_CLAUDE_AGENT));
    }

    #[test]
    fn migrate_legacy_agent_defaults_preserves_custom_agent_commands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let custom_codex = "codex exec --model gpt-5";
        fs::write(
            &path,
            format!(
                r#"
[general]
default_run_mode = "output"
[projects]
roots = ["~/Development"]
scan_depth = 2
scan_interval_hours = 1
[ai]
protocol = "openai"
base_url = "https://api.openai.com/v1"
model = "gpt-4o"
api_key = ""
api_key_env = "OPENAI_API_KEY"
[stats]
enabled = false
db_path = "__default__"
[do.agents]
codex = "{custom_codex}"
claude = "claude --dangerously-skip-permissions -p"
"#
            ),
        )
        .unwrap();

        let config = AppConfig::load_from_env_with_path(path.clone()).unwrap();

        assert_eq!(config.do_config.agents.codex, custom_codex);
        assert_eq!(config.do_config.agents.claude, default_claude_agent());

        let updated = fs::read_to_string(&path).unwrap();
        assert!(updated.contains(&format!("codex = \"{custom_codex}\"")));
    }

    #[test]
    fn rewrite_stats_db_path_preserves_comments_and_formatting() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let config_path = root.path().join("config.toml");
        let legacy_db = legacy.join("stats.db");
        fs::write(
            &config_path,
            format!(
                r#"# my custom config
[general]
default_run_mode = "output"
[projects]
roots = ["~/Code"]
scan_depth = 2
scan_interval_hours = 1
[ai]
protocol = "openai"
base_url = "https://api.openai.com/v1"
model = "gpt-4o"
api_key = ""
api_key_env = "OPENAI_API_KEY"
[stats]
enabled = true
# keep stats near the legacy db
db_path = "{}"
"#,
                legacy_db.display()
            ),
        )
        .unwrap();

        rewrite_legacy_paths_in_config(&config_path, &legacy, root.path()).unwrap();

        let updated = fs::read_to_string(&config_path).unwrap();
        assert!(updated.contains("# my custom config"));
        assert!(updated.contains("# keep stats near the legacy db"));
        assert!(updated.contains("db_path = \"__default__\""));
        assert!(!updated.contains(&legacy_db.display().to_string()));
    }

    #[test]
    fn migrate_legacy_config_preserves_custom_stats_db_filename() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let new_dir = root.path().join("new");
        fs::create_dir_all(&legacy).unwrap();
        let custom_db = legacy.join("work-stats.db");
        fs::write(&custom_db, b"sqlite").unwrap();
        fs::write(
            legacy.join("config.toml"),
            format!(
                r#"
[general]
default_run_mode = "output"
[projects]
roots = ["~/Code"]
scan_depth = 2
scan_interval_hours = 1
[ai]
protocol = "openai"
base_url = "https://api.openai.com/v1"
model = "gpt-4o"
api_key = ""
api_key_env = "OPENAI_API_KEY"
[stats]
enabled = true
db_path = "{}"
"#,
                custom_db.display()
            ),
        )
        .unwrap();

        migrate_legacy_config(&new_dir, Some(&legacy)).unwrap();

        assert!(new_dir.join("work-stats.db").exists());
        let config = AppConfig::load_from_env_with_path(new_dir.join("config.toml")).unwrap();
        assert_eq!(
            expand_path(&config.stats.db_path),
            new_dir.join("work-stats.db")
        );
    }

    #[cfg(unix)]
    #[test]
    fn migrate_legacy_config_moves_symlinked_config() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let new_dir = root.path().join("new");
        let real_config = root.path().join("real-config.toml");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(&real_config, default_config_str()).unwrap();
        symlink(&real_config, legacy.join("config.toml")).unwrap();

        let migrated = migrate_legacy_config(&new_dir, Some(&legacy)).unwrap();
        assert!(migrated);
        assert!(new_dir.join("config.toml").exists());
        assert!(!legacy.join("config.toml").exists());
    }
}
