use std::{
    collections::{BTreeMap, BTreeSet},
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
    // Keys that exist in package.json may be invoked as package scripts
    // (`pnpm test`). Framework-invented fallbacks use the package manager's
    // executable runner (`pnpm exec next dev`) rather than a missing script.
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

    insert_node_framework_defaults(
        &mut scripts,
        framework.as_deref(),
        package_manager.as_deref(),
    );

    let pm = package_manager.as_deref();
    let role = |name: &str| -> Option<String> {
        if package_scripts.contains_key(name) {
            script_or_qualified(pm, &scripts, name)
        } else {
            // Invented / merged default: use the command body as-is (framework
            // tool, `make …`, `just …`), not a PM-qualified missing script.
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
    })
}

/// Insert executable framework defaults only when the key is absent. When the
/// manifest omits a script, use the package manager's exec form so a local
/// `node_modules/.bin` tool works without requiring a global install.
fn insert_node_framework_defaults(
    scripts: &mut BTreeMap<String, String>,
    framework: Option<&str>,
    package_manager: Option<&str>,
) {
    let tool = |command: &str| node_tool_command(package_manager, command);
    match framework {
        Some("nextjs") => {
            insert_default(scripts, "dev", &tool("next dev"));
            insert_default(scripts, "build", &tool("next build"));
            insert_default(scripts, "start", &tool("next start"));
            // Do not invent `next lint`: Next.js 16 removed that command. Only
            // promote lint when package.json (or another merger) already has it.
        }
        Some("vite") => {
            insert_default(scripts, "dev", &tool("vite"));
            insert_default(scripts, "build", &tool("vite build"));
            insert_default(scripts, "start", &tool("vite preview"));
        }
        _ => {}
    }
}

fn node_tool_command(package_manager: Option<&str>, command: &str) -> String {
    match package_manager.unwrap_or("npm") {
        "pnpm" => format!("pnpm exec {command}"),
        "yarn" => format!("yarn exec {command}"),
        "bun" => format!("bunx {command}"),
        _ => format!("npm exec -- {command}"),
    }
}

fn detect_rust_profile(root: &Path) -> Result<ProjectProfile> {
    let raw = fs::read_to_string(root.join("Cargo.toml"))
        .with_context(|| format!("Failed to read {}", root.join("Cargo.toml").display()))?;
    let toml: TomlValue = toml::from_str(&raw).context("Failed to parse Cargo.toml")?;
    let name = toml_name(&toml, "package", root);
    let run_command = cargo_run_command(root, &toml);

    // Hardcoded Cargo defaults for the common developer loop. `.qr.toml` and
    // Makefile/Justfile merges can override any of these later. Run-oriented
    // commands are only filled when a binary target is present.
    let mut scripts = BTreeMap::new();
    scripts.insert("build".into(), "cargo build".into());
    scripts.insert("test".into(), "cargo test".into());
    scripts.insert("lint".into(), "cargo clippy".into());
    scripts.insert("fmt".into(), "cargo fmt".into());
    scripts.insert("check".into(), "cargo check".into());
    if let Some(run) = &run_command {
        scripts.insert("run".into(), run.clone());
        scripts.insert("dev".into(), run.clone());
        scripts.insert("debug".into(), format!("RUST_BACKTRACE=1 {run}"));
    }
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
        dev_command: run_command.clone(),
        run_command: run_command.clone(),
        debug_command: run_command.map(|run| format!("RUST_BACKTRACE=1 {run}")),
        scripts,
        prefer_agent: None,
        entry_points: detect_entry_points(root, &["src/main.rs", "src/lib.rs", "src/bin/"]),
    })
}

/// Return a safe unqualified Cargo run command only when Cargo can choose one
/// runnable binary without extra features or `--bin`. Ambiguous multi-bin
/// crates, virtual workspaces, disabled auto-bins, and feature-gated-only bins
/// deliberately leave the role empty.
fn cargo_run_command(root: &Path, toml: &TomlValue) -> Option<String> {
    let package = toml.get("package").and_then(TomlValue::as_table)?;
    let package_name = package.get("name").and_then(TomlValue::as_str);
    let edition_is_2015 = match package.get("edition") {
        None => true,
        Some(edition) => edition.as_str() == Some("2015"),
    };
    let has_manually_defined_target = toml.get("lib").is_some()
        || ["bin", "example", "test", "bench"].iter().any(|kind| {
            toml.get(*kind)
                .and_then(TomlValue::as_array)
                .is_some_and(|targets| !targets.is_empty())
        });
    let autobins = package
        .get("autobins")
        .and_then(TomlValue::as_bool)
        .unwrap_or(!(edition_is_2015 && has_manually_defined_target));
    let default_features = cargo_default_enabled_features(toml);
    let mut bins = Vec::<String>::new();
    let mut explicit_paths = Vec::<PathBuf>::new();
    let push_bin = |bins: &mut Vec<String>, name: String| {
        if !name.is_empty() && !bins.contains(&name) {
            bins.push(name);
        }
    };

    if let Some(TomlValue::Array(explicit_bins)) = toml.get("bin") {
        for bin in explicit_bins.iter().filter_map(TomlValue::as_table) {
            let declared_name = bin.get("name").and_then(TomlValue::as_str);
            let declared_path = bin
                .get("path")
                .and_then(TomlValue::as_str)
                .map(|path| root.join(path));
            let path = declared_path.or_else(|| {
                let name = declared_name?;
                let mut candidates = vec![
                    root.join("src/bin").join(format!("{name}.rs")),
                    root.join("src/bin").join(name).join("main.rs"),
                ];
                if package_name == Some(name) {
                    candidates.push(root.join("src/main.rs"));
                }
                candidates.into_iter().find(|candidate| candidate.is_file())
            });
            let path = path.filter(|path| path.is_file())?;
            if !explicit_paths.contains(&path) {
                explicit_paths.push(path.clone());
            }

            let has_unavailable_required_feature = bin
                .get("required-features")
                .and_then(TomlValue::as_array)
                .is_some_and(|required_features| {
                    required_features.iter().any(|required_feature| {
                        let Some(required_feature) = required_feature.as_str() else {
                            return true;
                        };
                        !default_features.contains(required_feature)
                    })
                });
            if has_unavailable_required_feature {
                continue;
            }
            let name = declared_name.map(ToOwned::to_owned).or_else(|| {
                if path.file_name().and_then(|name| name.to_str()) == Some("main.rs") {
                    path.parent()?
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(ToOwned::to_owned)
                } else {
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .map(ToOwned::to_owned)
                }
            });
            if let Some(name) = name {
                push_bin(&mut bins, name);
            }
        }
    }

    if autobins {
        let main_path = root.join("src/main.rs");
        if main_path.is_file() && !explicit_paths.contains(&main_path) {
            if let Some(name) = package_name {
                push_bin(&mut bins, name.to_string());
            }
        }
        if let Ok(entries) = fs::read_dir(root.join("src/bin")) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = if path.is_file()
                    && path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                {
                    path.file_stem().and_then(|name| name.to_str())
                } else if path.is_dir() && path.join("main.rs").is_file() {
                    path.file_name().and_then(|name| name.to_str())
                } else {
                    None
                };
                let target_path = if path.is_dir() {
                    path.join("main.rs")
                } else {
                    path.clone()
                };
                if !explicit_paths.contains(&target_path) {
                    if let Some(name) = name {
                        push_bin(&mut bins, name.to_string());
                    }
                }
            }
        }
    }

    if let Some(default_run) = package.get("default-run").and_then(TomlValue::as_str) {
        return bins
            .iter()
            .any(|name| name == default_run)
            .then(|| "cargo run".into());
    }

    (bins.len() == 1).then(|| "cargo run".into())
}

