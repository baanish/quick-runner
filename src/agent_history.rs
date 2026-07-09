//! Mine shell-like commands from coding-agent session histories.
//!
//! Supported agents (best-effort; formats drift, so parsers are defensive):
//! - **Claude Code** — `~/.claude/projects/<encoded-path>/*.jsonl` (`Bash` tool_use)
//! - **Codex** — `~/.codex/sessions/**/*.jsonl` (`exec_command` / shell, filtered by cwd)
//! - **Pi** — `~/.pi/agent/sessions/<encoded-path>/**/*.jsonl` (`bash` toolCall)
//! - **omp** (Pi fork) — `~/.omp/agent/sessions/<encoded-path>/**/*.jsonl` (same shape)
//! - **OpenCode** — `~/.local/share/opencode/opencode.db` (`part` bash tools for matching sessions)
//!
//! Opt-in via `[learn].mine_agent_history` (default off) or
//! `QR_LEARN_MINE_AGENT_HISTORY`.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Maximum distinct commands kept in the learned profile after ranking.
const MAX_MINED_COMMANDS: usize = 40;

/// Cap how many session files we open per agent root so learn stays snappy on
/// machines with huge histories.
const MAX_SESSION_FILES_PER_AGENT: usize = 200;

/// A command observed in agent session history for this project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MinedCommand {
    pub command: String,
    pub count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

/// Overrideable roots for agent session stores (tests inject fixtures here).
#[derive(Debug, Clone, Default)]
pub struct AgentHistoryRoots {
    /// Optional home directory used only for omp relative path encodings.
    /// Snapshotted once when resolving defaults so mining never re-reads
    /// process env mid-scan (avoids racing tests that mutate `HOME`).
    pub home: Option<PathBuf>,
    pub claude_projects: Option<PathBuf>,
    pub codex_sessions: Option<PathBuf>,
    pub pi_sessions: Option<PathBuf>,
    pub omp_sessions: Option<PathBuf>,
    pub opencode_db: Option<PathBuf>,
}

impl AgentHistoryRoots {
    /// Default on-disk locations under the current user's home directory.
    pub fn from_home(home: &Path) -> Self {
        Self {
            home: Some(home.to_path_buf()),
            claude_projects: Some(home.join(".claude/projects")),
            codex_sessions: Some(home.join(".codex/sessions")),
            pi_sessions: Some(home.join(".pi/agent/sessions")),
            omp_sessions: Some(home.join(".omp/agent/sessions")),
            opencode_db: Some(home.join(".local/share/opencode/opencode.db")),
        }
    }

    pub fn from_env_home() -> Self {
        let home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Self::from_home(&home)
    }
}

/// Mine command-like shell invocations from agent sessions associated with
/// `project_root`. Missing agent roots and unreadable files are skipped.
pub fn mine_for_project(project_root: &Path) -> Vec<MinedCommand> {
    mine_for_project_with_roots(project_root, &AgentHistoryRoots::from_env_home())
}

pub fn mine_for_project_with_roots(
    project_root: &Path,
    roots: &AgentHistoryRoots,
) -> Vec<MinedCommand> {
    // Keep both the path the caller passed and its canonical form. Agent
    // session dirs are encoded from whichever form the agent saw at runtime
    // (often the non-canonical `/var/...` on macOS), so dropping either side
    // misses history.
    let project_variants = path_variants(project_root);
    let project = project_variants
        .first()
        .cloned()
        .unwrap_or_else(|| project_root.to_path_buf());
    let mut counts: BTreeMap<String, (u32, BTreeSet<String>)> = BTreeMap::new();

    let home = roots.home.as_deref();
    if let Some(dir) = &roots.claude_projects {
        collect_from_dir_encoded(
            dir,
            &project,
            "claude",
            &claude_path_encodings(&project_variants),
            home,
            extract_claude_commands,
            &mut counts,
        );
    }
    if let Some(dir) = &roots.codex_sessions {
        collect_codex(dir, &project_variants, &mut counts);
    }
    if let Some(dir) = &roots.pi_sessions {
        collect_from_dir_encoded(
            dir,
            &project,
            "pi",
            &pi_path_encodings(&project_variants),
            home,
            extract_pi_omp_commands,
            &mut counts,
        );
    }
    if let Some(dir) = &roots.omp_sessions {
        // omp is a Pi fork; sessions use the same JSONL toolCall shape. Path
        // encoding is often relative to $HOME rather than absolute.
        collect_from_dir_encoded(
            dir,
            &project,
            "omp",
            &omp_path_encodings(&project_variants, home),
            home,
            extract_pi_omp_commands,
            &mut counts,
        );
    }
    if let Some(db) = &roots.opencode_db {
        collect_opencode(db, &project_variants, &mut counts);
    }

    rank_commands(counts)
}

