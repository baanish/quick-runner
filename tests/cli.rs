use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    sync::{Mutex, OnceLock},
    thread,
};

use quick_runner::stats_db::StatsDb;

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn clear_test_env() {
    for key in ["QR_CONFIG_DIR", "QR_TEST_AI_KEY"] {
        unsafe {
            std::env::remove_var(key);
        }
    }
}

#[test]
fn help_lists_core_commands() {
    let mut cmd = Command::cargo_bin("qr").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(contains("go"))
        .stdout(contains("run"))
        .stdout(contains("alias"))
        .stdout(contains("stats"))
        .stdout(contains("scan"))
        .stdout(contains("init"));
}

#[test]
fn learn_generates_profile_json_for_current_project() {
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
    "next": "15.0.0",
    "react": "19.0.0"
  }
}"#,
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("qr").unwrap();
    cmd.current_dir(tmp.path())
        .arg("learn")
        .assert()
        .success()
        .stdout(contains("demo-app"))
        .stdout(contains("pnpm"))
        .stdout(contains("next"));

    let raw = fs::read_to_string(tmp.path().join(".qr/profile.json")).unwrap();
    let profile: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(profile["name"], "demo-app");
    assert_eq!(profile["package_manager"], "pnpm");
    assert_eq!(profile["framework"], "nextjs");
    assert_eq!(profile["test_command"], "pnpm test");
}

#[test]
fn learn_short_alias_works() {
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

    let mut cmd = Command::cargo_bin("qr").unwrap();
    cmd.current_dir(tmp.path())
        .arg("l")
        .assert()
        .success()
        .stdout(contains("demo-rs"))
        .stdout(contains("rust"));
}

#[test]
fn config_path_prints_config_file_path() {
    let _guard = env_lock().lock().unwrap();
    clear_test_env();
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join("cfg");
    fs::create_dir_all(&cfg_dir).unwrap();

    unsafe {
        std::env::set_var("QR_CONFIG_DIR", &cfg_dir);
    }

    let expected = cfg_dir.join("config.toml");

    let mut cmd = Command::cargo_bin("qr").unwrap();
    cmd.arg("config")
        .arg("path")
        .assert()
        .success()
        .stdout(format!("{}\n", expected.display()));

    unsafe {
        std::env::remove_var("QR_CONFIG_DIR");
    }
}

#[test]
fn do_routes_inline_tasks_with_preview() {
    let _guard = env_lock().lock().unwrap();
    clear_test_env();
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join("cfg");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::create_dir_all(tmp.path().join(".qr")).unwrap();
    let server = spawn_server(
        200,
        r#"{"choices":[{"message":{"content":"{\"classification\":\"inline\",\"command\":\"cargo test\"}"}}],"usage":{"prompt_tokens":8,"completion_tokens":6}}"#,
    );

    fs::write(
        cfg_dir.join("config.toml"),
        format!(
            r#"[general]
default_run_mode = "output"

[projects]
roots = ["~/Development"]
scan_depth = 2
scan_interval_hours = 1

[ai]
protocol = "openai"
base_url = "{server}"
model = "demo"
api_key = ""
api_key_env = "QR_TEST_AI_KEY"

[stats]
enabled = false
db_path = "__default__"
"#
        ),
    )
    .unwrap();
    fs::write(
        tmp.path().join(".qr/profile.json"),
        r#"{"name":"demo-rs","language":"rust","framework":null,"package_manager":"cargo","test_command":"cargo test","build_command":"cargo build","lint_command":"cargo clippy","scripts":{"test":"cargo test"},"prefer_agent":null,"entry_points":["src/main.rs"]}"#,
    )
    .unwrap();

    unsafe {
        std::env::set_var("QR_CONFIG_DIR", &cfg_dir);
        std::env::set_var("QR_TEST_AI_KEY", "token");
    }

    let mut cmd = Command::cargo_bin("qr").unwrap();
    cmd.current_dir(tmp.path())
        .arg("do")
        .arg("run tests")
        .write_stdin("n\n")
        .assert()
        .success()
        .stdout(contains("cargo test"))
        .stdout(contains("Run?"));

    unsafe {
        std::env::remove_var("QR_CONFIG_DIR");
        std::env::remove_var("QR_TEST_AI_KEY");
    }
}

#[test]
fn do_records_ai_run_when_stats_disabled() {
    let _guard = env_lock().lock().unwrap();
    clear_test_env();
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join("cfg");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::create_dir_all(tmp.path().join(".qr")).unwrap();
    let server = spawn_server(
        200,
        r#"{"choices":[{"message":{"content":"{\"classification\":\"inline\",\"command\":\"cargo test\"}"}}],"usage":{"prompt_tokens":8,"completion_tokens":6}}"#,
    );

    fs::write(
        cfg_dir.join("config.toml"),
        format!(
            r#"[general]
default_run_mode = "output"

[projects]
roots = ["~/Development"]
scan_depth = 2
scan_interval_hours = 1

[ai]
protocol = "openai"
base_url = "{server}"
model = "demo"
api_key = ""
api_key_env = "QR_TEST_AI_KEY"

[stats]
enabled = false
db_path = "__default__"
"#
        ),
    )
    .unwrap();
    fs::write(
        tmp.path().join(".qr/profile.json"),
        r#"{"name":"demo-rs","language":"rust","framework":null,"package_manager":"cargo","test_command":"cargo test","build_command":"cargo build","lint_command":"cargo clippy","scripts":{"test":"cargo test"},"prefer_agent":null,"entry_points":["src/main.rs"]}"#,
    )
    .unwrap();

    unsafe {
        std::env::set_var("QR_CONFIG_DIR", &cfg_dir);
        std::env::set_var("QR_TEST_AI_KEY", "token");
    }

    let mut cmd = Command::cargo_bin("qr").unwrap();
    cmd.current_dir(tmp.path())
        .arg("do")
        .arg("run tests")
        .write_stdin("n\n")
        .assert()
        .success();

    let db = StatsDb::open(&cfg_dir.join("stats.db")).unwrap();
    let summary = db.summary().unwrap();
    assert_eq!(summary.total_runs, 1);
    assert_eq!(summary.ai_assisted_runs, 1);
    assert_eq!(summary.input_tokens, 8);
    assert_eq!(summary.output_tokens, 6);

    unsafe {
        std::env::remove_var("QR_CONFIG_DIR");
        std::env::remove_var("QR_TEST_AI_KEY");
    }
}