fn cargo_default_enabled_features(toml: &TomlValue) -> BTreeSet<&str> {
    let Some(features) = toml.get("features").and_then(TomlValue::as_table) else {
        return BTreeSet::new();
    };
    if features
        .get("default")
        .and_then(TomlValue::as_array)
        .is_none()
    {
        return BTreeSet::new();
    }

    let mut enabled = BTreeSet::new();
    let mut pending = vec!["default"];
    while let Some(feature) = pending.pop() {
        if !enabled.insert(feature) {
            continue;
        }
        let Some(members) = features.get(feature).and_then(TomlValue::as_array) else {
            continue;
        };
        for member in members.iter().filter_map(TomlValue::as_str) {
            if member.starts_with("dep:") {
                continue;
            }
            if let Some((dependency, _)) = member.split_once('/') {
                // `dependency/feature` enables an optional dependency's implicit
                // feature; the weak `dependency?/feature` form deliberately does not.
                if !dependency.ends_with('?') {
                    pending.push(dependency);
                }
            } else {
                pending.push(member);
            }
        }
    }
    enabled
}

fn detect_python_profile(root: &Path) -> Result<ProjectProfile> {
    let raw = fs::read_to_string(root.join("pyproject.toml"))
        .with_context(|| format!("Failed to read {}", root.join("pyproject.toml").display()))?;
    let toml: TomlValue = toml::from_str(&raw).context("Failed to parse pyproject.toml")?;
    let name = toml_name(&toml, "project", root);
    let package_manager = detect_python_package_manager(root);

    let framework = if root.join("manage.py").is_file() || python_dep_mentions(&toml, "django") {
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
            if root.join("manage.py").is_file() {
                scripts.insert("dev".into(), "python manage.py runserver".into());
                scripts.insert("run".into(), "python manage.py runserver".into());
                scripts.insert("debug".into(), "python -X dev manage.py runserver".into());
                (
                    Some("python manage.py runserver".into()),
                    Some("python manage.py runserver".into()),
                    Some("python -X dev manage.py runserver".into()),
                )
            } else {
                (None, None, None)
            }
        }
        Some("fastapi") => {
            let app = if root.join("main.py").is_file() {
                Some("main:app")
            } else if root.join("app.py").is_file() {
                Some("app:app")
            } else {
                None
            };
            if let Some(app) = app {
                let dev = format!("uvicorn {app} --reload");
                let run = format!("uvicorn {app}");
                let debug = format!("uvicorn {app} --reload --log-level debug");
                scripts.insert("dev".into(), dev.clone());
                scripts.insert("run".into(), run.clone());
                scripts.insert("debug".into(), debug.clone());
                (Some(dev), Some(run), Some(debug))
            } else {
                (None, None, None)
            }
        }
        Some("flask") => {
            if root.join("app.py").is_file() || root.join("wsgi.py").is_file() {
                scripts.insert("dev".into(), "flask run --debug".into());
                scripts.insert("run".into(), "flask run".into());
                scripts.insert("debug".into(), "flask run --debug".into());
                (
                    Some("flask run --debug".into()),
                    Some("flask run".into()),
                    Some("flask run --debug".into()),
                )
            } else {
                (None, None, None)
            }
        }
        _ => {
            let script = if root.join("main.py").is_file() {
                Some("main.py")
            } else if root.join("app.py").is_file() {
                Some("app.py")
            } else {
                None
            };
            if let Some(script) = script {
                let run = format!("python {script}");
                let debug = format!("python -X dev {script}");
                scripts.insert("run".into(), run.clone());
                scripts.insert("dev".into(), run.clone());
                scripts.insert("debug".into(), debug.clone());
                (Some(run.clone()), Some(run), Some(debug))
            } else {
                (None, None, None)
            }
        }
    };

    // Prefer `uv run` / `poetry run` / `pipenv run` prefixes when that PM is
    // detected so the learned commands match how the project is actually invoked.
    let prefix = match package_manager.as_deref() {
        Some("uv") => Some("uv run"),
        Some("poetry") => Some("poetry run"),
        Some("pdm") => Some("pdm run"),
        Some("pipenv") => Some("pipenv run"),
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

    // Only emit `go run .` when the module root itself is a main package.
    // Projects whose binaries live under `cmd/` keep build/test/lint but leave
    // run/dev/debug unset (Makefile/Justfile/`.qr.toml` can still fill them).
    let build_context = GoBuildContext::from_go_mod(&raw);
    let root_is_main = go_root_is_main_package(root, &build_context);

    let mut scripts = BTreeMap::new();
    scripts.insert("build".into(), "go build ./...".into());
    scripts.insert("test".into(), "go test ./...".into());
    scripts.insert("lint".into(), "go vet ./...".into());
    scripts.insert("fmt".into(), "go fmt ./...".into());
    if root_is_main {
        scripts.insert("run".into(), "go run .".into());
        scripts.insert("dev".into(), "go run .".into());
        scripts.insert("debug".into(), "go run -race .".into());
    }
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
        dev_command: root_is_main.then(|| "go run .".into()),
        run_command: root_is_main.then(|| "go run .".into()),
        debug_command: root_is_main.then(|| "go run -race .".into()),
        scripts,
        prefer_agent: None,
        entry_points: detect_entry_points(root, &["main.go", "cmd/", "internal/"]),
    })
}

/// True when the module root looks like a runnable `main` package. Go does not
/// require the entrypoint file to be named `main.go`; any root `.go` file may
/// declare `package main` and provide `func main`.
fn go_root_is_main_package(root: &Path, build_context: &GoBuildContext) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    let mut has_main_package = false;
    let mut has_main_function = false;

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !go_filename_matches_current_target(filename, build_context) {
            continue;
        }
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        if !go_build_constraints_match(&raw, build_context) {
            continue;
        }
        let code = go_code_without_comments_and_literals(&raw);
        let tokens = code
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        let package = tokens
            .windows(2)
            .find(|pair| pair[0] == "package")
            .map(|pair| pair[1]);
        if package != Some("main") {
            return false;
        }
        has_main_package = true;
        has_main_function |= tokens.windows(2).any(|pair| pair == ["func", "main"]);
    }

    has_main_package && has_main_function
}