fn path_variants(project_root: &Path) -> Vec<PathBuf> {
    let mut out = vec![project_root.to_path_buf()];
    let push_unique = |out: &mut Vec<PathBuf>, p: PathBuf| {
        if !out.iter().any(|existing| existing == &p) {
            out.push(p);
        }
    };
    if let Ok(canon) = project_root.canonicalize() {
        push_unique(&mut out, canon);
    }
    // macOS: `/var` is a symlink to `/private/var`. `current_dir()` often returns
    // the canonical form while agents encode whichever path they were launched
    // with — keep both so session lookup still hits.
    if let Ok(rest) = project_root.strip_prefix("/private") {
        push_unique(&mut out, PathBuf::from("/").join(rest));
    }
    for existing in out.clone() {
        if let Ok(rest) = existing.strip_prefix("/private") {
            push_unique(&mut out, PathBuf::from("/").join(rest));
        }
        if existing.starts_with("/var") {
            let mut private = PathBuf::from("/private");
            private.push(existing.strip_prefix("/").unwrap_or(existing.as_path()));
            push_unique(&mut out, private);
        }
    }
    out
}

fn collect_from_dir_encoded(
    root: &Path,
    project: &Path,
    source: &str,
    encodings: &[String],
    home: Option<&Path>,
    extract: fn(&str) -> Vec<String>,
    counts: &mut BTreeMap<String, (u32, BTreeSet<String>)>,
) {
    if !root.is_dir() {
        return;
    }

    let mut session_dirs: Vec<PathBuf> = encodings
        .iter()
        .map(|enc| root.join(enc))
        .filter(|p| p.is_dir())
        .collect();

    // Fallback: any child dir whose name encodes this project path.
    if session_dirs.is_empty() {
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if (encodings.iter().any(|enc| &name == enc)
                    || path_encoding_matches_project(&name, project, home))
                    && entry.path().is_dir()
                {
                    session_dirs.push(entry.path());
                }
            }
        }
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for dir in session_dirs {
        collect_jsonl_files(&dir, &mut files);
    }

    files.truncate(MAX_SESSION_FILES_PER_AGENT);
    for file in files {
        scan_jsonl_file(&file, source, extract, counts);
    }
}

fn collect_codex(
    sessions_root: &Path,
    project_variants: &[PathBuf],
    counts: &mut BTreeMap<String, (u32, BTreeSet<String>)>,
) {
    if !sessions_root.is_dir() {
        return;
    }
    // Collect broadly first, then keep only sessions whose cwd matches this
    // project (or a subdir). Cap *after* filtering so busy multi-project
    // machines still mine older-but-relevant sessions for the current project.
    let mut candidates = Vec::new();
    collect_jsonl_files_uncapped(sessions_root, &mut candidates);

    let mut matched = 0usize;
    for file in candidates {
        if matched >= MAX_SESSION_FILES_PER_AGENT {
            break;
        }
        let Ok(raw) = fs::read_to_string(&file) else {
            continue;
        };
        // Only mine sessions whose meta cwd is this project (or a subdir).
        let mut belongs = false;
        for line in raw.lines().take(20) {
            if let Ok(v) = serde_json::from_str::<JsonValue>(line) {
                if v.get("type").and_then(JsonValue::as_str) == Some("session_meta") {
                    if let Some(cwd) = v
                        .pointer("/payload/cwd")
                        .and_then(JsonValue::as_str)
                        .map(PathBuf::from)
                    {
                        let cwd_variants = path_variants(&cwd);
                        if project_variants.iter().any(|project| {
                            cwd_variants.iter().any(|cwd| paths_related(cwd, project))
                        }) {
                            belongs = true;
                        }
                    }
                    break;
                }
            }
        }
        if !belongs {
            continue;
        }
        matched = matched.saturating_add(1);
        for line in raw.lines() {
            for cmd in extract_codex_commands(line) {
                record_command(counts, &cmd, "codex");
            }
        }
    }
}

fn collect_opencode(
    db_path: &Path,
    project_variants: &[PathBuf],
    counts: &mut BTreeMap<String, (u32, BTreeSet<String>)>,
) {
    if !db_path.is_file() {
        return;
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return;
    };

    // Session directory or project worktree must match this project.
    // Query each path variant (canonical + as-given) because agents store either.
    // Escape LIKE wildcards in the project path so `_` / `%` in directory names
    // cannot match unrelated siblings.
    let Ok(mut stmt) = conn.prepare(
        "SELECT p.data
         FROM part p
         JOIN session s ON s.id = p.session_id
         LEFT JOIN project proj ON proj.id = s.project_id
         WHERE s.directory = ?1
            OR s.directory LIKE ?2 ESCAPE '\\'
            OR proj.worktree = ?1
            OR proj.worktree LIKE ?2 ESCAPE '\\'",
    ) else {
        return;
    };

    for project in project_variants {
        let project_str = project.to_string_lossy().to_string();
        let like = format!("{}/%", escape_like_literal(&project_str));
        let Ok(rows) = stmt.query_map(rusqlite::params![project_str, like], |row| {
            row.get::<_, String>(0)
        }) else {
            continue;
        };

        for row in rows.flatten() {
            for cmd in extract_opencode_part_commands(&row) {
                record_command(counts, &cmd, "opencode");
            }
        }
    }
}

