use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use toml::Value as TomlValue;

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    ".qr.toml",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectProfile {
    pub name: String,
    pub language: Option<String>,
    pub framework: Option<String>,
    pub package_manager: Option<String>,
    pub test_command: Option<String>,
    pub build_command: Option<String>,
    pub lint_command: Option<String>,
    #[serde(default)]
    pub scripts: BTreeMap<String, String>,
    pub prefer_agent: Option<String>,
    #[serde(default)]
    pub entry_points: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LearnResult {
    pub project_root: PathBuf,
    pub profile_path: PathBuf,
    pub profile: ProjectProfile,
}

#[derive(Debug, Default, Deserialize)]
struct ProjectProfileOverride {
    name: Option<String>,
    language: Option<String>,
    framework: Option<String>,
    package_manager: Option<String>,
    test_command: Option<String>,
    build_command: Option<String>,
    lint_command: Option<String>,
    prefer_agent: Option<String>,
    #[serde(default)]
    scripts: BTreeMap<String, String>,
}

pub fn learn_current_dir() -> Result<LearnResult> {
    let cwd = env::current_dir().context("Failed to resolve current directory")?;
    let project_root = discover_project_root(&cwd);
    let mut profile = detect_profile(&project_root)?;
    apply_overrides(&project_root, &mut profile)?;

    let profile_dir = project_root.join(".qr");
    fs::create_dir_all(&profile_dir)
        .with_context(|| format!("Failed to create {}", profile_dir.display()))?;
    let profile_path = profile_dir.join("profile.json");
    fs::write(&profile_path, serde_json::to_vec_pretty(&profile)?)
        .with_context(|| format!("Failed to write {}", profile_path.display()))?;

    Ok(LearnResult {
        project_root,
        profile_path,
        profile,
    })
}

pub fn load_profile_from(root: &Path) -> Result<ProjectProfile> {
    let path = root.join(".qr/profile.json");
    let raw = fs::read(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_slice(&raw).context("Failed to parse project profile")
}

pub fn discover_project_root(start: &Path) -> PathBuf {
    for candidate in start.ancestors() {
        if PROJECT_MARKERS
            .iter()
            .any(|marker| candidate.join(marker).exists())
        {
            return candidate.to_path_buf();
        }
    }
    start.to_path_buf()
}

pub fn detect_profile(root: &Path) -> Result<ProjectProfile> {
    if root.join("package.json").is_file() {
        return detect_node_profile(root);
    }
    if root.join("Cargo.toml").is_file() {
        return detect_rust_profile(root);
    }
    if root.join("pyproject.toml").is_file() {
        return detect_python_profile(root);
    }
    if root.join("go.mod").is_file() {
        return detect_go_profile(root);
    }

    Ok(ProjectProfile {
        name: fallback_name(root),
        language: None,
        framework: None,
        package_manager: None,
        test_command: None,
        build_command: None,
        lint_command: None,
        scripts: BTreeMap::new(),
        prefer_agent: None,
        entry_points: detect_common_entry_points(root),
    })
}

fn detect_node_profile(root: &Path) -> Result<ProjectProfile> {
    let raw = fs::read_to_string(root.join("package.json"))
        .with_context(|| format!("Failed to read {}", root.join("package.json").display()))?;
    let json: JsonValue = serde_json::from_str(&raw).context("Failed to parse package.json")?;
    let name = json
        .get("name")
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback_name(root));
    let scripts = json
        .get("scripts")
        .and_then(JsonValue::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let package_manager = detect_package_manager(&json, root);
    let framework = detect_node_framework(&json);
    let test_script = scripts.get("test").cloned();
    let build_script = scripts.get("build").cloned();
    let lint_script = scripts.get("lint").cloned();

    Ok(ProjectProfile {
        name,
        language: Some("typescript".into()),
        framework,
        package_manager: package_manager.clone(),
        test_command: qualify_script_command(
            package_manager.as_deref(),
            "test",
            test_script.as_deref(),
        ),
        build_command: qualify_script_command(
            package_manager.as_deref(),
            "build",
            build_script.as_deref(),
        ),
        lint_command: qualify_script_command(
            package_manager.as_deref(),
            "lint",
            lint_script.as_deref(),
        ),
        scripts,
        prefer_agent: None,
        entry_points: detect_node_entry_points(root),
    })
}

fn detect_rust_profile(root: &Path) -> Result<ProjectProfile> {
    let raw = fs::read_to_string(root.join("Cargo.toml"))
        .with_context(|| format!("Failed to read {}", root.join("Cargo.toml").display()))?;
    let toml: TomlValue = toml::from_str(&raw).context("Failed to parse Cargo.toml")?;
    let name = toml
        .get("package")
        .and_then(TomlValue::as_table)
        .and_then(|pkg| pkg.get("name"))
        .and_then(TomlValue::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback_name(root));

    let mut scripts = BTreeMap::new();
    scripts.insert("build".into(), "cargo build".into());
    scripts.insert("test".into(), "cargo test".into());
    scripts.insert("lint".into(), "cargo clippy".into());

    Ok(ProjectProfile {
        name,
        language: Some("rust".into()),
        framework: None,
        package_manager: Some("cargo".into()),
        test_command: Some("cargo test".into()),
        build_command: Some("cargo build".into()),
        lint_command: Some("cargo clippy".into()),
        scripts,
        prefer_agent: None,
        entry_points: detect_rust_entry_points(root),
    })
}

fn detect_python_profile(root: &Path) -> Result<ProjectProfile> {
    let raw = fs::read_to_string(root.join("pyproject.toml"))
        .with_context(|| format!("Failed to read {}", root.join("pyproject.toml").display()))?;
    let toml: TomlValue = toml::from_str(&raw).context("Failed to parse pyproject.toml")?;
    let name = toml
        .get("project")
        .and_then(TomlValue::as_table)
        .and_then(|project| project.get("name"))
        .and_then(TomlValue::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback_name(root));

    let framework = if root.join("manage.py").is_file() {
        Some("django".into())
    } else if root.join("app.py").is_file() || root.join("main.py").is_file() {
        Some("fastapi".into())
    } else {
        None
    };

    let mut scripts = BTreeMap::new();
    scripts.insert("test".into(), "pytest".into());
    scripts.insert("build".into(), "python -m build".into());

    Ok(ProjectProfile {
        name,
        language: Some("python".into()),
        framework,
        package_manager: Some("pip".into()),
        test_command: Some("pytest".into()),
        build_command: Some("python -m build".into()),
        lint_command: None,
        scripts,
        prefer_agent: None,
        entry_points: detect_common_entry_points(root),
    })
}

fn detect_go_profile(root: &Path) -> Result<ProjectProfile> {
    let raw = fs::read_to_string(root.join("go.mod"))
        .with_context(|| format!("Failed to read {}", root.join("go.mod").display()))?;
    let name = raw
        .lines()
        .find_map(|line| line.trim().strip_prefix("module "))
        .and_then(|module| module.rsplit('/').next())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback_name(root));

    let mut scripts = BTreeMap::new();
    scripts.insert("build".into(), "go build ./...".into());
    scripts.insert("test".into(), "go test ./...".into());

    Ok(ProjectProfile {
        name,
        language: Some("go".into()),
        framework: None,
        package_manager: Some("go".into()),
        test_command: Some("go test ./...".into()),
        build_command: Some("go build ./...".into()),
        lint_command: None,
        scripts,
        prefer_agent: None,
        entry_points: detect_go_entry_points(root),
    })
}

fn apply_overrides(root: &Path, profile: &mut ProjectProfile) -> Result<()> {
    let path = root.join(".qr.toml");
    if !path.is_file() {
        return Ok(());
    }

    let raw =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    let overrides: ProjectProfileOverride =
        toml::from_str(&raw).context("Failed to parse .qr.toml")?;

    if let Some(value) = overrides.name {
        profile.name = value;
    }
    if let Some(value) = overrides.language {
        profile.language = Some(value);
    }
    if let Some(value) = overrides.framework {
        profile.framework = Some(value);
    }
    if let Some(value) = overrides.package_manager {
        profile.package_manager = Some(value);
    }
    if let Some(value) = overrides.test_command {
        profile.test_command = Some(value);
    }
    if let Some(value) = overrides.build_command {
        profile.build_command = Some(value);
    }
    if let Some(value) = overrides.lint_command {
        profile.lint_command = Some(value);
    }
    if let Some(value) = overrides.prefer_agent {
        profile.prefer_agent = Some(value);
    }
    for (key, value) in overrides.scripts {
        profile.scripts.insert(key, value);
    }

    Ok(())
}

fn fallback_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| root.display().to_string())
}