fn go_filename_matches_current_target(filename: &str, build_context: &GoBuildContext) -> bool {
    if filename.starts_with(['.', '_'])
        || !filename.ends_with(".go")
        || filename.ends_with("_test.go")
    {
        return false;
    }
    const GOOS: &[&str] = &[
        "aix",
        "android",
        "darwin",
        "dragonfly",
        "freebsd",
        "hurd",
        "illumos",
        "ios",
        "js",
        "linux",
        "netbsd",
        "openbsd",
        "plan9",
        "solaris",
        "wasip1",
        "windows",
    ];
    const GOARCH: &[&str] = &[
        "386", "amd64", "arm", "arm64", "loong64", "mips", "mips64", "mips64le", "mipsle", "ppc64",
        "ppc64le", "riscv64", "s390x", "sparc64", "wasm",
    ];
    let stem = filename.trim_end_matches(".go");
    if !stem.contains('_') {
        return true;
    }
    let suffixes = stem.rsplit('_').collect::<Vec<_>>();
    if suffixes.len() >= 2 && GOARCH.contains(&suffixes[0]) && GOOS.contains(&suffixes[1]) {
        return suffixes[0] == build_context.goarch
            && go_os_tag_enabled(suffixes[1], &build_context.goos);
    }
    if GOOS.contains(&suffixes[0]) {
        return go_os_tag_enabled(suffixes[0], &build_context.goos);
    }
    if GOARCH.contains(&suffixes[0]) {
        return suffixes[0] == build_context.goarch;
    }
    true
}

fn go_code_without_comments_and_literals(raw: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        InterpretedString,
        RawString,
        Rune,
    }

    let mut code = raw.as_bytes().to_vec();
    let mut state = State::Code;
    let mut index = 0usize;
    while index < code.len() {
        match state {
            State::Code => match (code[index], code.get(index + 1).copied()) {
                (b'/', Some(b'/')) => {
                    code[index] = b' ';
                    code[index + 1] = b' ';
                    index += 2;
                    state = State::LineComment;
                }
                (b'/', Some(b'*')) => {
                    code[index] = b' ';
                    code[index + 1] = b' ';
                    index += 2;
                    state = State::BlockComment;
                }
                (b'"', _) => {
                    code[index] = b' ';
                    index += 1;
                    state = State::InterpretedString;
                }
                (b'`', _) => {
                    code[index] = b' ';
                    index += 1;
                    state = State::RawString;
                }
                (b'\'', _) => {
                    code[index] = b' ';
                    index += 1;
                    state = State::Rune;
                }
                _ => index += 1,
            },
            State::LineComment => {
                if code[index] == b'\n' {
                    state = State::Code;
                } else {
                    code[index] = b' ';
                }
                index += 1;
            }
            State::BlockComment => {
                if code[index] == b'*' && code.get(index + 1) == Some(&b'/') {
                    code[index] = b' ';
                    code[index + 1] = b' ';
                    index += 2;
                    state = State::Code;
                } else {
                    if code[index] != b'\n' {
                        code[index] = b' ';
                    }
                    index += 1;
                }
            }
            State::InterpretedString | State::Rune => {
                let delimiter = if matches!(state, State::Rune) {
                    b'\''
                } else {
                    b'"'
                };
                if code[index] == b'\\' {
                    code[index] = b' ';
                    if let Some(next) = code.get_mut(index + 1) {
                        if *next != b'\n' {
                            *next = b' ';
                        }
                    }
                    index += 2;
                } else {
                    let closes_literal = code[index] == delimiter;
                    if code[index] != b'\n' {
                        code[index] = b' ';
                    }
                    index += 1;
                    if closes_literal {
                        state = State::Code;
                    }
                }
            }
            State::RawString => {
                let closes_literal = code[index] == b'`';
                if code[index] != b'\n' {
                    code[index] = b' ';
                }
                index += 1;
                if closes_literal {
                    state = State::Code;
                }
            }
        }
    }

    String::from_utf8(code).expect("replacing source bytes with ASCII preserves UTF-8")
}

fn current_goos() -> &'static str {
    match env::consts::OS {
        "macos" => "darwin",
        "visionos" => "ios",
        other => other,
    }
}

fn current_goarch() -> &'static str {
    match env::consts::ARCH {
        "x86" => "386",
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "powerpc64" if cfg!(target_endian = "little") => "ppc64le",
        "powerpc64" => "ppc64",
        "wasm32" => "wasm",
        other => other,
    }
}

struct GoBuildContext {
    goos: String,
    goarch: String,
    cgo_enabled: Option<bool>,
    minimum_release_minor: Option<u32>,
    arch_tuning: Option<String>,
    user_tags: Option<BTreeSet<String>>,
}

impl GoBuildContext {
    fn from_go_mod(go_mod: &str) -> Self {
        let goos = env::var("GOOS")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| current_goos().to_string());
        let goarch = env::var("GOARCH")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| current_goarch().to_string());
        let cgo_enabled = match env::var("CGO_ENABLED").ok().as_deref() {
            Some("1") => Some(true),
            Some("0") => Some(false),
            _ => None,
        };
        let minimum_release_minor = go_mod.lines().find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some("go"))
                .then(|| fields.next())
                .flatten()
                .and_then(parse_go_version_minor)
        });
        let arch_tuning = match goarch.as_str() {
            "amd64" => env::var("GOAMD64").ok(),
            _ => None,
        }
        .filter(|value| !value.is_empty());
        let goflags = env::var("GOFLAGS").ok();
        let user_tags = parse_go_user_build_tags(goflags.as_deref());

        Self {
            goos,
            goarch,
            cgo_enabled,
            minimum_release_minor,
            arch_tuning,
            user_tags,
        }
    }
}

fn parse_go_user_build_tags(goflags: Option<&str>) -> Option<BTreeSet<String>> {
    let mut fields = goflags.unwrap_or_default().split_ascii_whitespace();
    let mut selected = None;
    while let Some(field) = fields.next() {
        let value = if field == "-tags" {
            fields.next()?
        } else if let Some(value) = field.strip_prefix("-tags=") {
            value
        } else {
            if field.starts_with("-tags") {
                return None;
            }
            continue;
        };
        selected = Some(parse_go_tag_list(value)?);
    }
    Some(selected.unwrap_or_default())
}