/// Escape `\`, `%`, and `_` for use in a SQL LIKE pattern with `ESCAPE '\'`.
fn escape_like_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

fn scan_jsonl_file(
    path: &Path,
    source: &str,
    extract: fn(&str) -> Vec<String>,
    counts: &mut BTreeMap<String, (u32, BTreeSet<String>)>,
) {
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    for line in raw.lines() {
        for cmd in extract(line) {
            record_command(counts, &cmd, source);
        }
    }
}

fn collect_jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if out.len() >= MAX_SESSION_FILES_PER_AGENT {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    // Prefer newer session files when we hit the cap.
    entries.sort_by_key(|e| std::cmp::Reverse(e.metadata().ok().and_then(|m| m.modified().ok())));
    for entry in entries {
        if out.len() >= MAX_SESSION_FILES_PER_AGENT {
            break;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        {
            out.push(path);
        }
    }
}

/// Like [`collect_jsonl_files`], but without an early cap — used when a later
/// filter (e.g. Codex cwd match) decides which files count toward the budget.
fn collect_jsonl_files_uncapped(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.metadata().ok().and_then(|m| m.modified().ok())));
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files_uncapped(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        {
            out.push(path);
        }
    }
}

fn extract_claude_commands(line: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<JsonValue>(line) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // message.content[].type=tool_use name=Bash input.command
    if let Some(content) = v.pointer("/message/content").and_then(JsonValue::as_array) {
        for item in content {
            let name = item.get("name").and_then(JsonValue::as_str).unwrap_or("");
            let ty = item.get("type").and_then(JsonValue::as_str).unwrap_or("");
            if ty == "tool_use" && is_shell_tool_name(name) {
                if let Some(cmd) = item
                    .pointer("/input/command")
                    .and_then(JsonValue::as_str)
                    .or_else(|| item.pointer("/input/cmd").and_then(JsonValue::as_str))
                {
                    out.push(cmd.to_string());
                }
            }
        }
    }
    out
}

fn extract_codex_commands(line: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<JsonValue>(line) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let payload = v.get("payload").unwrap_or(&v);
    let ty = payload
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let name = payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .and_then(JsonValue::as_str)
        .unwrap_or("");

    if matches!(ty, "function_call" | "custom_tool_call") && is_shell_tool_name(name) {
        // arguments is often a JSON string: {"cmd":"..."} or {"command":"..."}
        if let Some(args) = payload.get("arguments") {
            if let Some(s) = args.as_str() {
                if let Ok(parsed) = serde_json::from_str::<JsonValue>(s) {
                    push_cmd_fields(&parsed, &mut out);
                }
            } else if args.is_object() {
                push_cmd_fields(args, &mut out);
            }
        }
        if let Some(s) = payload.get("input").and_then(JsonValue::as_str) {
            out.push(s.to_string());
        }
    }
    out
}

fn extract_pi_omp_commands(line: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<JsonValue>(line) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    // message.content[] toolCall name=bash arguments.command
    if let Some(content) = v.pointer("/message/content").and_then(JsonValue::as_array) {
        for item in content {
            let ty = item.get("type").and_then(JsonValue::as_str).unwrap_or("");
            let name = item.get("name").and_then(JsonValue::as_str).unwrap_or("");
            if (ty == "toolCall" || ty == "tool_use") && is_shell_tool_name(name) {
                if let Some(args) = item.get("arguments") {
                    push_cmd_fields(args, &mut out);
                }
            }
        }
    }

    // custom tool_execution_start
    if v.get("customType").and_then(JsonValue::as_str) == Some("tool_execution_start") {
        let name = v
            .pointer("/data/toolName")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        if is_shell_tool_name(name) {
            if let Some(args) = v.pointer("/data/args") {
                push_cmd_fields(args, &mut out);
            }
        }
    }

    out
}

fn extract_opencode_part_commands(data: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<JsonValue>(data) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let ty = v.get("type").and_then(JsonValue::as_str).unwrap_or("");
    let tool = v.get("tool").and_then(JsonValue::as_str).unwrap_or("");
    if ty == "tool" && is_shell_tool_name(tool) {
        if let Some(input) = v.pointer("/state/input") {
            push_cmd_fields(input, &mut out);
        }
        if let Some(input) = v.get("input") {
            push_cmd_fields(input, &mut out);
        }
    }
    out
}