fn detect_package_manager(json: &JsonValue, root: &Path) -> Option<String> {
    let explicit = json
        .get("packageManager")
        .and_then(JsonValue::as_str)
        .map(|value| {
            value
                .split('@')
                .next()
                .unwrap_or(value)
                .to_ascii_lowercase()
        });
    if explicit.is_some() {
        return explicit;
    }
    if root.join("pnpm-lock.yaml").is_file() {
        return Some("pnpm".into());
    }
    if root.join("yarn.lock").is_file() {
        return Some("yarn".into());
    }
    if root.join("package-lock.json").is_file() {
        return Some("npm".into());
    }
    Some("npm".into())
}

fn detect_node_framework(json: &JsonValue) -> Option<String> {
    let deps = json
        .get("dependencies")
        .and_then(JsonValue::as_object)
        .into_iter()
        .flat_map(|map| map.keys())
        .chain(
            json.get("devDependencies")
                .and_then(JsonValue::as_object)
                .into_iter()
                .flat_map(|map| map.keys()),
        )
        .map(|key| key.as_str())
        .collect::<Vec<_>>();

    if deps.contains(&"next") {
        Some("nextjs".into())
    } else if deps.contains(&"vite") {
        Some("vite".into())
    } else if deps.contains(&"react") {
        Some("react".into())
    } else {
        None
    }
}