fn parse_go_tag_list(value: &str) -> Option<BTreeSet<String>> {
    if value.is_empty() {
        return Some(BTreeSet::new());
    }
    value
        .split(',')
        .map(|tag| {
            (!tag.is_empty()
                && tag
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.')))
            .then(|| tag.to_string())
        })
        .collect()
}

fn parse_go_version_minor(value: &str) -> Option<u32> {
    let version = value.strip_prefix("go").unwrap_or(value);
    let minor = version.strip_prefix("1.")?;
    let digits = minor
        .bytes()
        .take_while(u8::is_ascii_digit)
        .collect::<Vec<_>>();
    (!digits.is_empty())
        .then(|| std::str::from_utf8(&digits).ok()?.parse().ok())
        .flatten()
}

fn parse_go_release_tag_minor(value: &str) -> Option<u32> {
    let minor = value.strip_prefix("go1.")?;
    (!minor.is_empty() && minor.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| minor.parse().ok())
        .flatten()
}

fn go_os_tag_enabled(tag: &str, goos: &str) -> bool {
    tag == goos
        || matches!(
            (goos, tag),
            ("android", "linux") | ("illumos", "solaris") | ("ios", "darwin")
        )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GoConstraintValue {
    Enabled,
    Disabled,
    Unknown,
}

impl GoConstraintValue {
    fn not(self) -> Self {
        match self {
            Self::Enabled => Self::Disabled,
            Self::Disabled => Self::Enabled,
            Self::Unknown => Self::Unknown,
        }
    }

    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Disabled, _) | (_, Self::Disabled) => Self::Disabled,
            (Self::Enabled, Self::Enabled) => Self::Enabled,
            _ => Self::Unknown,
        }
    }

    fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::Enabled, _) | (_, Self::Enabled) => Self::Enabled,
            (Self::Disabled, Self::Disabled) => Self::Disabled,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy)]
enum GoConstraintToken<'a> {
    Tag(&'a str),
    Not,
    And,
    Or,
    LeftParen,
    RightParen,
}

fn go_build_constraints_match(raw: &str, build_context: &GoBuildContext) -> bool {
    let mut legacy_constraints = Vec::new();
    let mut index = 0usize;
    while index < raw.len() {
        while raw
            .as_bytes()
            .get(index)
            .is_some_and(u8::is_ascii_whitespace)
        {
            index += 1;
        }
        let rest = &raw[index..];
        if rest.starts_with("/*") {
            let Some(end) = rest.find("*/") else {
                return false;
            };
            index += end + 2;
            continue;
        }
        if !rest.starts_with("//") {
            break;
        }

        let end = rest.find('\n').unwrap_or(rest.len());
        let comment = &rest[..end];
        index += end;
        if let Some(expression) = go_directive_expression(comment, "//go:build") {
            return evaluate_go_build_constraint(expression, build_context)
                == Some(GoConstraintValue::Enabled);
        }
        if let Some(expression) = go_directive_expression(comment, "// +build") {
            legacy_constraints.push(expression);
        }
    }
    legacy_constraints
        .into_iter()
        .try_fold(GoConstraintValue::Enabled, |value, expression| {
            evaluate_legacy_go_build_constraint(expression, build_context)
                .map(|expression_value| value.and(expression_value))
        })
        == Some(GoConstraintValue::Enabled)
}

fn go_directive_expression<'a>(comment: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = comment.strip_prefix(prefix)?;
    (rest.is_empty() || rest.as_bytes()[0].is_ascii_whitespace()).then(|| rest.trim_start())
}

fn evaluate_legacy_go_build_constraint(
    expression: &str,
    build_context: &GoBuildContext,
) -> Option<GoConstraintValue> {
    let mut has_option = false;
    let mut expression_matches = GoConstraintValue::Disabled;

    for option in expression.split_whitespace() {
        has_option = true;
        let mut option_matches = GoConstraintValue::Enabled;

        for raw_tag in option.split(',') {
            let (negated, tag) = raw_tag
                .strip_prefix('!')
                .map_or((false, raw_tag), |tag| (true, tag));
            if tag.is_empty()
                || !tag
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
            {
                return None;
            }

            let tag_matches = go_build_tag_value(tag, build_context);
            option_matches = option_matches.and(if negated {
                tag_matches.not()
            } else {
                tag_matches
            });
        }

        expression_matches = expression_matches.or(option_matches);
    }

    has_option.then_some(expression_matches)
}

fn evaluate_go_build_constraint(
    expression: &str,
    build_context: &GoBuildContext,
) -> Option<GoConstraintValue> {
    let mut tokens = Vec::new();
    let mut index = 0usize;
    let bytes = expression.as_bytes();
    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b'!' => {
                tokens.push(GoConstraintToken::Not);
                index += 1;
            }
            b'(' => {
                tokens.push(GoConstraintToken::LeftParen);
                index += 1;
            }
            b')' => {
                tokens.push(GoConstraintToken::RightParen);
                index += 1;
            }
            b'&' if bytes.get(index + 1) == Some(&b'&') => {
                tokens.push(GoConstraintToken::And);
                index += 2;
            }
            b'|' if bytes.get(index + 1) == Some(&b'|') => {
                tokens.push(GoConstraintToken::Or);
                index += 2;
            }
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.') => {
                let start = index;
                while bytes
                    .get(index)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
                {
                    index += 1;
                }
                tokens.push(GoConstraintToken::Tag(&expression[start..index]));
            }
            _ => return None,
        }
    }

    let mut parser = GoConstraintParser {
        tokens: &tokens,
        index: 0,
        build_context,
    };
    let value = parser.parse_or()?;
    (parser.index == tokens.len()).then_some(value)
}

struct GoConstraintParser<'a, 'b> {
    tokens: &'a [GoConstraintToken<'b>],
    index: usize,
    build_context: &'a GoBuildContext,
}

impl GoConstraintParser<'_, '_> {
    fn parse_or(&mut self) -> Option<GoConstraintValue> {
        let mut value = self.parse_and()?;
        while matches!(self.tokens.get(self.index), Some(GoConstraintToken::Or)) {
            self.index += 1;
            let right = self.parse_and()?;
            value = value.or(right);
        }
        Some(value)
    }

    fn parse_and(&mut self) -> Option<GoConstraintValue> {
        let mut value = self.parse_unary()?;
        while matches!(self.tokens.get(self.index), Some(GoConstraintToken::And)) {
            self.index += 1;
            let right = self.parse_unary()?;
            value = value.and(right);
        }
        Some(value)
    }

    fn parse_unary(&mut self) -> Option<GoConstraintValue> {
        match self.tokens.get(self.index).copied()? {
            GoConstraintToken::Not => {
                self.index += 1;
                self.parse_unary().map(GoConstraintValue::not)
            }
            GoConstraintToken::LeftParen => {
                self.index += 1;
                let value = self.parse_or()?;
                if !matches!(
                    self.tokens.get(self.index),
                    Some(GoConstraintToken::RightParen)
                ) {
                    return None;
                }
                self.index += 1;
                Some(value)
            }
            GoConstraintToken::Tag(tag) => {
                self.index += 1;
                Some(go_build_tag_value(tag, self.build_context))
            }
            _ => None,
        }
    }
}