fn push_cmd_fields(obj: &JsonValue, out: &mut Vec<String>) {
    for key in ["command", "cmd", "script", "commandLine"] {
        if let Some(s) = obj.get(key).and_then(JsonValue::as_str) {
            out.push(s.to_string());
            return;
        }
    }
    // Some agents pass argv arrays.
    if let Some(arr) = obj.get("command").and_then(JsonValue::as_array) {
        let parts: Vec<&str> = arr.iter().filter_map(JsonValue::as_str).collect();
        if !parts.is_empty() {
            out.push(shell_join(&parts));
        }
    }
}

fn is_shell_tool_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "bash"
            | "shell"
            | "exec"
            | "execute"
            | "exec_command"
            | "run_terminal_cmd"
            | "run_terminal_command"
            | "terminal"
            | "local_shell"
            | "Bash"
    ) || {
        let lower = name.to_ascii_lowercase();
        lower == "bash"
            || lower.contains("shell")
            || lower.contains("exec_command")
            || lower == "exec"
    }
}

fn record_command(counts: &mut BTreeMap<String, (u32, BTreeSet<String>)>, raw: &str, source: &str) {
    for cmd in normalize_and_split(raw) {
        let cmd = redact_secrets(&cmd);
        if !feels_commandlike(&cmd) {
            continue;
        }
        let entry = counts.entry(cmd).or_insert_with(|| (0, BTreeSet::new()));
        entry.0 = entry.0.saturating_add(1);
        entry.1.insert(source.to_string());
    }
}

/// Split compound shell lines into individual candidates and normalize whitespace.
fn normalize_and_split(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    // Always split `&&` / `;` chains so `cd <dir> && cargo test` yields the
    // useful trailing command instead of being rejected as a bare `cd`.
    let pieces: Vec<String> = if trimmed.contains("&&") || trimmed.contains(';') {
        trimmed
            .split("&&")
            .flat_map(|chunk| chunk.split(';'))
            .map(|piece| collapse_ws(piece.trim().trim_start_matches("then ").trim()))
            .filter(|p| !p.is_empty())
            .collect()
    } else {
        vec![collapse_ws(trimmed)]
    };

    pieces
        .into_iter()
        .filter_map(|piece| {
            // Drop leading `cd <path>` navigation that agents often prefix.
            let stripped = strip_leading_cd(&piece);
            if stripped.is_empty() {
                None
            } else {
                Some(stripped)
            }
        })
        .collect()
}

fn strip_leading_cd(cmd: &str) -> String {
    let mut rest = cmd;
    loop {
        let trimmed = rest.trim_start();
        let Some(after_cd) = trimmed
            .strip_prefix("cd ")
            .or_else(|| trimmed.strip_prefix("cd\t"))
        else {
            return trimmed.to_string();
        };
        // Consume one path argument (quoted or bare), then continue if more
        // leading cds remain (unusual but cheap to handle).
        let after_cd = after_cd.trim_start();
        rest = skip_one_shell_word(after_cd).trim_start();
        if rest.is_empty() {
            return String::new();
        }
    }
}

fn skip_one_shell_word(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return s;
    }
    match bytes[0] {
        b'\'' => {
            if let Some(end) = s[1..].find('\'') {
                &s[end + 2..]
            } else {
                ""
            }
        }
        b'"' => {
            if let Some(end) = s[1..].find('"') {
                &s[end + 2..]
            } else {
                ""
            }
        }
        _ => {
            if let Some(idx) = s.find(char::is_whitespace) {
                &s[idx..]
            } else {
                ""
            }
        }
    }
}