#[test]
fn stats_command_mentions_disabled_non_ai_tracking() {
    let _guard = env_lock().lock().unwrap();
    clear_test_env();
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join("cfg");
    fs::create_dir_all(&cfg_dir).unwrap();

    fs::write(
        cfg_dir.join("config.toml"),
        r#"[general]
default_run_mode = "output"

[projects]
roots = ["~/Development"]
scan_depth = 2
scan_interval_hours = 1

[ai]
protocol = "anthropic"
base_url = "https://api.anthropic.com"
model = "demo"
api_key = "demo-key"
api_key_env = "ANTHROPIC_API_KEY"

[stats]
enabled = false
db_path = "__default__"
"#,
    )
    .unwrap();

    unsafe {
        std::env::set_var("QR_CONFIG_DIR", &cfg_dir);
    }

    let mut cmd = Command::cargo_bin("qr").unwrap();
    cmd.arg("stats")
        .assert()
        .success()
        .stdout(contains(
            "ℹ Stats tracking is disabled for non-AI commands. Run qr config to enable.",
        ));

    unsafe {
        std::env::remove_var("QR_CONFIG_DIR");
    }
}

#[test]
fn init_collects_ai_provider_settings_and_writes_secure_config() {
    let _guard = env_lock().lock().unwrap();
    clear_test_env();
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join("cfg");
    let project_root = tmp.path().join("projects");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::create_dir_all(&project_root).unwrap();

    unsafe {
        std::env::set_var("QR_CONFIG_DIR", &cfg_dir);
    }

    let mut cmd = Command::cargo_bin("qr").unwrap();
    cmd.arg("init")
        .arg("--no-shell-wrapper")
        .arg("--no-cron")
        .write_stdin(format!(
            "{root}\n\nopenai-compatible\n\nopenai-model\nconfig-primary-key\n\n\
y\nanthropic-compatible\n\nclaude-fallback\nconfig-fallback-key\nFALLBACK_SECRET\n",
            root = project_root.display()
        ))
        .assert()
        .success()
        .stdout(contains("created"))
        .stdout(contains("initial scan found"));

    let config_path = cfg_dir.join("config.toml");
    let raw = fs::read_to_string(&config_path).unwrap();
    assert!(raw.contains(&format!("roots = [\"{}\"]", project_root.display())));
    assert!(raw.contains("protocol = \"openai\""));
    assert!(raw.contains("base_url = \"https://api.openai.com/v1\""));
    assert!(raw.contains("model = \"openai-model\""));
    assert!(raw.contains("api_key = \"config-primary-key\""));
    assert!(raw.contains("api_key_env = \"OPENAI_API_KEY\""));
    assert!(raw.contains("[ai.fallback]"));
    assert!(raw.contains("protocol = \"anthropic\""));
    assert!(raw.contains("base_url = \"https://api.anthropic.com\""));
    assert!(raw.contains("model = \"claude-fallback\""));
    assert!(raw.contains("api_key = \"config-fallback-key\""));
    assert!(raw.contains("api_key_env = \"FALLBACK_SECRET\""));

    let mode = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    unsafe {
        std::env::remove_var("QR_CONFIG_DIR");
    }
}

#[test]
fn init_prompts_before_installing_cron_and_skips_on_no() {
    let _guard = env_lock().lock().unwrap();
    clear_test_env();
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join("cfg");
    let project_root = tmp.path().join("projects");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::create_dir_all(&project_root).unwrap();

    unsafe {
        std::env::set_var("QR_CONFIG_DIR", &cfg_dir);
    }

    // No `--no-cron`, so init must prompt. Answering "n" must skip the crontab
    // entirely (install_cron is never reached, so it cannot touch the real crontab).
    let mut cmd = Command::cargo_bin("qr").unwrap();
    cmd.arg("init")
        .arg("--no-shell-wrapper")
        .write_stdin(format!(
            "{root}\n\nopenai-compatible\n\nopenai-model\nprimary-key\n\nn\nn\n",
            root = project_root.display()
        ))
        .assert()
        .success()
        .stdout(contains("Install hourly project rescan cron?"))
        .stdout(contains("installed hourly scan cron").not())
        .stdout(contains("initial scan found"));

    unsafe {
        std::env::remove_var("QR_CONFIG_DIR");
    }
}

fn spawn_server(status: u16, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });

    format!("http://{addr}/v1")
}