fn go_build_tag_value(tag: &str, build_context: &GoBuildContext) -> GoConstraintValue {
    const GOOS: &[&str] = &[
        "aix",
        "android",
        "darwin",
        "dragonfly",
        "freebsd",
        "hurd",
        "illumos",
        "ios",
        "js",
        "linux",
        "netbsd",
        "openbsd",
        "plan9",
        "solaris",
        "wasip1",
        "windows",
    ];
    const GOARCH: &[&str] = &[
        "386", "amd64", "arm", "arm64", "loong64", "mips", "mips64", "mips64le", "mipsle", "ppc64",
        "ppc64le", "riscv64", "s390x", "sparc64", "wasm",
    ];
    const UNIX_GOOS: &[&str] = &[
        "aix",
        "android",
        "darwin",
        "dragonfly",
        "freebsd",
        "hurd",
        "illumos",
        "ios",
        "linux",
        "netbsd",
        "openbsd",
        "solaris",
    ];

    if build_context
        .user_tags
        .as_ref()
        .is_some_and(|tags| tags.contains(tag))
    {
        return GoConstraintValue::Enabled;
    }
    if go_os_tag_enabled(tag, &build_context.goos) {
        return GoConstraintValue::Enabled;
    }
    if GOOS.contains(&tag) {
        return GoConstraintValue::Disabled;
    }
    if tag == build_context.goarch || tag == "gc" {
        return GoConstraintValue::Enabled;
    }
    if GOARCH.contains(&tag) || tag == "gccgo" {
        return GoConstraintValue::Disabled;
    }
    if tag == "unix" {
        return if UNIX_GOOS.contains(&build_context.goos.as_str()) {
            GoConstraintValue::Enabled
        } else {
            GoConstraintValue::Disabled
        };
    }
    if tag == "cgo" {
        return match build_context.cgo_enabled {
            Some(true) => GoConstraintValue::Enabled,
            Some(false) => GoConstraintValue::Disabled,
            None => GoConstraintValue::Unknown,
        };
    }
    if let Some(required_minor) = parse_go_release_tag_minor(tag) {
        return match build_context.minimum_release_minor {
            Some(minimum_minor) if required_minor <= minimum_minor => GoConstraintValue::Enabled,
            _ => GoConstraintValue::Unknown,
        };
    }
    if let Some((arch, feature)) = tag.split_once('.') {
        if GOARCH.contains(&arch) {
            if arch != build_context.goarch {
                return GoConstraintValue::Disabled;
            }
            if arch == "amd64" {
                let required = feature
                    .strip_prefix('v')
                    .and_then(|value| value.parse().ok());
                let selected = build_context
                    .arch_tuning
                    .as_deref()
                    .and_then(|value| value.strip_prefix('v'))
                    .and_then(|value| value.parse().ok());
                return match (required, selected) {
                    (Some(required), Some(selected))
                        if (1..=4).contains(&required) && (1..=4).contains(&selected) =>
                    {
                        if required <= selected {
                            GoConstraintValue::Enabled
                        } else {
                            GoConstraintValue::Disabled
                        }
                    }
                    _ => GoConstraintValue::Unknown,
                };
            }
            return GoConstraintValue::Unknown;
        }
    }
    if tag == "boringcrypto" || tag.starts_with("goexperiment.") {
        return GoConstraintValue::Unknown;
    }

    // If GOFLAGS was parsed completely, absent custom tags are disabled. Keep
    // malformed or unsupported tag configuration Unknown so negation cannot
    // create a false-positive `go run .` inference.
    if build_context.user_tags.is_some() {
        GoConstraintValue::Disabled
    } else {
        GoConstraintValue::Unknown
    }
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
    if root.join("bun.lock").is_file() || root.join("bun.lockb").is_file() {
        return Some("bun".into());
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
        "pnpm" if script_name == "run" => Some("pnpm run run".into()),
        "pnpm" => Some(format!("pnpm {script_name}")),
        "yarn" if script_name == "run" => Some("yarn run run".into()),
        "yarn" => Some(format!("yarn {script_name}")),
        "bun" => Some(format!("bun run {script_name}")),
        _ if script_name == "test" => Some("npm test".into()),
        _ => Some(format!("npm run {script_name}")),
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

    // Node role commands from package.json / framework defaults are set in
    // `detect_node_profile`. Anything still empty here came from a later merge
    // (Makefile/Justfile) and already has a runnable body — promote it as-is
    // rather than rewriting as `npm run <missing-script>`.
    let resolve = |scripts: &BTreeMap<String, String>, key: &str| -> Option<String> {
        scripts.get(key).cloned()
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
/// Ignores special targets (`.PHONY`, pattern rules with `%`) and variable
/// assignments (`:=`, `?=`, `::=`).
fn merge_makefile_targets(root: &Path, scripts: &mut BTreeMap<String, String>) {
    let path = root.join("Makefile");
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    for raw_line in raw.lines() {
        // Recipe bodies are tab-indented; skip them before trimming so a body
        // line that happens to contain `:` is not treated as a target header.
        if raw_line.starts_with('\t') {
            continue;
        }
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, rest)) = line.split_once(':').map(|(n, r)| (n.trim(), r)) else {
            continue;
        };
        // Skip variable assignments (`x := y`, `x ::= y`, `x :::= y`).
        // After splitting the first colon, GNU Make's longer assignment forms
        // leave one or more leading colons before the equals sign.
        if rest.trim_start().trim_start_matches(':').starts_with('=') {
            continue;
        }
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
/// Skips assignments (`:=`), aliases, and `set`/`export` directives.
fn merge_justfile_recipes(root: &Path, scripts: &mut BTreeMap<String, String>) {
    for filename in ["justfile", "Justfile"] {
        let path = root.join(filename);
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        for raw_line in raw.lines() {
            // Just recipe bodies are indented with spaces or tabs. Preserve that
            // signal until after classification so `echo http://…` is not
            // mistaken for a top-level `echo` recipe.
            if raw_line.len() != raw_line.trim_start().len() {
                continue;
            }
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            // Assignments and settings use `:=` — never recipe headers.
            if line.contains(":=") {
                continue;
            }
            let Some(first) = line.split_whitespace().next() else {
                continue;
            };
            // `alias name := recipe` already excluded by `:=` check; also skip
            // bare `alias` / `set` / `export` / `import` keywords.
            if matches!(first, "alias" | "set" | "export" | "import" | "mod") {
                continue;
            }
            let name = first.trim_end_matches(':');
            if name == first {
                // No trailing colon on the first token — only a recipe if the
                // line still has a `name args:` form (colon later on the line).
                if !line.contains(':') {
                    continue;
                }
            }
            if name.is_empty()
                || name.starts_with('@')
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                continue;
            }
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
    fn bun_package_scripts_use_bun_run() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{
  "name": "bun-app",
  "packageManager": "bun@1.2.0",
  "scripts": {
    "test": "vitest run",
    "build": "vite build",
    "dev": "vite"
  }
}"#,
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.test_command.as_deref(), Some("bun run test"));
        assert_eq!(profile.build_command.as_deref(), Some("bun run build"));
        assert_eq!(profile.dev_command.as_deref(), Some("bun run dev"));
    }

    #[test]
    fn bun_lockfile_selects_bun_for_package_scripts() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{ "name": "bun-lock-app", "scripts": { "build": "vite build" } }"#,
        )
        .unwrap();
        fs::write(tmp.path().join("bun.lock"), "").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.package_manager.as_deref(), Some("bun"));
        assert_eq!(profile.build_command.as_deref(), Some("bun run build"));
    }

    #[test]
    fn package_script_named_run_keeps_its_name() {
        assert_eq!(
            qualify_script_command(Some("pnpm"), "run", Some("node app.js")).as_deref(),
            Some("pnpm run run")
        );
        assert_eq!(
            qualify_script_command(Some("yarn"), "run", Some("node app.js")).as_deref(),
            Some("yarn run run")
        );
    }

    #[test]
    fn node_framework_defaults_fill_missing_scripts() {
        // package.json declares next but omits scripts — learn fills framework
        // defaults so the profile still carries build/dev/start.
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
        // Invented defaults must execute the local tool (not `pnpm <missing>`).
        assert_eq!(
            profile.scripts.get("dev").map(String::as_str),
            Some("pnpm exec next dev")
        );
        assert_eq!(
            profile.scripts.get("build").map(String::as_str),
            Some("pnpm exec next build")
        );
        assert_eq!(profile.dev_command.as_deref(), Some("pnpm exec next dev"));
        assert_eq!(
            profile.build_command.as_deref(),
            Some("pnpm exec next build")
        );
        assert_eq!(profile.run_command.as_deref(), Some("pnpm exec next start"));
        // Next.js 16 removed `next lint` — do not invent it.
        assert!(!profile.scripts.contains_key("lint"));
        assert!(profile.lint_command.is_none());
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
    fn node_makefile_role_is_preserved_not_pm_qualified() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{ "name": "node-make", "packageManager": "pnpm@9.0.0" }"#,
        )
        .unwrap();
        fs::write(
            tmp.path().join("Makefile"),
            "build:\n\techo build\ntest:\n\techo test\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert_eq!(
            profile.scripts.get("build").map(String::as_str),
            Some("make build")
        );
        assert_eq!(profile.build_command.as_deref(), Some("make build"));
        assert_eq!(profile.test_command.as_deref(), Some("make test"));
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
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

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
    fn rust_lib_only_crate_omits_run_commands() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "demo-lib"