/// Redact inline env assignments / tokens that look like secrets before a
/// mined command is stored in `.qr/profile.json`.
fn redact_secrets(cmd: &str) -> String {
    cmd.split_whitespace()
        .map(redact_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_token(token: &str) -> String {
    // `export NAME=value` / `NAME=value` forms.
    if let Some((name, _value)) = token.split_once('=') {
        let name = name.strip_prefix("export").unwrap_or(name);
        let name = name.trim_start_matches(['\'', '"']);
        if looks_secret_name(name) {
            return format!("{name}=***");
        }
    }
    // Bare high-entropy tokens (sk-..., ghp_..., etc.) as standalone args.
    if looks_secret_literal(token) {
        return "***".to_string();
    }
    token.to_string()
}

fn looks_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    const NEEDLES: &[&str] = &[
        "API_KEY",
        "APIKEY",
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "ACCESS_KEY",
        "PRIVATE_KEY",
        "AUTH",
        "BEARER",
    ];
    NEEDLES.iter().any(|n| upper.contains(n))
}

fn looks_secret_literal(token: &str) -> bool {
    let t = token.trim_matches(['\'', '"']);
    const PREFIXES: &[&str] = &[
        "sk-", "sk_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "xoxb-", "xoxp-", "xoxa-", "AKIA",
    ];
    if PREFIXES.iter().any(|p| t.starts_with(p)) && t.len() >= 16 {
        return true;
    }
    false
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Policy: keep project-task-like shell commands; drop pure inspection noise.
///
/// This is intentionally a defended heuristic — if the denylist/allow-signals
/// change, update the characterization tests in this module together with the
/// rationale here. Prefer over-including occasional noise over dropping a real
/// project command the user actually runs.
fn feels_commandlike(cmd: &str) -> bool {
    if cmd.len() < 2 || cmd.len() > 300 {
        return false;
    }
    // Multi-line heredocs / dumps are not useful as profile scripts.
    if cmd.contains('\n') {
        return false;
    }

    let first = cmd.split_whitespace().next().unwrap_or("");
    let base = first.rsplit('/').next().unwrap_or(first);

    // Denylist of pure inspection / navigation commands when used alone or
    // with only path arguments.
    const DENY: &[&str] = &[
        "ls", "ll", "la", "pwd", "cd", "echo", "printf", "true", "false", "clear", "which", "type",
        "file", "stat", "wc", "head", "tail", "less", "more", "cat", "bat", "rg", "grep", "ag",
        "fd", "find", "tree", "sed", "awk", "jq", "yq", "sleep", "true",
    ];
    if DENY.iter().any(|d| base.eq_ignore_ascii_case(d)) {
        return false;
    }

    // git inspection without mutations is usually noise for a project profile.
    if base.eq_ignore_ascii_case("git") {
        let sub = cmd.split_whitespace().nth(1).unwrap_or("");
        const GIT_KEEP: &[&str] = &[
            "commit",
            "push",
            "pull",
            "rebase",
            "merge",
            "cherry-pick",
            "stash",
            "tag",
            "clone",
            "fetch",
            "switch",
            "checkout",
            "restore",
            "reset",
            "rebase",
            "am",
            "revert",
        ];
        // Keep common workflow git commands; drop status/diff/log/show/branch.
        if !GIT_KEEP.iter().any(|k| sub.eq_ignore_ascii_case(k)) {
            return false;
        }
    }

    // Positive signals: looks like a real toolchain / script invocation.
    const SIGNALS: &[&str] = &[
        "cargo",
        "npm",
        "pnpm",
        "yarn",
        "bun",
        "node",
        "npx",
        "go",
        "python",
        "python3",
        "pytest",
        "uv",
        "poetry",
        "pdm",
        "pip",
        "pipenv",
        "make",
        "just",
        "cmake",
        "ninja",
        "docker",
        "docker-compose",
        "compose",
        "kubectl",
        "helm",
        "terraform",
        "ansible",
        "mvn",
        "gradle",
        "bazel",
        "next",
        "vite",
        "vitest",
        "jest",
        "eslint",
        "prettier",
        "tsc",
        "ruff",
        "black",
        "mypy",
        "uvicorn",
        "flask",
        "django-admin",
        "manage.py",
        "cargo",
        "rustc",
        "clippy",
        "wasm-pack",
        "deno",
        "mix",
        "bundle",
        "rails",
        "rake",
        "dotnet",
        "swift",
        "xcodebuild",
        "pod",
        "fastlane",
        "turbo",
        "nx",
        "playwright",
        "cypress",
        "storybook",
        "wrangler",
        "serverless",
        "sam",
        "cdk",
        "pulumi",
    ];

    if SIGNALS.iter().any(|s| base.eq_ignore_ascii_case(s)) {
        return true;
    }
    // Relative / absolute script paths: ./scripts/foo, bin/test, /usr/bin/… rare
    if first.starts_with("./") || first.starts_with("../") || first.starts_with('/') {
        return true;
    }
    // env-prefixed: FOO=1 cargo test — first token has '=', look at the rest
    if first.contains('=') {
        let rest = cmd.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
        return !rest.is_empty() && feels_commandlike(&rest);
    }
    // Otherwise keep short non-denied commands that look executable (have a
    // subcommand or flag) — e.g. custom project CLIs.
    cmd.split_whitespace().count() >= 1
        && base
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !base.chars().all(|c| c.is_ascii_digit())
}

fn rank_commands(counts: BTreeMap<String, (u32, BTreeSet<String>)>) -> Vec<MinedCommand> {
    let mut items: Vec<MinedCommand> = counts
        .into_iter()
        .map(|(command, (count, sources))| MinedCommand {
            command,
            count,
            sources: sources.into_iter().collect(),
        })
        .collect();
    // Higher frequency first; tie-break by command string for stability.
    items.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.command.cmp(&b.command))
    });
    items.truncate(MAX_MINED_COMMANDS);
    items
}

