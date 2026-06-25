//! Property-based fuzz tests for quick-runner's parsing and escaping surfaces.
//!
//! These complement the per-module unit tests: they hammer the public API with
//! thousands of adversarial inputs to guard against panics and round-trip
//! corruption. Run more cases with e.g. `PROPTEST_CASES=5000 cargo test`.

use std::process::Command;

use proptest::prelude::*;
use quick_runner::{
    commands::run::RunMode,
    config::AppConfig,
    pricing::Price,
    project_profile::detect_profile,
    scanner::{ProjectCache, ProjectEntry, read_project_cache, write_project_cache},
    shell::{ShellKind, add_or_update_alias, cron_line, load_aliases},
};

// ---- parsers must never panic on untrusted input -------------------------

proptest! {
    #[test]
    fn config_parse_never_panics(s in ".{0,400}") {
        let _ = AppConfig::load_from_str(&s);
    }

    #[test]
    fn config_parse_never_panics_on_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
        if let Ok(s) = String::from_utf8(bytes) {
            let _ = AppConfig::load_from_str(&s);
        }
    }

    #[test]
    fn project_cache_read_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects-cache.json");
        std::fs::write(&path, &bytes).unwrap();
        let _ = read_project_cache(&path);
    }

    #[test]
    fn project_cache_round_trips(name in ".{0,40}", path in ".{0,40}", source in ".{0,16}") {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cache.json");
        let cache = ProjectCache {
            scanned_at_unix_ms: 1,
            projects: vec![ProjectEntry { name: name.clone(), path: path.clone(), source }],
        };
        write_project_cache(&file, &cache).unwrap();
        let restored = read_project_cache(&file).unwrap();
        prop_assert_eq!(restored.projects[0].name.clone(), name);
        prop_assert_eq!(restored.projects[0].path.clone(), path);
    }

    #[test]
    fn run_mode_parse_never_panics(s in ".{0,20}") {
        let _ = RunMode::parse(&s);
    }

    #[test]
    fn price_cost_is_finite(input in -1e9f64..1e9, output in -1e9f64..1e9, it in any::<u32>(), ot in any::<u32>()) {
        let cost = Price { input, output }.cost(it as u64, ot as u64);
        prop_assert!(cost.is_finite());
    }

    // Arbitrary bytes in each project-marker file must never panic detection.
    #[test]
    fn detect_profile_never_panics(which in 0usize..4, body in ".{0,200}") {
        let dir = tempfile::tempdir().unwrap();
        let marker = ["package.json", "Cargo.toml", "pyproject.toml", "go.mod"][which];
        std::fs::write(dir.path().join(marker), &body).unwrap();
        let _ = detect_profile(dir.path());
    }
}

// ---- shell escaping & alias round-trips (the security-relevant surface) ----

fn sh_printf(quoted: &str) -> Option<String> {
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("printf %s {quoted}"))
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

// Shell-dangerous alphabet (quotes, backslash, $, backtick, pipes, tabs, ...)
// but never NUL (which std::process can't pass as an argument). `_nl` includes a
// newline for escaping checks; the alias storage variant excludes it because a
// newline is a rejected, line-corrupting input handled by a dedicated unit test.
fn shellish_inline() -> impl Strategy<Value = String> {
    let alphabet: Vec<char> = " _.-aZ09'\"$`;|&<>(){}*?~!#=\\\t".chars().collect();
    proptest::collection::vec(proptest::sample::select(alphabet), 0..40)
        .prop_map(|cs| cs.into_iter().collect())
}

fn shellish_with_newline() -> impl Strategy<Value = String> {
    let alphabet: Vec<char> = " _.-aZ09'\"$`;|&<>(){}*?~!#=\\\n\t".chars().collect();
    proptest::collection::vec(proptest::sample::select(alphabet), 0..40)
        .prop_map(|cs| cs.into_iter().collect())
}

proptest! {
    // alias_line must let /bin/sh see the command as one literal word.
    #[test]
    fn alias_line_is_one_sh_word(name in "[A-Za-z0-9_]{1,16}", cmd in shellish_with_newline()) {
        let line = ShellKind::Zsh.alias_line(&name, &cmd);
        let quoted = line.strip_prefix(&format!("alias {name}=")).unwrap();
        if let Some(got) = sh_printf(quoted) {
            prop_assert_eq!(got, cmd);
        }
    }

    // cron_line must quote the binary path so /bin/sh sees it as one literal word.
    #[test]
    fn cron_line_quotes_path(path in shellish_inline()) {
        let line = cron_line(std::path::Path::new(&path));
        let body = line.splitn(6, ' ').nth(5).unwrap();
        let quoted = body.strip_suffix(" scan >/dev/null 2>&1").unwrap();
        if let Some(got) = sh_printf(quoted) {
            prop_assert_eq!(got, path);
        }
    }

    // Writing a single-line alias then loading it back must return the same
    // command, including embedded quotes/backslashes.
    #[test]
    fn alias_write_read_round_trips(name in "[A-Za-z0-9_]{1,16}", cmd in shellish_inline()) {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".zshrc");
        add_or_update_alias(&rc, ShellKind::Zsh, &name, &cmd).unwrap();
        let aliases = load_aliases(&rc).unwrap();
        let found = aliases.iter().find(|(n, _)| n == &name);
        prop_assert!(found.is_some(), "alias {:?} not found after write", name);
        prop_assert_eq!(&found.unwrap().1, &cmd, "command round-trip lost data");
    }
}