version = "0.1.0"
edition = "2024"
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/lib.rs"), "").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert_eq!(profile.test_command.as_deref(), Some("cargo test"));
        assert_eq!(profile.build_command.as_deref(), Some("cargo build"));
        assert!(profile.dev_command.is_none());
        assert!(profile.run_command.is_none());
        assert!(profile.debug_command.is_none());
        assert!(!profile.scripts.contains_key("run"));
    }

    #[test]
    fn rust_multiple_binary_crate_omits_ambiguous_run_command() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "multi-bin"
version = "0.1.0"
edition = "2024"
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src/bin")).unwrap();
        fs::write(tmp.path().join("src/bin/one.rs"), "fn main() {}\n").unwrap();
        fs::write(tmp.path().join("src/bin/two.rs"), "fn main() {}\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
        assert!(profile.debug_command.is_none());
    }

    #[test]
    fn rust_autobins_false_does_not_invent_a_src_main_target() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "no-auto-bin"
version = "0.1.0"
edition = "2024"
autobins = false
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(!profile.scripts.contains_key("run"));
    }

    #[test]
    fn rust_2015_manual_target_disables_implicit_autobins() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "legacy-lib"
version = "0.1.0"
edition = "2015"

[lib]
path = "src/lib.rs"
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
        assert!(profile.debug_command.is_none());
        assert!(!profile.scripts.contains_key("run"));
    }

    #[test]
    fn rust_required_feature_binary_omits_unqualified_run_command() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "feature-bin"
version = "0.1.0"
edition = "2024"
autobins = false

[[bin]]
name = "feature-bin"
path = "src/feature.rs"
required-features = ["cli"]
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/feature.rs"), "fn main() {}\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(!profile.scripts.contains_key("run"));
    }

    #[test]
    fn rust_default_enabled_required_feature_keeps_run_command() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "default-feature-bin"
version = "0.1.0"
edition = "2024"
autobins = false

[features]
default = ["cli"]
cli = []

[[bin]]
name = "default-feature-bin"
path = "src/main.rs"
required-features = ["cli"]
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.run_command.as_deref(), Some("cargo run"));
        assert_eq!(profile.dev_command.as_deref(), Some("cargo run"));
        assert_eq!(
            profile.debug_command.as_deref(),
            Some("RUST_BACKTRACE=1 cargo run")
        );
        assert_eq!(
            profile.scripts.get("run").map(String::as_str),
            Some("cargo run")
        );
    }

    #[test]
    fn rust_transitive_default_feature_keeps_run_command() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "transitive-feature-bin"
version = "0.1.0"
edition = "2024"
autobins = false

[features]
default = ["full"]
full = ["cli"]
cli = []

[[bin]]
name = "transitive-feature-bin"
path = "src/main.rs"
required-features = ["cli"]
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.run_command.as_deref(), Some("cargo run"));
        assert_eq!(profile.dev_command.as_deref(), Some("cargo run"));
    }

    #[test]
    fn rust_nonweak_dependency_feature_enables_required_feature() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "dependency-feature-bin"
version = "0.1.0"
edition = "2024"
autobins = false

[dependencies]
helper = { path = "helper", optional = true }

[features]
default = ["helper/derive"]

[[bin]]
name = "dependency-feature-bin"
path = "src/main.rs"
required-features = ["helper"]
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::create_dir_all(tmp.path().join("helper/src")).unwrap();
        fs::write(
            tmp.path().join("helper/Cargo.toml"),
            r#"[package]
name = "helper"
version = "0.1.0"
edition = "2024"

[features]
derive = []
"#,
        )
        .unwrap();
        fs::write(tmp.path().join("helper/src/lib.rs"), "").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.run_command.as_deref(), Some("cargo run"));
        assert_eq!(profile.dev_command.as_deref(), Some("cargo run"));
    }

    #[test]
    fn rust_weak_dependency_feature_does_not_enable_required_feature() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "weak-dependency-feature-bin"
version = "0.1.0"
edition = "2024"
autobins = false

[dependencies]
helper = { path = "helper", optional = true }

[features]
default = ["helper?/derive"]

