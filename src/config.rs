use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::ai::providers::{AiProtocol, ProviderConfig};

const DEFAULT_CONFIG: &str = include_str!("../config/default.toml");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub general: GeneralConfig,
    pub projects: ProjectsConfig,
    pub ai: AiConfig,
    pub stats: StatsConfig,
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
    pub api_key_env: String,
    #[serde(default)]
    pub fallback: Option<FallbackAiConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackAiConfig {
    pub protocol: AiProtocol,
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsConfig {
    pub enabled: bool,
    pub db_path: String,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        Self::load_from_env_with_path(config_file_path())
    }

    pub fn load_from_str(raw: &str) -> Result<Self> {
        toml::from_str(raw).context("Failed to parse config.toml")
    }

    pub fn load_from_env_with_path(path: PathBuf) -> Result<Self> {
        let mut config = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config file {}", path.display()))?;
            Self::load_from_str(&raw)?
        } else {
            Self::load_from_str(DEFAULT_CONFIG)?
        };

        apply_env_overrides(&mut config)?;
        Ok(config)
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
        expand_path(&self.stats.db_path)
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
            api_key_env: self.ai.api_key_env.clone(),
        }
    }

    pub fn ai_fallback_provider(&self) -> Option<ProviderConfig> {
        self.ai.fallback.as_ref().map(|fallback| ProviderConfig {
            protocol: fallback.protocol,
            base_url: fallback.base_url.clone(),
            model: fallback.model.clone(),
            api_key_env: fallback.api_key_env.clone(),
        })
    }
}

pub fn config_dir() -> PathBuf {
    if let Some(base) = dirs::config_dir() {
        return base.join("qr");
    }
    PathBuf::from(".config/qr")
}

pub fn config_file_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn cache_file_path() -> PathBuf {
    config_dir().join("projects-cache.json")
}

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
    fs::write(path, DEFAULT_CONFIG)?;
    Ok(true)
}

pub fn default_config_str() -> &'static str {
    DEFAULT_CONFIG
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
    use std::sync::{Mutex, OnceLock};

    use super::*;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn clear_test_env() {
        for key in [
            "QR_DEFAULT_RUN_MODE",
            "QR_PROJECT_ROOTS",
            "QR_SCAN_DEPTH",
            "QR_SCAN_INTERVAL_HOURS",
            "QR_AI_PROTOCOL",
            "QR_AI_BASE_URL",
            "QR_AI_MODEL",
            "QR_AI_API_KEY_ENV",
            "QR_AI_FALLBACK_PROTOCOL",
            "QR_AI_FALLBACK_BASE_URL",
            "QR_AI_FALLBACK_MODEL",
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
        let _guard = env_lock().lock().unwrap();
        clear_test_env();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let config = AppConfig::load_from_env_with_path(path).unwrap();

        assert_eq!(config.general.default_run_mode, "output");
        assert_eq!(config.projects.scan_depth, 2);
        assert!(config.stats.enabled);
    }

    #[test]
    fn env_vars_override_config_values() {
        let _guard = env_lock().lock().unwrap();
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
api_key_env = "PRIMARY_KEY"
[ai.fallback]
protocol = "anthropic"
base_url = "https://fallback"
model = "fallback-model"
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
            env::set_var("QR_AI_API_KEY_ENV", "OVERRIDE_PRIMARY_KEY");
            env::set_var("QR_AI_FALLBACK_PROTOCOL", "openai");
            env::set_var("QR_AI_FALLBACK_BASE_URL", "https://override-fallback");
            env::set_var("QR_AI_FALLBACK_MODEL", "override-fallback-model");
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
        assert_eq!(config.ai.api_key_env, "OVERRIDE_PRIMARY_KEY");
        let fallback = config.ai.fallback.as_ref().unwrap();
        assert_eq!(fallback.protocol, AiProtocol::OpenAi);
        assert_eq!(fallback.base_url, "https://override-fallback");
        assert_eq!(fallback.model, "override-fallback-model");
        assert_eq!(fallback.api_key_env, "OVERRIDE_FALLBACK_KEY");
        assert!(!config.stats.enabled);
        assert_eq!(config.stats.db_path, "/tmp/override.db");

        clear_test_env();
    }

    #[test]
    fn config_parses_when_ai_fallback_section_is_missing() {
        let _guard = env_lock().lock().unwrap();
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
        assert_eq!(config.ai.api_key_env, "PRIMARY_KEY");
        assert!(config.ai.fallback.is_none());
        clear_test_env();
    }
}
