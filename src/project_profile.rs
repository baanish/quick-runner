use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use toml::Value as TomlValue;

use crate::agent_history::{self, MinedCommand};

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    ".qr.toml",
];

/// Common role keys that `qr learn` always tries to populate when the project
/// type supports them. Manifest scripts and `.qr.toml` overrides win over the
/// hardcoded language defaults.
const COMMON_SCRIPT_ROLES: &[&str] = &[
    "build",
    "test",
    "lint",
    "fmt",
    "format",
    "typecheck",
    "dev",
    "start",
    "run",
    "debug",
    "clean",
    "release",
    "check",
    "doc",
    "bench",
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
    /// Dev server / watch-mode entrypoint (e.g. `pnpm dev`, `cargo run`).
    #[serde(default)]
    pub dev_command: Option<String>,
    /// One-shot run of the app (e.g. `pnpm start`, `go run .`).
    #[serde(default)]
    pub run_command: Option<String>,
    /// Debug-oriented run (e.g. `RUST_BACKTRACE=1 cargo run`).
    #[serde(default)]
    pub debug_command: Option<String>,
    #[serde(default)]
    pub scripts: BTreeMap<String, String>,
    pub prefer_agent: Option<String>,
    #[serde(default)]
    pub entry_points: Vec<String>,
    /// Commands mined from coding-agent session histories (opt-in via config).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_commands: Vec<MinedCommand>,
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
    dev_command: Option<String>,
    run_command: Option<String>,
    debug_command: Option<String>,
    prefer_agent: Option<String>,
    #[serde(default)]
    scripts: BTreeMap<String, String>,
    entry_points: Option<Vec<String>>,
}

pub fn learn_current_dir() -> Result<LearnResult> {
    let cwd = env::current_dir().context("Failed to resolve current directory")?;
    let project_root = discover_project_root(&cwd);
    let mut profile = detect_profile(&project_root)?;
    apply_overrides(&project_root, &mut profile)?;

    // Opt-in: mine bash/exec-like commands from coding agent session histories
    // for this project folder. Default off; enabled via `[learn].mine_agent_history`
    // or `QR_LEARN_MINE_AGENT_HISTORY`. Failures are swallowed so learn still
    // succeeds when agent stores are missing or unreadable.
    if mine_agent_history_enabled() {
        let mined = agent_history::mine_for_project(&project_root);
        agent_history::merge_mined_into_profile(&mut profile, mined);
    }

    let profile_dir = project_root.join(".qr");
    fs::create_dir_all(&profile_dir)
        .with_context(|| format!("Failed to create {}", profile_dir.display()))?;
    let profile_path = profile_dir.join("profile.json");
    // Atomic write: `qr do` may read profile.json (via load_profile_from) while a
    // concurrent `qr learn` rewrites it; a truncate-then-write would let the
    // reader observe a partial/garbage profile.
    crate::atomic::write(&profile_path, &serde_json::to_vec_pretty(&profile)?)
        .with_context(|| format!("Failed to write {}", profile_path.display()))?;

    Ok(LearnResult {
        project_root,
        profile_path,
        profile,
    })
}