[[bin]]
name = "weak-dependency-feature-bin"
path = "src/main.rs"
required-features = ["helper"]
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::create_dir_all(tmp.path().join("helper/src")).unwrap();
        fs::write(
            tmp.path().join("helper/Cargo.toml"),
            r#"[package]
name = "helper"
version = "0.1.0"
edition = "2024"

[features]
derive = []
"#,
        )
        .unwrap();
        fs::write(tmp.path().join("helper/src/lib.rs"), "").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
    }

    #[test]
    fn rust_unresolved_explicit_bin_does_not_fall_back_to_src_main() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "primary"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "worker"
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
        assert!(profile.debug_command.is_none());
        assert!(!profile.scripts.contains_key("run"));
    }

    #[test]
    fn rust_explicit_bin_rename_does_not_double_count_src_main() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "renamed-bin"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "cli"
path = "src/main.rs"
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.run_command.as_deref(), Some("cargo run"));
    }

    #[test]
    fn rust_feature_gated_src_main_override_omits_run_command() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "gated-main"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "gated-main"
path = "src/main.rs"
required-features = ["cli"]
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(!profile.scripts.contains_key("run"));
    }

    #[test]
    fn go_profile_includes_common_commands() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/svc\n\ngo 1.22\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("main.go"),
            "package main\n\nfunc main() {}\n",
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
    fn go_root_main_package_does_not_require_main_go_filename() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/svc\n\ngo 1.22\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("server.go"),
            "package main\n\nfunc main() {}\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.run_command.as_deref(), Some("go run ."));
        assert_eq!(profile.dev_command.as_deref(), Some("go run ."));
    }

    #[test]
    fn go_ignored_and_nonmatching_constrained_files_do_not_create_run_command() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/constrained\n\ngo 1.22\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("_main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .unwrap();
        let other_os = if cfg!(target_os = "windows") {
            "linux"
        } else {
            "windows"
        };
        fs::write(
            tmp.path().join(format!("main_{other_os}.go")),
            "package main\n\nfunc main() {}\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("tagged.go"),
            "//go:build quickrunner_never\n\npackage main\n\nfunc main() {}\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
    }

    #[test]
    fn go_build_constraint_accepts_tab_whitespace() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/tab-constrained\n\ngo 1.22\n",
        )
        .unwrap();
        let other_os = if cfg!(target_os = "windows") {
            "linux"
        } else {
            "windows"
        };
        fs::write(
            tmp.path().join("server.go"),
            format!("//go:build\t{other_os}\n\npackage main\n\nfunc main() {{}}\n"),
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
    }

    #[test]
    fn go_build_constraint_text_inside_block_comment_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/block-comment\n\ngo 1.22\n",
        )
        .unwrap();
        let other_os = if cfg!(target_os = "windows") {
            "linux"
        } else {
            "windows"
        };
        fs::write(
            tmp.path().join("server.go"),
            format!("/*\n//go:build {other_os}\n*/\n\npackage main\n\nfunc main() {{}}\n"),
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.run_command.as_deref(), Some("go run ."));
        assert_eq!(profile.dev_command.as_deref(), Some("go run ."));
    }

    #[test]
    fn go_custom_tags_from_goflags_are_respected() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/custom-tags\n\ngo 1.22\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("server.go"),
            "//go:build !enterprise\n\npackage main\n\nfunc main() {}\n",
        )
        .unwrap();

        let profile = {
            let _guard = crate::test_env_lock().lock().unwrap();
            let previous = std::env::var_os("GOFLAGS");
            unsafe {
                std::env::set_var("GOFLAGS", "-tags=enterprise");
            }
            let profile = detect_profile(tmp.path());
            unsafe {
                match previous {
                    Some(value) => std::env::set_var("GOFLAGS", value),
                    None => std::env::remove_var("GOFLAGS"),
                }
            }
            profile
        }
        .unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
    }

    #[test]
    fn malformed_goflags_tag_value_is_conservatively_unknown() {
        assert!(parse_go_user_build_tags(Some("-tags='enterprise'")).is_none());
    }

    #[test]
    fn go_package_and_main_function_must_come_from_one_package() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/mixed-package\n\ngo 1.22\n",
        )
        .unwrap();
        fs::write(tmp.path().join("app.go"), "package main\n").unwrap();
        fs::write(
            tmp.path().join("helper.go"),
            "package helper\n\nfunc main() {}\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
    }

    #[test]
    fn go_legacy_plus_build_constraint_is_respected() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/legacy-constrained\n\ngo 1.22\n",
        )
        .unwrap();
        let other_os = if cfg!(target_os = "windows") {
            "linux"
        } else {
            "windows"
        };
        fs::write(
            tmp.path().join("main.go"),
            format!("// +build {other_os}\n\npackage main\n\nfunc main() {{}}\n"),
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
        assert!(profile.debug_command.is_none());
    }

    #[test]
    fn go_declarations_inside_comments_do_not_create_run_command() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/commented-declarations\n\ngo 1.22\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("commented.go"),
            "package helper\n\n/*\npackage main\nfunc main() {}\n*/\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
        assert!(profile.debug_command.is_none());
    }

    #[test]
    fn go_release_build_tag_at_module_version_is_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/release-tagged\n\ngo 1.20\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("main.go"),
            "//go:build go1.20\n\npackage main\n\nfunc main() {}\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.run_command.as_deref(), Some("go run ."));
        assert_eq!(profile.dev_command.as_deref(), Some("go run ."));
    }

    #[test]
    fn go_explicit_cgo_build_tag_is_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/cgo-tagged\n\ngo 1.22\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("main.go"),
            "//go:build cgo\n\npackage main\n\nfunc main() {}\n",
        )
        .unwrap();

        let _guard = crate::test_env_lock().lock().unwrap();
        let previous = std::env::var_os("CGO_ENABLED");
        unsafe {
            std::env::set_var("CGO_ENABLED", "1");
        }
        let profile = detect_profile(tmp.path());
        unsafe {
            match previous {
                Some(value) => std::env::set_var("CGO_ENABLED", value),
                None => std::env::remove_var("CGO_ENABLED"),
            }
        }
        let profile = profile.unwrap();

        assert_eq!(profile.run_command.as_deref(), Some("go run ."));
    }

    #[test]
    fn go_unknown_cgo_state_does_not_enable_negated_tag() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/not-cgo-tagged\n\ngo 1.22\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("main.go"),
            "//go:build !cgo\n\npackage main\n\nfunc main() {}\n",
        )
        .unwrap();

        let _guard = crate::test_env_lock().lock().unwrap();
        let previous = std::env::var_os("CGO_ENABLED");
        unsafe {
            std::env::remove_var("CGO_ENABLED");
        }
        let profile = detect_profile(tmp.path());
        unsafe {
            if let Some(value) = previous {
                std::env::set_var("CGO_ENABLED", value);
            }
        }
        let profile = profile.unwrap();

        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
    }

    #[test]
    fn go_explicit_arch_feature_build_tag_is_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/arch-tagged\n\ngo 1.22\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("main.go"),
            "//go:build amd64.v2\n\npackage main\n\nfunc main() {}\n",
        )
        .unwrap();

        let _guard = crate::test_env_lock().lock().unwrap();
        let previous_arch = std::env::var_os("GOARCH");
        let previous_level = std::env::var_os("GOAMD64");
        unsafe {
            std::env::set_var("GOARCH", "amd64");
            std::env::set_var("GOAMD64", "v2");
        }
        let profile = detect_profile(tmp.path());
        unsafe {
            match previous_arch {
                Some(value) => std::env::set_var("GOARCH", value),
                None => std::env::remove_var("GOARCH"),
            }
            match previous_level {
                Some(value) => std::env::set_var("GOAMD64", value),
                None => std::env::remove_var("GOAMD64"),
            }
        }
        let profile = profile.unwrap();

        assert_eq!(profile.run_command.as_deref(), Some("go run ."));
    }

    #[test]
    fn go_os_word_before_terminal_extra_suffix_is_not_a_constraint() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/nonterminal-os-word\n\ngo 1.22\n",
        )
        .unwrap();
        let other_os = if cfg!(target_os = "windows") {
            "linux"
        } else {
            "windows"
        };
        fs::write(
            tmp.path().join(format!("main_{other_os}_extra.go")),
            "package main\n\nfunc main() {}\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.run_command.as_deref(), Some("go run ."));
        assert_eq!(profile.dev_command.as_deref(), Some("go run ."));
    }

    #[test]
    fn go_cmd_only_module_omits_root_run_commands() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module github.com/acme/svc\n\ngo 1.22\n",
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("cmd/svc")).unwrap();
        fs::write(
            tmp.path().join("cmd/svc/main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert_eq!(profile.build_command.as_deref(), Some("go build ./..."));
        assert!(profile.dev_command.is_none());
        assert!(profile.run_command.is_none());
        assert!(profile.debug_command.is_none());
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
    fn plain_main_py_is_not_fastapi() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            r#"[project]
name = "script-py"
version = "0.1.0"
dependencies = []
"#,
        )
        .unwrap();
        fs::write(tmp.path().join("main.py"), "print('hi')\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert!(profile.framework.is_none());
        assert_eq!(profile.dev_command.as_deref(), Some("python main.py"));
        assert_eq!(profile.run_command.as_deref(), Some("python main.py"));
        assert!(
            !profile
                .dev_command
                .as_deref()
                .unwrap_or("")
                .contains("uvicorn")
        );
    }

    #[test]
    fn plain_app_py_is_not_assumed_to_be_fastapi() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            r#"[project]
name = "plain-app"
version = "0.1.0"
dependencies = []
"#,
        )
        .unwrap();
        fs::write(tmp.path().join("app.py"), "print('hi')\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert!(profile.framework.is_none());
        assert_eq!(profile.run_command.as_deref(), Some("python app.py"));
        assert_eq!(profile.dev_command.as_deref(), Some("python app.py"));
    }

    #[test]
    fn fastapi_without_a_known_root_module_omits_run_commands() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            r#"[project]