fn qualify_script_command(
    package_manager: Option<&str>,
    script_name: &str,
    script_body: Option<&str>,
) -> Option<String> {
    let _ = script_body?;
    match package_manager.unwrap_or("npm") {
        "pnpm" => Some(format!("pnpm {script_name}")),
        "yarn" => Some(format!("yarn {script_name}")),
        _ => Some(format!("npm run {script_name}"))
            .filter(|_| script_name != "test")
            .or_else(|| Some("npm test".into())),
    }
}

fn detect_node_entry_points(root: &Path) -> Vec<String> {
    [
        "src/app/",
        "src/pages/",
        "src/server/",
        "app/",
        "server/",
        "index.ts",
        "index.js",
    ]
    .into_iter()
    .filter(|entry| root.join(entry.trim_end_matches('/')).exists())
    .map(ToOwned::to_owned)
    .collect()
}

fn detect_rust_entry_points(root: &Path) -> Vec<String> {
    ["src/main.rs", "src/lib.rs", "src/bin/"]
        .into_iter()
        .filter(|entry| root.join(entry.trim_end_matches('/')).exists())
        .map(ToOwned::to_owned)
        .collect()
}

fn detect_go_entry_points(root: &Path) -> Vec<String> {
    ["main.go", "cmd/", "internal/"]
        .into_iter()
        .filter(|entry| root.join(entry.trim_end_matches('/')).exists())
        .map(ToOwned::to_owned)
        .collect()
}

fn detect_common_entry_points(root: &Path) -> Vec<String> {
    ["src/", "tests/", "docs/"]
        .into_iter()
        .filter(|entry| root.join(entry.trim_end_matches('/')).exists())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_node_profile_from_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{
  "name": "demo-app",
  "packageManager": "pnpm@9.0.0",
  "scripts": {
    "build": "next build",
    "test": "vitest run",
    "lint": "eslint ."
  },
  "dependencies": {
    "next": "15.0.0"
  }
}"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src/app")).unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert_eq!(profile.name, "demo-app");
        assert_eq!(profile.language.as_deref(), Some("typescript"));
        assert_eq!(profile.framework.as_deref(), Some("nextjs"));
        assert_eq!(profile.package_manager.as_deref(), Some("pnpm"));
        assert_eq!(profile.test_command.as_deref(), Some("pnpm test"));
        assert!(profile.entry_points.contains(&"src/app/".to_string()));
    }

    #[test]
    fn qr_toml_overrides_detected_values() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "demo-rs"
version = "0.1.0"
edition = "2024"
"#,
        )
        .unwrap();
        fs::write(
            tmp.path().join(".qr.toml"),
            r#"framework = "axum"
prefer_agent = "claude"
test_command = "cargo nextest run"

[scripts]
dev = "cargo run"
"#,
        )
        .unwrap();

        let mut profile = detect_profile(tmp.path()).unwrap();
        apply_overrides(tmp.path(), &mut profile).unwrap();

        assert_eq!(profile.framework.as_deref(), Some("axum"));
        assert_eq!(profile.prefer_agent.as_deref(), Some("claude"));
        assert_eq!(profile.test_command.as_deref(), Some("cargo nextest run"));
        assert_eq!(
            profile.scripts.get("dev").map(String::as_str),
            Some("cargo run")
        );
    }
}