/// Claude encodes `/Users/foo/bar` as `-Users-foo-bar`.
fn claude_path_encodings(projects: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    for project in projects {
        let s = project.to_string_lossy().replace('/', "-");
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Pi encodes `/Users/foo/bar` as `--Users-foo-bar--`.
fn pi_path_encodings(projects: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    for project in projects {
        let abs = project.to_string_lossy();
        let inner = abs.trim_start_matches('/').replace('/', "-");
        for enc in [
            format!("--{inner}--"),
            format!("--{inner}"),
            format!("-{inner}-"),
        ] {
            if !out.contains(&enc) {
                out.push(enc);
            }
        }
    }
    out
}

/// omp (Pi fork) often encodes paths relative to `$HOME` as `-Development-foo`.
/// `home` must be snapshotted by the caller — do not re-read process env here
/// (lib tests mutate `HOME` under a shared lock).
fn omp_path_encodings(projects: &[PathBuf], home: Option<&Path>) -> Vec<String> {
    let mut out = pi_path_encodings(projects);
    for project in projects {
        if let Some(home) = home {
            if let Ok(rel) = project.strip_prefix(home) {
                let enc = format!("-{}", rel.to_string_lossy().replace('/', "-"));
                if !out.contains(&enc) {
                    out.push(enc);
                }
            }
        }
        // Parent+name form (e.g. `-Development-quick-runner`) covers agents
        // that strip `$HOME` even when strip_prefix fails on path variants.
        if let Some(name) = project.file_name() {
            if let Some(parent) = project.parent().and_then(|p| p.file_name()) {
                let enc = format!("-{}-{}", parent.to_string_lossy(), name.to_string_lossy());
                if !out.contains(&enc) {
                    out.push(enc);
                }
            }
        }
        let abs = project.to_string_lossy();
        let enc = format!("-{}", abs.trim_start_matches('/').replace('/', "-"));
        if !out.contains(&enc) {
            out.push(enc);
        }
    }
    out
}

fn path_encoding_matches_project(encoded_dir: &str, project: &Path, home: Option<&Path>) -> bool {
    let variants = path_variants(project);
    let candidates = [
        claude_path_encodings(&variants),
        pi_path_encodings(&variants),
        omp_path_encodings(&variants, home),
    ]
    .concat();
    candidates.iter().any(|c| c == encoded_dir)
}

/// True when `cwd` is the project itself or a subdirectory of it.
/// Parent-directory sessions (monorepo root, `$HOME`, `/`) are intentionally
/// excluded so their commands are not attributed to every child project.
fn paths_related(cwd: &Path, project: &Path) -> bool {
    cwd == project || cwd.starts_with(project)
}

fn shell_join(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| {
            if p.contains(|c: char| c.is_whitespace() || "\"'`$&|;<>()".contains(c)) {
                format!("'{}'", p.replace('\'', r"'\''"))
            } else {
                (*p).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Merge mined commands into a profile: store them under `agent_commands` and
/// fill any still-empty top-level role fields when a frequent mined command
/// looks like that role.
pub fn merge_mined_into_profile(
    profile: &mut crate::project_profile::ProjectProfile,
    mined: Vec<MinedCommand>,
) {
    if mined.is_empty() {
        return;
    }

    // Also surface high-signal commands into scripts under a stable key when
    // the name is free — never clobber manifest-derived scripts.
    for (i, item) in mined.iter().enumerate() {
        let key = format!("agent:{}", i + 1);
        profile
            .scripts
            .entry(key)
            .or_insert_with(|| item.command.clone());
    }

    fill_role_from_mined(profile, &mined);
    profile.agent_commands = mined;
}

fn fill_role_from_mined(
    profile: &mut crate::project_profile::ProjectProfile,
    mined: &[MinedCommand],
) {
    let pick = |pred: fn(&str) -> bool| -> Option<String> {
        mined
            .iter()
            .find(|m| pred(&m.command))
            .map(|m| m.command.clone())
    };

    if profile.test_command.is_none() {
        profile.test_command = pick(|c| {
            let t = c.to_ascii_lowercase();
            t.contains("pytest")
                || t.contains("vitest")
                || t.contains("nextest")
                || t.contains("jest")
                || t.contains("cargo test")
                || t.contains("go test")
                || t.split_whitespace().any(|w| w == "test")
        });
    }
    if profile.build_command.is_none() {
        profile.build_command = pick(|c| {
            let t = c.to_ascii_lowercase();
            t.contains(" build") || t.ends_with(" build") || t.contains("cargo build")
        });
    }
    if profile.lint_command.is_none() {
        profile.lint_command = pick(|c| {
            let t = c.to_ascii_lowercase();
            t.contains("lint") || t.contains("clippy") || t.contains("eslint") || t.contains("ruff")
        });
    }
    if profile.dev_command.is_none() {
        profile.dev_command = pick(|c| {
            let t = c.to_ascii_lowercase();
            t.contains(" dev")
                || t.ends_with(" dev")
                || t.contains("--reload")
                || t.contains("runserver")
        });
    }
    if profile.run_command.is_none() {
        profile.run_command = pick(|c| {
            let t = c.to_ascii_lowercase();
            t.contains(" start") || t.contains("cargo run") || t.contains("go run")
        });
    }
    if profile.debug_command.is_none() {
        profile.debug_command = pick(|c| {
            let t = c.to_ascii_lowercase();
            t.contains("debug") || t.contains("backtrace") || t.contains("--inspect")
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_jsonl(path: &Path, lines: &[&str]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    #[test]
    fn feels_commandlike_keeps_toolchain_drops_inspection() {
        // Pinned heuristic contract — update this test AND the doc-comment on
        // feels_commandlike together if the policy changes.
        assert!(feels_commandlike("cargo test"));
        assert!(feels_commandlike("pnpm build"));
        assert!(feels_commandlike("uv run pytest"));
        assert!(feels_commandlike("./scripts/deploy.sh"));
        assert!(feels_commandlike("RUST_BACKTRACE=1 cargo run"));
        assert!(!feels_commandlike("ls -la"));
        assert!(!feels_commandlike("git status"));
        assert!(!feels_commandlike("cat Cargo.toml"));
        assert!(!feels_commandlike("pwd"));
        assert!(feels_commandlike("git commit -m 'x'"));
    }

    #[test]
    fn normalize_splits_short_cd_chains() {
        let parts = normalize_and_split("cd /repo && cargo test");
        assert_eq!(parts, vec!["cargo test".to_string()]);
        let parts = normalize_and_split("cd '/tmp/proj'; pnpm build");
        assert_eq!(parts, vec!["pnpm build".to_string()]);
    }

    #[test]
    fn redact_secrets_masks_env_assignments_and_token_literals() {
        assert_eq!(
            redact_secrets("AWS_SECRET_ACCESS_KEY=deadbeef cargo test"),
            "AWS_SECRET_ACCESS_KEY=*** cargo test"
        );
        assert_eq!(
            redact_secrets("OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz cargo test"),
            "OPENAI_API_KEY=*** cargo test"
        );
        assert_eq!(
            redact_secrets("curl -H sk-abcdefghijklmnopqrstuvwxyz https://example.com"),
            "curl -H *** https://example.com"
        );
        // Non-secret env assignments are preserved.
        assert_eq!(
            redact_secrets("RUST_BACKTRACE=1 cargo run"),
            "RUST_BACKTRACE=1 cargo run"
        );
    }

    #[test]
    fn paths_related_excludes_parent_cwd() {
        let project = Path::new("/workspace/app");
        assert!(paths_related(Path::new("/workspace/app"), project));
        assert!(paths_related(Path::new("/workspace/app/crates/x"), project));
        assert!(!paths_related(Path::new("/workspace"), project));
        assert!(!paths_related(Path::new("/"), project));
    }

    #[test]
    fn escape_like_literal_escapes_wildcards() {
        assert_eq!(escape_like_literal(r"a_b%c\d"), r"a\_b\%c\\d");
    }

    #[test]
    fn mines_claude_bash_tool_use() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        fs::create_dir_all(&project).unwrap();
        let home = tmp.path().join("home");
        let enc = project.to_string_lossy().replace('/', "-");
        let session = home.join(".claude/projects").join(&enc).join("sess.jsonl");
        write_jsonl(
            &session,
            &[
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#,
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#,
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}]}}"#,
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cd /repo && cargo nextest run"}}]}}"#,
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz cargo build"}}]}}"#,
            ],
        );

        let roots = AgentHistoryRoots {
            claude_projects: Some(home.join(".claude/projects")),
            ..AgentHistoryRoots::default()
        };
        let mined = mine_for_project_with_roots(&project, &roots);
        let cmds: Vec<_> = mined.iter().map(|m| m.command.as_str()).collect();
        assert!(cmds.contains(&"cargo test"), "{cmds:?}");
        assert!(cmds.contains(&"cargo nextest run"), "{cmds:?}");
        assert!(
            cmds.iter()
                .any(|c| c.contains("OPENAI_API_KEY=***") && c.contains("cargo build")),
            "{cmds:?}"
        );
        assert!(!cmds.iter().any(|c| c.contains("sk-")), "{cmds:?}");
    }

    #[test]
    fn mines_omp_bash_tool_call() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("Development").join("app");
        fs::create_dir_all(&project).unwrap();
        let home = tmp.path().join("home");
        // omp-style relative encoding: -Development-app
        let enc = "-Development-app";
        let session = home
            .join(".omp/agent/sessions")
            .join(enc)
            .join("sess.jsonl");
        write_jsonl(
            &session,
            &[
                r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","name":"bash","arguments":{"command":"pnpm test"}}]}}"#,
                r#"{"type":"custom","customType":"tool_execution_start","data":{"toolName":"bash","args":{"command":"pnpm build"}}}"#,
            ],
        );

        let roots = AgentHistoryRoots {
            home: Some(home.clone()),
            omp_sessions: Some(home.join(".omp/agent/sessions")),
            ..AgentHistoryRoots::default()
        };
        // Encoding match is by dir name equality with omp_path_encodings; inject
        // by also using full-path encoding fallback — write a second encoding.
        let abs_enc = format!(
            "-{}",
            project
                .to_string_lossy()
                .trim_start_matches('/')
                .replace('/', "-")
        );
        let session2 = home
            .join(".omp/agent/sessions")
            .join(&abs_enc)
            .join("sess.jsonl");
        write_jsonl(
            &session2,
            &[
                r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","name":"bash","arguments":{"command":"pnpm test"}}]}}"#,
                r#"{"type":"custom","customType":"tool_execution_start","data":{"toolName":"bash","args":{"command":"pnpm build"}}}"#,
            ],
        );

        let mined = mine_for_project_with_roots(&project, &roots);
        let cmds: Vec<_> = mined.iter().map(|m| m.command.as_str()).collect();
        assert!(cmds.contains(&"pnpm test"), "{cmds:?}");
        assert!(cmds.contains(&"pnpm build"), "{cmds:?}");
        assert!(mined.iter().all(|m| m.sources.contains(&"omp".to_string())));
    }

    #[test]
    fn mines_codex_exec_command_for_matching_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        fs::create_dir_all(&project).unwrap();
        let sessions = tmp.path().join("sessions");
        let file = sessions.join("2026/07/01/rollout.jsonl");
        let project_s = project.to_string_lossy();
        write_jsonl(
            &file,
            &[
                &format!(
                    r#"{{"type":"session_meta","payload":{{"cwd":"{project_s}","session_id":"x"}}}}"#
                ),
                r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo clippy\"}"}}"#,
                r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls\"}"}}"#,
            ],
        );

        // Parent-cwd session must not leak into this project's mined commands.
        let parent = tmp.path().to_string_lossy();
        let parent_file = sessions.join("2026/07/01/parent.jsonl");
        write_jsonl(
            &parent_file,
            &[
                &format!(
                    r#"{{"type":"session_meta","payload":{{"cwd":"{parent}","session_id":"parent"}}}}"#
                ),
                r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo build -p sibling\"}"}}"#,
            ],
        );

        let roots = AgentHistoryRoots {
            codex_sessions: Some(sessions),
            ..AgentHistoryRoots::default()
        };
        let mined = mine_for_project_with_roots(&project, &roots);
        assert_eq!(mined.len(), 1);
        assert_eq!(mined[0].command, "cargo clippy");
        assert_eq!(mined[0].sources, vec!["codex".to_string()]);
    }

    #[test]
    fn merge_fills_empty_roles_and_sets_agent_commands() {
        let mut profile = crate::project_profile::ProjectProfile {
            name: "x".into(),
            language: Some("rust".into()),
            framework: None,
            package_manager: Some("cargo".into()),
            test_command: None,
            build_command: Some("cargo build".into()),
            lint_command: None,
            dev_command: None,
            run_command: None,
            debug_command: None,
            scripts: BTreeMap::new(),
            prefer_agent: None,
            entry_points: vec![],
            agent_commands: vec![],
        };
        let mined = vec![
            MinedCommand {
                command: "cargo nextest run".into(),
                count: 5,
                sources: vec!["claude".into()],
            },
            MinedCommand {
                command: "cargo clippy -- -D warnings".into(),
                count: 3,
                sources: vec!["codex".into()],
            },
        ];
        merge_mined_into_profile(&mut profile, mined.clone());
        assert_eq!(profile.test_command.as_deref(), Some("cargo nextest run"));
        assert_eq!(
            profile.lint_command.as_deref(),
            Some("cargo clippy -- -D warnings")
        );
        // Existing build is preserved.
        assert_eq!(profile.build_command.as_deref(), Some("cargo build"));
        assert_eq!(profile.agent_commands, mined);
        assert_eq!(
            profile.scripts.get("agent:1").map(String::as_str),
            Some("cargo nextest run")
        );
    }
}