name = "nested-fastapi"
version = "0.1.0"
dependencies = ["fastapi"]
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src/service")).unwrap();
        fs::write(tmp.path().join("src/service/app.py"), "app = None\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.framework.as_deref(), Some("fastapi"));
        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
        assert!(profile.debug_command.is_none());
    }

    #[test]
    fn django_dependency_without_manage_py_omits_run_commands() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            r#"[project]
name = "django-package"
version = "0.1.0"
dependencies = ["django"]
"#,
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(profile.framework.as_deref(), Some("django"));
        assert!(profile.run_command.is_none());
        assert!(profile.dev_command.is_none());
        assert!(profile.debug_command.is_none());
    }

    #[test]
    fn pipenv_project_prefixes_common_commands() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            r#"[project]
name = "pipenv-py"
version = "0.1.0"
dependencies = ["flask"]
"#,
        )
        .unwrap();
        fs::write(tmp.path().join("Pipfile"), "[packages]\nflask = \"*\"\n").unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert_eq!(profile.package_manager.as_deref(), Some("pipenv"));
        assert_eq!(profile.test_command.as_deref(), Some("pipenv run pytest"));
        assert!(profile.dev_command.is_none());
        assert!(profile.run_command.is_none());
    }

    #[test]
    fn makefile_targets_merge_into_scripts_without_clobbering() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            tmp.path().join("Makefile"),
            "\
.PHONY: test deploy
CC := gcc
test := should-not-be-a-target
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
        assert!(!profile.scripts.contains_key("CC"));
        // `test := …` must not invent a make target either (cargo already owns test).
        assert_ne!(
            profile.scripts.get("test").map(String::as_str),
            Some("make test")
        );
    }

    #[test]
    fn makefile_assignment_only_project_does_not_invent_roles() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{ "name": "make-vars" }"#,
        )
        .unwrap();
        fs::write(
            tmp.path().join("Makefile"),
            "VERSION := 1.2.3\ntest := pytest\nCC ?= gcc\nCACHE ::= value\nSHELL :::= value\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert!(!profile.scripts.contains_key("VERSION"));
        assert!(!profile.scripts.contains_key("test"));
        assert!(!profile.scripts.contains_key("CC"));
        assert!(!profile.scripts.contains_key("CACHE"));
        assert!(!profile.scripts.contains_key("SHELL"));
        assert!(profile.test_command.is_none());
    }

    #[test]
    fn justfile_skips_assignments_and_settings() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{ "name": "just-demo" }"#,
        )
        .unwrap();
        fs::write(
            tmp.path().join("justfile"),
            r#"
set shell := ["bash", "-cu"]
export RUST_BACKTRACE := "1"
target := "main"
alias b := build

build:
    echo build

test:
    echo test
"#,
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();
        assert_eq!(
            profile.scripts.get("build").map(String::as_str),
            Some("just build")
        );
        assert_eq!(
            profile.scripts.get("test").map(String::as_str),
            Some("just test")
        );
        assert!(!profile.scripts.contains_key("set"));
        assert!(!profile.scripts.contains_key("export"));
        assert!(!profile.scripts.contains_key("target"));
        assert!(!profile.scripts.contains_key("alias"));
        assert!(!profile.scripts.contains_key("b"));
        assert_eq!(profile.build_command.as_deref(), Some("just build"));
        assert_eq!(profile.test_command.as_deref(), Some("just test"));
    }

    #[test]
    fn justfile_skips_indented_recipe_body_lines_with_colons() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{ "name": "just-body" }"#,
        )
        .unwrap();
        fs::write(
            tmp.path().join("justfile"),
            "build:\n    echo http://example.com\n    printf 'status: ok'\n",
        )
        .unwrap();

        let profile = detect_profile(tmp.path()).unwrap();

        assert_eq!(
            profile.scripts.get("build").map(String::as_str),
            Some("just build")
        );
        assert!(!profile.scripts.contains_key("echo"));
        assert!(!profile.scripts.contains_key("printf"));
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