/// Whether `qr learn` should scan coding-agent session histories.
///
/// Resolution order: `QR_LEARN_MINE_AGENT_HISTORY` env override, then
/// `config.toml` `[learn].mine_agent_history`, defaulting to `false` when the
/// config is missing or unreadable (learn stays usable without init).
pub fn mine_agent_history_enabled() -> bool {
    if let Ok(value) = env::var("QR_LEARN_MINE_AGENT_HISTORY") {
        return matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }
    crate::config::AppConfig::load()
        .map(|cfg| cfg.learn.mine_agent_history)
        .unwrap_or(false)
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
    let mut profile = if root.join("package.json").is_file() {
        detect_node_profile(root)?
    } else if root.join("Cargo.toml").is_file() {
        detect_rust_profile(root)?
    } else if root.join("pyproject.toml").is_file() {
        detect_python_profile(root)?
    } else if root.join("go.mod").is_file() {
        detect_go_profile(root)?
    } else {
        ProjectProfile {
            name: fallback_name(root),
            language: None,
            framework: None,
            package_manager: None,
            test_command: None,
            build_command: None,
            lint_command: None,
            dev_command: None,
            run_command: None,
            debug_command: None,
            scripts: BTreeMap::new(),
            prefer_agent: None,
            entry_points: detect_entry_points(root, &["src/", "tests/", "docs/"]),
            agent_commands: Vec::new(),
        }
    };

    // Makefile / Justfile targets supplement scripts for any project type
    // without overwriting keys already filled from the language manifest.
    merge_makefile_targets(root, &mut profile.scripts);
    merge_justfile_recipes(root, &mut profile.scripts);
    fill_role_commands_from_scripts(&mut profile);

    Ok(profile)
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
    // Keys that exist in package.json may be invoked via the package manager
    // (`pnpm test`). Framework-invented fallbacks are stored as direct tool
    // invocations (`next dev`) — inventing `pnpm <missing-script>` would fail.
    let package_scripts = json
        .get("scripts")
        .and_then(JsonValue::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut scripts = package_scripts.clone();
    let package_manager = detect_package_manager(&json, root);
    let framework = detect_node_framework(&json);

    insert_node_framework_defaults(&mut scripts, framework.as_deref());

    let pm = package_manager.as_deref();
    let role = |name: &str| -> Option<String> {
        if package_scripts.contains_key(name) {
            script_or_qualified(pm, &scripts, name)
        } else {
            // Invented default: use the direct tool body, not PM-qualified.
            scripts.get(name).cloned()
        }
    };
    let test_command = role("test");
    let build_command = role("build");
    let lint_command = role("lint").or_else(|| role("typecheck"));
    let dev_command = role("dev").or_else(|| role("start"));
    let run_command = role("start").or_else(|| role("run"));
    let debug_command = role("debug");

    Ok(ProjectProfile {
        name,
        language: Some("typescript".into()),
        framework,
        package_manager,
        test_command,
        build_command,
        lint_command,
        dev_command,
        run_command,
        debug_command,
        scripts,
        prefer_agent: None,
        entry_points: detect_entry_points(
            root,
            &[
                "src/app/",
                "src/pages/",
                "src/server/",
                "app/",
                "server/",
                "index.ts",
                "index.js",
            ],
        ),
        agent_commands: Vec::new(),
    })
}

/// Insert framework-known script bodies only when the key is absent. Bodies are
/// the raw script content (not package-manager-qualified) so they match the
/// rest of the Node `scripts` map.
fn insert_node_framework_defaults(scripts: &mut BTreeMap<String, String>, framework: Option<&str>) {
    match framework {
        Some("nextjs") => {
            insert_default(scripts, "dev", "next dev");
            insert_default(scripts, "build", "next build");
            insert_default(scripts, "start", "next start");
            insert_default(scripts, "lint", "next lint");
        }
        Some("vite") => {
            insert_default(scripts, "dev", "vite");
            insert_default(scripts, "build", "vite build");
            insert_default(scripts, "start", "vite preview");
        }
        _ => {}
    }
}

fn detect_rust_profile(root: &Path) -> Result<ProjectProfile> {
    let raw = fs::read_to_string(root.join("Cargo.toml"))
        .with_context(|| format!("Failed to read {}", root.join("Cargo.toml").display()))?;
    let toml: TomlValue = toml::from_str(&raw).context("Failed to parse Cargo.toml")?;
    let name = toml_name(&toml, "package", root);

    // Hardcoded Cargo defaults for the common developer loop. `.qr.toml` and
    // Makefile/Justfile merges can override any of these later.
    let mut scripts = BTreeMap::new();
    scripts.insert("build".into(), "cargo build".into());
    scripts.insert("test".into(), "cargo test".into());
    scripts.insert("lint".into(), "cargo clippy".into());
    scripts.insert("fmt".into(), "cargo fmt".into());
    scripts.insert("check".into(), "cargo check".into());
    scripts.insert("run".into(), "cargo run".into());
    scripts.insert("dev".into(), "cargo run".into());
    scripts.insert("debug".into(), "RUST_BACKTRACE=1 cargo run".into());
    scripts.insert("release".into(), "cargo build --release".into());
    scripts.insert("clean".into(), "cargo clean".into());
    scripts.insert("doc".into(), "cargo doc".into());
    scripts.insert("bench".into(), "cargo bench".into());

    Ok(ProjectProfile {
        name,
        language: Some("rust".into()),
        framework: None,
        package_manager: Some("cargo".into()),
        test_command: Some("cargo test".into()),
        build_command: Some("cargo build".into()),
        lint_command: Some("cargo clippy".into()),
        dev_command: Some("cargo run".into()),
        run_command: Some("cargo run".into()),
        debug_command: Some("RUST_BACKTRACE=1 cargo run".into()),
        scripts,
        prefer_agent: None,
        entry_points: detect_entry_points(root, &["src/main.rs", "src/lib.rs", "src/bin/"]),
        agent_commands: Vec::new(),
    })
}

fn detect_python_profile(root: &Path) -> Result<ProjectProfile> {
    let raw = fs::read_to_string(root.join("pyproject.toml"))
        .with_context(|| format!("Failed to read {}", root.join("pyproject.toml").display()))?;
    let toml: TomlValue = toml::from_str(&raw).context("Failed to parse pyproject.toml")?;
    let name = toml_name(&toml, "project", root);
    let package_manager = detect_python_package_manager(root);

    let framework = if root.join("manage.py").is_file() {
        Some("django".into())
    } else if root.join("app.py").is_file() || root.join("main.py").is_file() {
        // Heuristic: app.py/main.py often means FastAPI/Flask; prefer fastapi
        // when the name appears in deps, otherwise leave as fastapi only when
        // app.py exists (historical behavior) else None.
        if python_dep_mentions(&toml, "fastapi") || root.join("app.py").is_file() {
            Some("fastapi".into())
        } else if python_dep_mentions(&toml, "flask") {
            Some("flask".into())
        } else {
            Some("fastapi".into())
        }
    } else if python_dep_mentions(&toml, "django") {
        Some("django".into())
    } else if python_dep_mentions(&toml, "fastapi") {
        Some("fastapi".into())
    } else if python_dep_mentions(&toml, "flask") {
        Some("flask".into())
    } else {
        None
    };

    let mut scripts = BTreeMap::new();
    scripts.insert("test".into(), "pytest".into());
    scripts.insert("build".into(), "python -m build".into());
    scripts.insert("lint".into(), "ruff check .".into());
    scripts.insert("fmt".into(), "ruff format .".into());
    scripts.insert("format".into(), "ruff format .".into());
    scripts.insert("clean".into(), "rm -rf dist build *.egg-info".into());

    let (dev_command, run_command, debug_command) = match framework.as_deref() {
        Some("django") => {
            scripts.insert("dev".into(), "python manage.py runserver".into());
            scripts.insert("run".into(), "python manage.py runserver".into());
            scripts.insert("debug".into(), "python -X dev manage.py runserver".into());
            (
                Some("python manage.py runserver".into()),
                Some("python manage.py runserver".into()),
                Some("python -X dev manage.py runserver".into()),
            )
        }
        Some("fastapi") => {
            let app = if root.join("main.py").is_file() {
                "main:app"
            } else {
                "app:app"
            };
            let dev = format!("uvicorn {app} --reload");
            let run = format!("uvicorn {app}");
            let debug = format!("uvicorn {app} --reload --log-level debug");
            scripts.insert("dev".into(), dev.clone());
            scripts.insert("run".into(), run.clone());
            scripts.insert("debug".into(), debug.clone());
            (Some(dev), Some(run), Some(debug))
        }
        Some("flask") => {
            scripts.insert("dev".into(), "flask run --debug".into());
            scripts.insert("run".into(), "flask run".into());
            scripts.insert("debug".into(), "flask run --debug".into());
            (
                Some("flask run --debug".into()),
                Some("flask run".into()),
                Some("flask run --debug".into()),
            )
        }
        _ => {
            if root.join("main.py").is_file() {
                scripts.insert("run".into(), "python main.py".into());
                scripts.insert("dev".into(), "python main.py".into());
                scripts.insert("debug".into(), "python -X dev main.py".into());
                (
                    Some("python main.py".into()),
                    Some("python main.py".into()),
                    Some("python -X dev main.py".into()),
                )
            } else {
                (None, None, None)
            }
        }
    };

    // Prefer `uv run` / `poetry run` prefixes when that PM is detected so the
    // learned commands match how the project is actually invoked.
    let prefix = match package_manager.as_deref() {
        Some("uv") => Some("uv run"),
        Some("poetry") => Some("poetry run"),
        Some("pdm") => Some("pdm run"),
        _ => None,
    };
    if let Some(prefix) = prefix {
        for key in ["test", "lint", "fmt", "format", "dev", "run", "debug"] {
            if let Some(cmd) = scripts.get(key).cloned() {
                if !cmd.starts_with(prefix) {
                    scripts.insert(key.into(), format!("{prefix} {cmd}"));
                }
            }
        }
    }

    let qualify = |cmd: Option<String>| -> Option<String> {
        match (prefix, cmd) {
            (Some(prefix), Some(cmd)) if !cmd.starts_with(prefix) => {
                Some(format!("{prefix} {cmd}"))
            }
            (_, cmd) => cmd,
        }
    };

    Ok(ProjectProfile {
        name,
        language: Some("python".into()),
        framework,
        package_manager,
        test_command: qualify(Some("pytest".into())),
        build_command: Some("python -m build".into()),
        lint_command: qualify(Some("ruff check .".into())),
        dev_command: qualify(dev_command),
        run_command: qualify(run_command),
        debug_command: qualify(debug_command),
        scripts,
        prefer_agent: None,
        entry_points: detect_entry_points(root, &["src/", "tests/", "docs/", "main.py", "app.py"]),
        agent_commands: Vec::new(),
    })
}

fn detect_go_profile(root: &Path) -> Result<ProjectProfile> {
    let raw = fs::read_to_string(root.join("go.mod"))
        .with_context(|| format!("Failed to read {}", root.join("go.mod").display()))?;
    let name = raw
        .lines()
        .find_map(|line| line.trim().strip_prefix("module "))
        .map(|module| module.trim().trim_end_matches('/'))
        .and_then(|module| module.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback_name(root));

    let mut scripts = BTreeMap::new();
    scripts.insert("build".into(), "go build ./...".into());
    scripts.insert("test".into(), "go test ./...".into());
    scripts.insert("lint".into(), "go vet ./...".into());
    scripts.insert("fmt".into(), "go fmt ./...".into());
    scripts.insert("run".into(), "go run .".into());
    scripts.insert("dev".into(), "go run .".into());
    scripts.insert("debug".into(), "go run -race .".into());
    scripts.insert("clean".into(), "go clean".into());
    scripts.insert("tidy".into(), "go mod tidy".into());

    Ok(ProjectProfile {
        name,
        language: Some("go".into()),
        framework: None,
        package_manager: Some("go".into()),
        test_command: Some("go test ./...".into()),
        build_command: Some("go build ./...".into()),
        lint_command: Some("go vet ./...".into()),
        dev_command: Some("go run .".into()),
        run_command: Some("go run .".into()),
        debug_command: Some("go run -race .".into()),
        scripts,
        prefer_agent: None,
        entry_points: detect_entry_points(root, &["main.go", "cmd/", "internal/"]),
        agent_commands: Vec::new(),
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
    if let Some(value) = overrides.dev_command {
        profile.dev_command = Some(value);
    }
    if let Some(value) = overrides.run_command {
        profile.run_command = Some(value);
    }
    if let Some(value) = overrides.debug_command {
        profile.debug_command = Some(value);
    }
    if let Some(value) = overrides.prefer_agent {
        profile.prefer_agent = Some(value);
    }
    if let Some(value) = overrides.entry_points {
        profile.entry_points = value;
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

/// The `<section>.name` string from a parsed TOML manifest (e.g. `[package]` for
/// Cargo, `[project]` for pyproject), falling back to the directory name.
fn toml_name(toml: &TomlValue, section: &str, root: &Path) -> String {
    toml.get(section)
        .and_then(TomlValue::as_table)
        .and_then(|table| table.get("name"))
        .and_then(TomlValue::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback_name(root))
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

fn detect_python_package_manager(root: &Path) -> Option<String> {
    if root.join("uv.lock").is_file() {
        return Some("uv".into());
    }
    if root.join("poetry.lock").is_file() {
        return Some("poetry".into());
    }
    if root.join("pdm.lock").is_file() {
        return Some("pdm".into());
    }
    if root.join("Pipfile").is_file() || root.join("Pipfile.lock").is_file() {
        return Some("pipenv".into());
    }
    Some("pip".into())
}

fn python_dep_mentions(toml: &TomlValue, name: &str) -> bool {
    let project = toml.get("project").and_then(TomlValue::as_table);
    let deps = project
        .and_then(|t| t.get("dependencies"))
        .and_then(TomlValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(TomlValue::as_str);
    let optional = project
        .and_then(|t| t.get("optional-dependencies"))
        .and_then(TomlValue::as_table)
        .into_iter()
        .flat_map(|table| table.values())
        .filter_map(TomlValue::as_array)
        .flatten()
        .filter_map(TomlValue::as_str);

    deps.chain(optional).any(|dep| {
        dep.split(&['[', ' ', '=', '>', '<', '!', '~', ';'][..])
            .next()
            .is_some_and(|pkg| pkg.eq_ignore_ascii_case(name))
    })
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

/// Prefer a package-manager-qualified script invocation when the script key
/// exists in `scripts` (so `pnpm test` rather than the raw body). For Node,
/// role commands are always PM-qualified when the script is present.
fn script_or_qualified(
    package_manager: Option<&str>,
    scripts: &BTreeMap<String, String>,
    script_name: &str,
) -> Option<String> {
    if !scripts.contains_key(script_name) {
        return None;
    }
    qualify_script_command(
        package_manager,
        script_name,
        scripts.get(script_name).map(String::as_str),
    )
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

/// If a top-level role command is still empty but `scripts` has a matching key,
/// promote it. Handles Makefile/Justfile targets that arrived after language
/// detection, and non-Node projects whose scripts map already holds the
/// runnable command string.
fn fill_role_commands_from_scripts(profile: &mut ProjectProfile) {
    let is_node = profile.language.as_deref() == Some("typescript")
        || profile
            .package_manager
            .as_deref()
            .is_some_and(|pm| matches!(pm, "npm" | "pnpm" | "yarn" | "bun"));

    let resolve = |scripts: &BTreeMap<String, String>, key: &str| -> Option<String> {
        let body = scripts.get(key)?;
        if is_node {
            qualify_script_command(profile.package_manager.as_deref(), key, Some(body.as_str()))
        } else {
            Some(body.clone())
        }
    };

    if profile.test_command.is_none() {
        profile.test_command = resolve(&profile.scripts, "test");
    }
    if profile.build_command.is_none() {
        profile.build_command = resolve(&profile.scripts, "build");
    }
    if profile.lint_command.is_none() {
        profile.lint_command =
            resolve(&profile.scripts, "lint").or_else(|| resolve(&profile.scripts, "fmt"));
    }
    if profile.dev_command.is_none() {
        profile.dev_command =
            resolve(&profile.scripts, "dev").or_else(|| resolve(&profile.scripts, "start"));
    }
    if profile.run_command.is_none() {
        profile.run_command =
            resolve(&profile.scripts, "run").or_else(|| resolve(&profile.scripts, "start"));
    }
    if profile.debug_command.is_none() {
        profile.debug_command = resolve(&profile.scripts, "debug");
    }

    // Ensure common role keys appear in scripts when top-level fields are set
    // but the map is missing them (e.g. role filled only via top-level).
    for (key, value) in [
        ("test", profile.test_command.as_deref()),
        ("build", profile.build_command.as_deref()),
        ("lint", profile.lint_command.as_deref()),
        ("dev", profile.dev_command.as_deref()),
        ("run", profile.run_command.as_deref()),
        ("debug", profile.debug_command.as_deref()),
    ] {
        if let Some(cmd) = value {
            // For Node, scripts hold raw bodies; don't overwrite with PM-qualified.
            if !is_node {
                insert_default(&mut profile.scripts, key, cmd);
            }
        }
    }

    let _ = COMMON_SCRIPT_ROLES; // documented roles; detection fills what it can
}

fn insert_default(scripts: &mut BTreeMap<String, String>, key: &str, value: &str) {
    scripts
        .entry(key.to_string())
        .or_insert_with(|| value.to_string());
}

/// Parse simple Makefile targets (`name:`) and merge missing keys into scripts.
/// Ignores special targets (`.PHONY`, pattern rules with `%`).
fn merge_makefile_targets(root: &Path, scripts: &mut BTreeMap<String, String>) {
    let path = root.join("Makefile");
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('\t') {
            continue;
        }
        let Some(name) = line.split_once(':').map(|(n, _)| n.trim()) else {
            continue;
        };
        if name.is_empty()
            || name.starts_with('.')
            || name.contains('%')
            || name.contains('=')
            || name.contains(' ')
        {
            continue;
        }
        if name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            insert_default(scripts, name, &format!("make {name}"));
        }
    }
}

/// Parse Justfile recipe names (`name:` / `name arg:`) and merge missing keys.
fn merge_justfile_recipes(root: &Path, scripts: &mut BTreeMap<String, String>) {
    for filename in ["justfile", "Justfile"] {
        let path = root.join(filename);
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            // Recipe: `name:` or `name arg:` — take the first identifier.
            let Some(first) = line.split_whitespace().next() else {
                continue;
            };
            let name = first.trim_end_matches(':');
            if name == first {
                // No trailing colon on the first token — not a recipe header.
                if !line.contains(':') {
                    continue;
                }
                // `name arg1 arg2:` form — name is first token without colon.
            }
            if name.is_empty()
                || name.starts_with('@')
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                continue;
            }
            // Only treat as recipe if the line has a colon (recipe header).
            if !line.contains(':') {
                continue;
            }
            insert_default(scripts, name, &format!("just {name}"));
        }
        break;
    }
}

/// Keep the candidate entry points that actually exist under `root` (a trailing
/// `/` marks a directory candidate), preserving the given order.
fn detect_entry_points(root: &Path, candidates: &[&str]) -> Vec<String> {
    candidates
        .iter()
        .filter(|entry| root.join(entry.trim_end_matches('/')).exists())
        .map(|entry| (*entry).to_owned())
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
    "lint": "eslint .",
    "dev": "next dev"
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
        // Pinned: PM-qualified role commands from package.json scripts.
        assert_eq!(profile.test_command.as_deref(), Some("pnpm test"));
        assert_eq!(profile.build_command.as_deref(), Some("pnpm build"));
        assert_eq!(profile.lint_command.as_deref(), Some("pnpm lint"));
        assert_eq!(profile.dev_command.as_deref(), Some("pnpm dev"));
        assert!(profile.entry_points.contains(&"src/app/".to_string()));
    }

    #[test]
    fn node_framework_defaults_fill_missing_scripts() {
        // package.json declares next but omits scripts — learn fills framework
        // defaults so the profile still carries build/dev/start/lint.
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{
  "name": "bare-next",
  "packageManager": "pnpm@9.0.0",
  "dependencies": { "next": "15.0.0" }
}"#,
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        // if this fails after a refactor, that's intentional — update the values
        // AND the doc-comment on insert_node_framework_defaults together.
        // Invented defaults must be direct tool commands (not `pnpm <missing>`).
        assert_eq!(
            profile.scripts.get("dev").map(String::as_str),
            Some("next dev")
        );
        assert_eq!(
            profile.scripts.get("build").map(String::as_str),
            Some("next build")
        );
        assert_eq!(profile.dev_command.as_deref(), Some("next dev"));
        assert_eq!(profile.build_command.as_deref(), Some("next build"));
        assert_eq!(profile.run_command.as_deref(), Some("next start"));
    }

    #[test]
    fn package_json_scripts_override_framework_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{
  "name": "custom-next",
  "scripts": {
    "dev": "node ./scripts/dev.mjs"
  },
  "dependencies": { "next": "15.0.0" }
}"#,
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert_eq!(
            profile.scripts.get("dev").map(String::as_str),
            Some("node ./scripts/dev.mjs")
        );
        assert_eq!(profile.dev_command.as_deref(), Some("npm run dev"));
    }

    #[test]
    fn rust_profile_includes_common_commands() {
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

        let profile = detect_profile(tmp.path()).unwrap();
        // Pinned common Cargo loop — if this fails after a refactor, update the
        // values AND the detect_rust_profile defaults together.
        assert_eq!(profile.test_command.as_deref(), Some("cargo test"));
        assert_eq!(profile.build_command.as_deref(), Some("cargo build"));
        assert_eq!(profile.lint_command.as_deref(), Some("cargo clippy"));
        assert_eq!(profile.dev_command.as_deref(), Some("cargo run"));
        assert_eq!(profile.run_command.as_deref(), Some("cargo run"));
        assert_eq!(
            profile.debug_command.as_deref(),
            Some("RUST_BACKTRACE=1 cargo run")
        );
        assert_eq!(
            profile.scripts.get("release").map(String::as_str),
            Some("cargo build --release")
        );
        assert_eq!(
            profile.scripts.get("fmt").map(String::as_str),
            Some("cargo fmt")
        );
    }

    #[test]
    fn go_profile_includes_common_commands() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/svc\n\ngo 1.22\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert_eq!(profile.name, "svc");
        assert_eq!(profile.test_command.as_deref(), Some("go test ./..."));
        assert_eq!(profile.build_command.as_deref(), Some("go build ./..."));
        assert_eq!(profile.lint_command.as_deref(), Some("go vet ./..."));
        assert_eq!(profile.dev_command.as_deref(), Some("go run ."));
        assert_eq!(profile.run_command.as_deref(), Some("go run ."));
        assert_eq!(profile.debug_command.as_deref(), Some("go run -race ."));
    }

    #[test]
    fn python_uv_project_prefixes_common_commands() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            r#"[project]
name = "demo-py"
version = "0.1.0"
dependencies = ["fastapi"]
"#,
        )
        .unwrap();
        fs::write(tmp.path().join("uv.lock"), "# lock\n").unwrap();
        fs::write(tmp.path().join("main.py"), "app = None\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert_eq!(profile.package_manager.as_deref(), Some("uv"));
        assert_eq!(profile.framework.as_deref(), Some("fastapi"));
        assert_eq!(profile.test_command.as_deref(), Some("uv run pytest"));
        assert_eq!(
            profile.dev_command.as_deref(),
            Some("uv run uvicorn main:app --reload")
        );
        assert_eq!(profile.lint_command.as_deref(), Some("uv run ruff check ."));
    }

    #[test]
    fn makefile_targets_merge_into_scripts_without_clobbering() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("Makefile"),
            "\
.PHONY: test deploy
test:
\tcargo nextest run
deploy:
\t./scripts/deploy.sh
",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        // Existing cargo test must not be replaced by `make test`.
        assert_eq!(
            profile.scripts.get("test").map(String::as_str),
            Some("cargo test")
        );
        assert_eq!(
            profile.scripts.get("deploy").map(String::as_str),
            Some("make deploy")
        );
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
dev_command = "cargo watch -x run"

[scripts]
dev = "cargo watch -x run"
"#,
        )
        .unwrap();

        let mut profile = detect_profile(tmp.path()).unwrap();
        apply_overrides(tmp.path(), &mut profile).unwrap();

        assert_eq!(profile.framework.as_deref(), Some("axum"));
        assert_eq!(profile.prefer_agent.as_deref(), Some("claude"));
        assert_eq!(profile.test_command.as_deref(), Some("cargo nextest run"));
        assert_eq!(profile.dev_command.as_deref(), Some("cargo watch -x run"));
        assert_eq!(
            profile.scripts.get("dev").map(String::as_str),
            Some("cargo watch -x run")
        );
    }

    #[test]
    fn qr_toml_overrides_entry_points() {
        // `entry_points` was missing from the override struct, so this key was a
        // silent no-op; it must now replace the detected entry points.
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join(".qr.toml"),
            "entry_points = [\"custom/main.rs\"]\n",
        )
        .unwrap();

        let mut profile = detect_profile(tmp.path()).unwrap();
        apply_overrides(tmp.path(), &mut profile).unwrap();
        assert_eq!(profile.entry_points, vec!["custom/main.rs".to_string()]);
    }

    #[test]
    fn go_module_with_trailing_slash_yields_last_segment_not_empty() {
        // `module github.com/acme/` previously yielded an empty project name;
        // it must now resolve to the last real path segment.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("acme-svc");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("go.mod"), "module github.com/acme/\n\ngo 1.22\n").unwrap();

        let profile = detect_profile(&root).unwrap();
        assert_eq!(profile.name, "acme");
        assert!(!profile.name.is_empty());
    }

    #[test]
    fn go_module_that_is_only_slashes_falls_back_to_dir_name() {
        // A degenerate `module /` has no real segment, so the directory name is
        // used rather than an empty string.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("acme-svc");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("go.mod"), "module /\n\ngo 1.22\n").unwrap();

        let profile = detect_profile(&root).unwrap();
        assert_eq!(profile.name, "acme-svc");
    }

    #[test]
    fn old_profile_json_without_new_fields_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".qr")).unwrap();
        fs::write(
            tmp.path().join(".qr/profile.json"),
            r#"{"name":"demo","language":"rust","framework":null,"package_manager":"cargo","test_command":"cargo test","build_command":"cargo build","lint_command":"cargo clippy","scripts":{},"prefer_agent":null,"entry_points":[]}"#,
        )
        .unwrap();

        let profile = load_profile_from(tmp.path()).unwrap();
        assert_eq!(profile.name, "demo");
        assert_eq!(profile.dev_command, None);
        assert_eq!(profile.run_command, None);
        assert_eq!(profile.debug_command, None);
    }
}
