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
    env,
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader},
    path::{Component, Path, PathBuf},
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

#[derive(Debug)]
struct ShellInvocation {
    command: String,
    workdir: Option<PathBuf>,
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
    let normalized_root = lexically_normalize(project_root);
    let mut out = vec![normalized_root.clone()];
    let push_unique = |out: &mut Vec<PathBuf>, p: PathBuf| {
        if !out.iter().any(|existing| existing == &p) {
            out.push(p);
        }
    };
    if let Ok(canon) = normalized_root.canonicalize() {
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

fn lexically_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = normalized
                    .file_name()
                    .is_some_and(|name| name != OsStr::new(".."));
                if can_pop {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
            }
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
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

    let project_variants = path_variants(project);
    let mut matched = 0usize;
    for dir in session_dirs {
        visit_jsonl_files(&dir, &mut |file| {
            if matched >= MAX_SESSION_FILES_PER_AGENT {
                return false;
            }
            let session_cwd = session_declared_cwd(file);
            let belongs = session_cwd
                .as_ref()
                .is_some_and(|cwd| workdir_belongs_to_project(cwd, &project_variants));
            if let Some(session_cwd) = session_cwd.filter(|_| belongs) {
                scan_jsonl_file(
                    file,
                    source,
                    extract,
                    &session_cwd,
                    &project_variants,
                    counts,
                );
                matched = matched.saturating_add(1);
            }
            matched < MAX_SESSION_FILES_PER_AGENT
        });
        if matched >= MAX_SESSION_FILES_PER_AGENT {
            break;
        }
    }
}

fn session_declared_cwd(path: &Path) -> Option<PathBuf> {
    let file = fs::File::open(path).ok()?;
    const POINTERS: &[&str] = &[
        "/cwd",
        "/payload/cwd",
        "/session/cwd",
        "/metadata/cwd",
        "/data/cwd",
        "/header/cwd",
        "/worktree",
        "/projectPath",
        "/project_path",
    ];
    for line in BufReader::new(file).lines().take(50) {
        let Ok(line) = line else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<JsonValue>(&line) else {
            continue;
        };
        if let Some(cwd) = POINTERS
            .iter()
            .find_map(|pointer| value.pointer(pointer).and_then(JsonValue::as_str))
        {
            return Some(PathBuf::from(cwd));
        }
    }
    None
}

fn collect_codex(
    sessions_root: &Path,
    project_variants: &[PathBuf],
    counts: &mut BTreeMap<String, (u32, BTreeSet<String>)>,
) {
    if !sessions_root.is_dir() {
        return;
    }
    let mut scanned = 0usize;
    // Visit paths lazily so years of unrelated history do not accumulate in
    // memory. Read only the small metadata prefix for unrelated sessions; scan
    // the full file only when the session cwd overlaps this project.
    visit_jsonl_files(sessions_root, &mut |file| {
        if scanned >= MAX_SESSION_FILES_PER_AGENT {
            return false;
        }
        let Some(session_cwd) = codex_session_cwd(file) else {
            return true;
        };
        let cwd_variants = path_variants(&session_cwd);
        let overlaps = project_variants
            .iter()
            .any(|project| cwd_variants.iter().any(|cwd| paths_overlap(cwd, project)));
        if reserve_codex_session_scan(&mut scanned, overlaps) {
            scan_codex_session(file, &session_cwd, project_variants, counts);
        }
        scanned < MAX_SESSION_FILES_PER_AGENT
    });
}

fn reserve_codex_session_scan(scanned: &mut usize, overlaps: bool) -> bool {
    if !overlaps || *scanned >= MAX_SESSION_FILES_PER_AGENT {
        return false;
    }
    *scanned = scanned.saturating_add(1);
    true
}

fn codex_session_cwd(path: &Path) -> Option<PathBuf> {
    let file = fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(20) {
        let Ok(line) = line else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<JsonValue>(&line) else {
            continue;
        };
        if value.get("type").and_then(JsonValue::as_str) == Some("session_meta") {
            return value
                .pointer("/payload/cwd")
                .and_then(JsonValue::as_str)
                .map(PathBuf::from);
        }
    }
    None
}

fn scan_codex_session(
    path: &Path,
    session_cwd: &Path,
    project_variants: &[PathBuf],
    counts: &mut BTreeMap<String, (u32, BTreeSet<String>)>,
) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut matched = false;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        for invocation in extract_codex_invocations(&line) {
            let declared_workdir = invocation
                .workdir
                .as_deref()
                .map(|workdir| {
                    if workdir.is_absolute() {
                        workdir.to_path_buf()
                    } else {
                        session_cwd.join(workdir)
                    }
                })
                .unwrap_or_else(|| session_cwd.to_path_buf());
            matched |= record_command_scoped(
                counts,
                &invocation.command,
                "codex",
                &declared_workdir,
                project_variants,
            );
        }
    }
    matched
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
    // Use binary equality/prefix comparisons rather than LIKE: LIKE folds ASCII
    // case by default and treats `%` / `_` in real paths as wildcards.
    let Ok(mut stmt) = conn.prepare(
        "SELECT p.data,
                CASE WHEN s.directory IS NOT NULL AND s.directory != ''
                     THEN s.directory ELSE proj.worktree END
         FROM part p
         JOIN session s ON s.id = p.session_id
         LEFT JOIN project proj ON proj.id = s.project_id
         WHERE s.directory COLLATE BINARY = ?1
            OR substr(s.directory, 1, length(?1) + 1) COLLATE BINARY = ?1 || '/'
            OR proj.worktree COLLATE BINARY = ?1
            OR substr(proj.worktree, 1, length(?1) + 1) COLLATE BINARY = ?1 || '/'",
    ) else {
        return;
    };

    for project in project_variants {
        let project_str = project.to_string_lossy().to_string();
        let Ok(rows) = stmt.query_map(rusqlite::params![project_str], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PathBuf::from(row.get::<_, String>(1)?),
            ))
        }) else {
            continue;
        };

        for (data, session_cwd) in rows.flatten() {
            for cmd in extract_opencode_part_commands(&data) {
                record_command_scoped(counts, &cmd, "opencode", &session_cwd, project_variants);
            }
        }
    }
}

fn scan_jsonl_file(
    path: &Path,
    source: &str,
    extract: fn(&str) -> Vec<String>,
    session_cwd: &Path,
    project_variants: &[PathBuf],
    counts: &mut BTreeMap<String, (u32, BTreeSet<String>)>,
) {
    let Ok(file) = fs::File::open(path) else {
        return;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        for cmd in extract(&line) {
            record_command_scoped(counts, &cmd, source, session_cwd, project_variants);
        }
    }
}

fn visit_jsonl_files(dir: &Path, visitor: &mut impl FnMut(&Path) -> bool) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return true;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| {
        std::cmp::Reverse(
            entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok()),
        )
    });
    for entry in entries {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if !visit_jsonl_files(&path, visitor) {
                return false;
            }
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
            && !visitor(&path)
        {
            return false;
        }
    }
    true
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

fn extract_codex_invocations(line: &str) -> Vec<ShellInvocation> {
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
            let parsed = if let Some(serialized) = args.as_str() {
                serde_json::from_str::<JsonValue>(serialized).ok()
            } else if args.is_object() {
                Some(args.clone())
            } else {
                None
            };
            if let Some(parsed) = parsed {
                let workdir = parsed
                    .get("workdir")
                    .or_else(|| parsed.get("cwd"))
                    .or_else(|| parsed.get("dir"))
                    .and_then(JsonValue::as_str)
                    .map(PathBuf::from);
                let mut commands = Vec::new();
                push_cmd_fields(&parsed, &mut commands);
                out.extend(commands.into_iter().map(|command| ShellInvocation {
                    command,
                    workdir: workdir.clone(),
                }));
            }
        }
        if let Some(s) = payload.get("input").and_then(JsonValue::as_str) {
            out.push(ShellInvocation {
                command: s.to_string(),
                workdir: None,
            });
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

fn record_command_scoped(
    counts: &mut BTreeMap<String, (u32, BTreeSet<String>)>,
    raw: &str,
    source: &str,
    initial_workdir: &Path,
    project_variants: &[PathBuf],
) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return false;
    }
    let mut workdir = lexically_normalize(initial_workdir);
    let mut recorded = false;
    for piece in split_shell_chains(trimmed) {
        let piece = collapse_unquoted_ws(piece.trim().trim_start_matches("then ").trim());
        if piece.is_empty() {
            continue;
        }
        match parse_cd_navigation(&piece) {
            CdNavigation::NotCd => {
                if workdir_belongs_to_project(&workdir, project_variants) {
                    recorded |= record_normalized_command(counts, &piece, source);
                }
            }
            CdNavigation::Target(target) => {
                if target == Path::new("-")
                    || target.starts_with("~")
                    || target.to_string_lossy().contains('$')
                {
                    break;
                }
                workdir = if target.is_absolute() {
                    lexically_normalize(&target)
                } else {
                    lexically_normalize(&workdir.join(target))
                };
            }
            CdNavigation::Invalid => break,
        }
    }
    recorded
}

fn record_normalized_command(
    counts: &mut BTreeMap<String, (u32, BTreeSet<String>)>,
    command: &str,
    source: &str,
) -> bool {
    let command = redact_secrets(command);
    if !feels_commandlike(&command) {
        return false;
    }
    let entry = counts
        .entry(command)
        .or_insert_with(|| (0, BTreeSet::new()));
    entry.0 = entry.0.saturating_add(1);
    entry.1.insert(source.to_string());
    true
}

fn workdir_belongs_to_project(workdir: &Path, project_variants: &[PathBuf]) -> bool {
    let workdir_variants = path_variants(workdir);
    project_variants.iter().any(|project| {
        workdir_variants
            .iter()
            .any(|workdir| paths_related(workdir, project))
    })
}

enum CdNavigation {
    NotCd,
    Target(PathBuf),
    Invalid,
}

fn parse_cd_navigation(piece: &str) -> CdNavigation {
    let Some(words) = shlex::split(piece) else {
        return CdNavigation::Invalid;
    };
    if words.first().map(String::as_str) == Some("command")
        && words
            .iter()
            .skip(1)
            .take_while(|word| word.starts_with('-'))
            .any(|word| matches!(word.as_str(), "-v" | "-V"))
    {
        return CdNavigation::NotCd;
    }
    let start = command_start_index(&words, |_, _| {});
    if words.get(start).map(String::as_str) != Some("cd") {
        return CdNavigation::NotCd;
    }
    match &words[start..] {
        [_, target] => CdNavigation::Target(PathBuf::from(target)),
        [_, option, target] if option == "--" => CdNavigation::Target(PathBuf::from(target)),
        _ => CdNavigation::Invalid,
    }
}

/// Split compound shell lines into individual candidates and normalize whitespace.
#[cfg(test)]
fn normalize_and_split(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return Vec::new();
    }

    // Split only unquoted `&&` / `;` operators so `cd <dir> && cargo test`
    // yields the useful command without corrupting Python snippets, URLs, or
    // other arguments that contain literal separators.
    let pieces = split_shell_chains(trimmed)
        .into_iter()
        .map(|piece| collapse_unquoted_ws(piece.trim().trim_start_matches("then ").trim()))
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>();

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

fn split_shell_chains(raw: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match quote {
            Some('\'') => {
                current.push(ch);
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                current.push(ch);
                if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quote = None;
                }
            }
            Some(_) => unreachable!(),
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    current.push(ch);
                }
                '\\' => {
                    current.push(ch);
                    escaped = true;
                }
                ';' => {
                    pieces.push(std::mem::take(&mut current));
                }
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    pieces.push(std::mem::take(&mut current));
                }
                _ => current.push(ch),
            },
        }
    }
    pieces.push(current);
    pieces
}

fn collapse_unquoted_ws(raw: &str) -> String {
    let mut output = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut pending_space = false;

    for ch in raw.chars() {
        if escaped {
            output.push(ch);
            escaped = false;
            continue;
        }
        if quote.is_none() && ch.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(ch);
        match quote {
            Some('\'') if ch == '\'' => quote = None,
            Some('"') if ch == '\\' => escaped = true,
            Some('"') if ch == '"' => quote = None,
            Some(_) => {}
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch == '\\' => escaped = true,
            None => {}
        }
    }
    output
}

#[cfg(test)]
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

#[cfg(test)]
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
    let spans = shell_word_spans(cmd);
    let (tool, subcommand) = command_tool_context(cmd);
    let mut output = String::with_capacity(cmd.len());
    let mut cursor = 0;
    let mut redact_next = None;
    for (start, end) in spans {
        output.push_str(&cmd[cursor..start]);
        let token = &cmd[start..end];
        match redact_next.take() {
            Some(PendingRedaction::SecretValue) => output.push_str("***"),
            Some(PendingRedaction::HeaderValue) if is_auth_header(token) => {
                output.push_str("***");
            }
            Some(PendingRedaction::HeaderValue) | None => {
                if let Some(redacted) = redact_inline_context_token(
                    &tool,
                    subcommand.as_deref(),
                    token.trim_matches(['\'', '"']),
                ) {
                    output.push_str(&redacted);
                    cursor = end;
                    continue;
                }
                output.push_str(&redact_token(token));
                let flag = token.trim_matches(['\'', '"']);
                if !token.contains('=')
                    && (is_secret_value_flag(token)
                        || is_context_secret_flag(&tool, subcommand.as_deref(), flag))
                {
                    redact_next = Some(PendingRedaction::SecretValue);
                } else if tool == "curl" && matches!(flag, "-H" | "--header") {
                    redact_next = Some(PendingRedaction::HeaderValue);
                }
            }
        }
        cursor = end;
    }
    output.push_str(&cmd[cursor..]);
    output
}

#[derive(Clone, Copy)]
enum PendingRedaction {
    SecretValue,
    HeaderValue,
}

fn command_tool_context(command: &str) -> (String, Option<String>) {
    let Some(words) = shlex::split(command) else {
        return (String::new(), None);
    };
    let index = command_start_index(&words, |_, _| {});
    let tool = words
        .get(index)
        .and_then(|word| word.rsplit('/').next())
        .unwrap_or("")
        .to_ascii_lowercase();
    let subcommand = words.get(index + 1).map(|word| word.to_ascii_lowercase());
    (tool, subcommand)
}

fn shell_assignment(word: &str) -> Option<(&str, &str)> {
    if word.starts_with('-') {
        return None;
    }
    let (name, value) = word.split_once('=')?;
    (!name.is_empty()).then_some((name, value))
}

fn command_start_index(words: &[String], mut assignment: impl FnMut(&str, &str)) -> usize {
    let mut index = 0;
    loop {
        while let Some((name, value)) = words.get(index).and_then(|word| shell_assignment(word)) {
            assignment(name, value);
            index += 1;
        }
        let wrapper = words
            .get(index)
            .and_then(|word| word.rsplit('/').next())
            .unwrap_or("");
        match wrapper {
            "env" => {
                index += 1;
                while index < words.len() {
                    if matches!(words[index].as_str(), "-u" | "--unset") {
                        if let Some(name) = words.get(index + 1) {
                            assignment(name, "");
                        }
                        index = (index + 2).min(words.len());
                    } else if let Some(name) = words[index].strip_prefix("--unset=") {
                        assignment(name, "");
                        index += 1;
                    } else if matches!(
                        words[index].as_str(),
                        "-C" | "--chdir" | "-S" | "--split-string" | "-a" | "--argv0"
                    ) {
                        index = (index + 2).min(words.len());
                    } else if let Some((name, value)) = shell_assignment(&words[index]) {
                        assignment(name, value);
                        index += 1;
                    } else if words[index].starts_with('-') {
                        index += 1;
                    } else {
                        break;
                    }
                }
            }
            "sudo" => {
                index += 1;
                while index < words.len() && words[index].starts_with('-') {
                    let consumes_value = matches!(
                        words[index].as_str(),
                        "-u" | "--user"
                            | "-g"
                            | "--group"
                            | "-h"
                            | "--host"
                            | "-p"
                            | "--prompt"
                            | "-C"
                            | "--close-from"
                            | "-R"
                            | "--chroot"
                            | "-D"
                            | "--chdir"
                    );
                    index += if consumes_value { 2 } else { 1 };
                    index = index.min(words.len());
                }
            }
            "command" | "builtin" => {
                index += 1;
                while index < words.len() && words[index].starts_with('-') {
                    index += 1;
                }
            }
            _ => return index,
        }
    }
}

fn is_context_secret_flag(tool: &str, subcommand: Option<&str>, flag: &str) -> bool {
    (tool == "docker" && subcommand == Some("login") && flag == "-p")
        || (tool == "curl" && matches!(flag, "-u" | "--user" | "--proxy-user"))
}

fn redact_inline_context_token(
    tool: &str,
    subcommand: Option<&str>,
    token: &str,
) -> Option<String> {
    if tool == "docker" && subcommand == Some("login") {
        if token.starts_with("-p=") {
            return Some("-p=***".into());
        }
        if token.starts_with("-p") && token.len() > 2 {
            return Some("-p***".into());
        }
    }
    if tool != "curl" {
        return None;
    }
    for flag in ["--user=", "--proxy-user="] {
        if token.starts_with(flag) {
            return Some(format!("{flag}***"));
        }
    }
    if token.starts_with("-u") && token.len() > 2 {
        return Some("-u***".into());
    }
    for flag in ["--header=", "-H"] {
        if let Some(value) = token.strip_prefix(flag) {
            let value = value.strip_prefix('=').unwrap_or(value);
            if is_auth_header(value) {
                return Some(format!("{flag}***"));
            }
        }
    }
    None
}

fn is_auth_header(token: &str) -> bool {
    let token = token.trim_matches(['\'', '"']).to_ascii_lowercase();
    token.starts_with("authorization:") || token.starts_with("proxy-authorization:")
}

fn shell_word_spans(cmd: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut escaped = false;

    for (index, ch) in cmd.char_indices() {
        if start.is_none() {
            if ch.is_whitespace() {
                continue;
            }
            start = Some(index);
        }
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some('\'') if ch == '\'' => quote = None,
            Some('"') if ch == '\\' => escaped = true,
            Some('"') if ch == '"' => quote = None,
            Some(_) => {}
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch == '\\' => escaped = true,
            None if ch.is_whitespace() => {
                spans.push((start.take().expect("word has a start"), index));
            }
            None => {}
        }
    }
    if let Some(start) = start {
        spans.push((start, cmd.len()));
    }
    spans
}

fn is_secret_value_flag(token: &str) -> bool {
    let token = token.trim_matches(['\'', '"']);
    if !token.starts_with('-') {
        return false;
    }
    let name = token.trim_start_matches('-').trim_end_matches([':', '=']);
    let normalized = name.to_ascii_lowercase().replace('_', "-");
    !name.is_empty() && !normalized.ends_with("-stdin") && looks_secret_name(name)
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
    if contains_url_userinfo(token) {
        return "***".to_string();
    }
    token.to_string()
}

fn contains_url_userinfo(token: &str) -> bool {
    let token = token.trim_matches(['\'', '"']);
    let Some((_, rest)) = token.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority
        .split_once('@')
        .is_some_and(|(userinfo, _)| !userinfo.is_empty())
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

fn paths_overlap(left: &Path, right: &Path) -> bool {
    paths_related(left, right) || paths_related(right, left)
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
    let pick = |role: MinedRole| -> Option<String> {
        mined
            .iter()
            .find(|m| command_matches_role(&m.command, role))
            .map(|m| m.command.clone())
    };

    if profile.test_command.is_none() {
        profile.test_command = pick(MinedRole::Test);
    }
    if profile.build_command.is_none() {
        profile.build_command = pick(MinedRole::Build);
    }
    if profile.lint_command.is_none() {
        profile.lint_command = pick(MinedRole::Lint);
    }
    if profile.dev_command.is_none() {
        profile.dev_command = pick(MinedRole::Dev);
    }
    if profile.run_command.is_none() {
        profile.run_command = pick(MinedRole::Run);
    }
    if profile.debug_command.is_none() {
        profile.debug_command = pick(MinedRole::Debug);
    }
}

#[derive(Clone, Copy)]
enum MinedRole {
    Test,
    Build,
    Lint,
    Dev,
    Run,
    Debug,
}

fn command_matches_role(command: &str, role: MinedRole) -> bool {
    let Some(words) = shlex::split(command) else {
        return false;
    };
    let mut backtrace_enabled = None;
    let start = command_start_index(&words, |name, value| {
        if name.eq_ignore_ascii_case("RUST_BACKTRACE") {
            backtrace_enabled = Some(!value.is_empty() && value != "0");
        }
    });
    let mut command_words = &words[start..];
    while command_words.len() >= 2
        && matches!(
            command_words[0]
                .rsplit('/')
                .next()
                .unwrap_or(&command_words[0]),
            "uv" | "pipenv" | "poetry"
        )
        && command_words[1] == "run"
    {
        command_words = &command_words[2..];
    }
    let Some(first) = command_words.first() else {
        return false;
    };
    let tool = first.rsplit('/').next().unwrap_or(first.as_str());
    let args = &command_words[1..];
    let subcommand = args.first().map(String::as_str).unwrap_or("");
    let package_script = || {
        if subcommand == "run" {
            args.get(1).map(String::as_str)
        } else if !subcommand.starts_with('-') {
            Some(subcommand)
        } else {
            None
        }
    };
    let target_matches = |target: Option<&str>, expected: &str| {
        target
            .is_some_and(|target| target == expected || target.starts_with(&format!("{expected}:")))
    };

    match role {
        MinedRole::Test => match tool {
            "cargo" => {
                subcommand == "test"
                    || (subcommand == "nextest" && args.get(1).is_some_and(|arg| arg == "run"))
            }
            "go" => subcommand == "test",
            "npm" | "pnpm" | "yarn" | "bun" => target_matches(package_script(), "test"),
            "make" | "just" => target_matches(
                args.iter()
                    .find(|arg| !arg.starts_with('-'))
                    .map(String::as_str),
                "test",
            ),
            "pytest" | "vitest" | "jest" | "nextest" | "cargo-nextest" => true,
            "python" | "python3" => args
                .windows(2)
                .any(|pair| pair[0] == "-m" && pair[1] == "pytest"),
            _ => false,
        },
        MinedRole::Build => match tool {
            "cargo" | "go" | "docker" => subcommand == "build",
            "npm" | "pnpm" | "yarn" | "bun" => target_matches(package_script(), "build"),
            "make" | "just" => target_matches(
                args.iter()
                    .find(|arg| !arg.starts_with('-'))
                    .map(String::as_str),
                "build",
            ),
            _ => false,
        },
        MinedRole::Lint => match tool {
            "cargo" => subcommand == "clippy",
            "npm" | "pnpm" | "yarn" | "bun" => target_matches(package_script(), "lint"),
            "make" | "just" => target_matches(
                args.iter()
                    .find(|arg| !arg.starts_with('-'))
                    .map(String::as_str),
                "lint",
            ),
            "eslint" | "ruff" => true,
            _ => false,
        },
        MinedRole::Dev => match tool {
            "npm" | "pnpm" | "yarn" | "bun" => target_matches(package_script(), "dev"),
            "make" | "just" => target_matches(
                args.iter()
                    .find(|arg| !arg.starts_with('-'))
                    .map(String::as_str),
                "dev",
            ),
            "next" => subcommand == "dev",
            "vite" => true,
            "uvicorn" => args.iter().any(|arg| arg == "--reload"),
            "python" | "python3" => args.iter().any(|arg| arg == "runserver"),
            _ => false,
        },
        MinedRole::Run => match tool {
            "cargo" | "go" => subcommand == "run",
            "npm" | "pnpm" | "yarn" | "bun" => {
                target_matches(package_script(), "start") || target_matches(package_script(), "run")
            }
            "make" | "just" => target_matches(
                args.iter()
                    .find(|arg| !arg.starts_with('-'))
                    .map(String::as_str),
                "run",
            ),
            "next" => subcommand == "start",
            _ => false,
        },
        MinedRole::Debug => {
            if backtrace_enabled == Some(true) {
                return true;
            }
            match tool {
                "npm" | "pnpm" | "yarn" | "bun" => target_matches(package_script(), "debug"),
                "make" | "just" => target_matches(
                    args.iter()
                        .find(|arg| !arg.starts_with('-'))
                        .map(String::as_str),
                    "debug",
                ),
                "node" => args.iter().any(|arg| arg.starts_with("--inspect")),
                _ => false,
            }
        }
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
    fn normalize_preserves_quoted_shell_separators_and_whitespace() {
        let parts = normalize_and_split(r#"python -c 'print("a;b && c")' && cargo test"#);
        assert_eq!(
            parts,
            vec![
                r#"python -c 'print("a;b && c")'"#.to_string(),
                "cargo test".to_string(),
            ]
        );

        let parts = normalize_and_split("printf 'a  b'");
        assert_eq!(parts, vec!["printf 'a  b'".to_string()]);
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
        assert_eq!(
            redact_secrets("deploy --token opaque-secret cargo test"),
            "deploy --token *** cargo test"
        );
        assert_eq!(
            redact_secrets("docker login --password hunter2 registry.example.com"),
            "docker login --password *** registry.example.com"
        );
        assert_eq!(
            redact_secrets("docker login --password 'correct horse' registry.example.com"),
            "docker login --password *** registry.example.com"
        );
        assert_eq!(
            redact_secrets("cargo test -- --exact 'a  b'"),
            "cargo test -- --exact 'a  b'"
        );
        assert_eq!(
            redact_secrets("docker login --password-stdin registry.example.com"),
            "docker login --password-stdin registry.example.com"
        );
        assert_eq!(
            redact_secrets(r#"curl -H "Authorization: Bearer opaque-secret" https://example.com"#),
            "curl -H *** https://example.com"
        );
        assert_eq!(
            redact_secrets("docker login -p hunter2 registry.example.com"),
            "docker login -p *** registry.example.com"
        );
        assert_eq!(
            redact_secrets("curl -u alice:hunter2 https://example.com"),
            "curl -u *** https://example.com"
        );
        assert_eq!(
            redact_secrets("curl https://alice:hunter2@example.com/status"),
            "curl ***"
        );
        assert_eq!(
            redact_secrets("curl --user=alice:hunter2 https://example.com"),
            "curl --user=*** https://example.com"
        );
        assert_eq!(
            redact_secrets("curl -ualice:hunter2 https://example.com"),
            "curl -u*** https://example.com"
        );
        assert_eq!(
            redact_secrets("curl --header=Authorization:Bearer_opaque https://example.com"),
            "curl --header=*** https://example.com"
        );
        assert_eq!(
            redact_secrets("sudo -u root docker login -p hunter2 registry.example.com"),
            "sudo -u root docker login -p *** registry.example.com"
        );
        assert_eq!(
            redact_secrets("curl https://ghp_secret@github.com/org/repo"),
            "curl ***"
        );
        assert_eq!(
            redact_secrets("env -C /tmp curl -u alice:hunter2 https://example.com"),
            "env -C /tmp curl -u *** https://example.com"
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
    fn opencode_path_matching_is_case_sensitive() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("opencode.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT);\n\
             CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, directory TEXT);\n\
             CREATE TABLE part (session_id TEXT, data TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, directory) VALUES (?1, ?2)",
            rusqlite::params!["wanted", "/work/App"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, directory) VALUES (?1, ?2)",
            rusqlite::params!["sibling", "/work/app/sub"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (session_id, data) VALUES (?1, ?2)",
            rusqlite::params![
                "wanted",
                r#"{"type":"tool","tool":"bash","state":{"input":{"command":"cargo test"}}}"#
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (session_id, data) VALUES (?1, ?2)",
            rusqlite::params![
                "sibling",
                r#"{"type":"tool","tool":"bash","state":{"input":{"command":"cargo build"}}}"#
            ],
        )
        .unwrap();
        drop(conn);

        let roots = AgentHistoryRoots {
            opencode_db: Some(db),
            ..AgentHistoryRoots::default()
        };
        let mined = mine_for_project_with_roots(Path::new("/work/App"), &roots);
        let commands = mined
            .iter()
            .map(|item| item.command.as_str())
            .collect::<Vec<_>>();

        assert!(commands.contains(&"cargo test"), "{commands:?}");
        assert!(!commands.contains(&"cargo build"), "{commands:?}");
    }

    #[test]
    fn mines_claude_bash_tool_use() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        fs::create_dir_all(&project).unwrap();
        let home = tmp.path().join("home");
        let enc = project.to_string_lossy().replace('/', "-");
        let session = home.join(".claude/projects").join(&enc).join("sess.jsonl");
        let cwd_line = serde_json::json!({ "cwd": project }).to_string();
        write_jsonl(
            &session,
            &[
                &cwd_line,
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#,
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#,
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}]}}"#,
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cd . && cargo nextest run"}}]}}"#,
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo check && cd ../sibling && ./deploy-prod"}}]}}"#,
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
        assert!(!cmds.contains(&"./deploy-prod"), "{cmds:?}");
        assert!(
            cmds.iter()
                .any(|c| c.contains("OPENAI_API_KEY=***") && c.contains("cargo build")),
            "{cmds:?}"
        );
        assert!(!cmds.iter().any(|c| c.contains("sk-")), "{cmds:?}");
    }

    #[test]
    fn encoded_directory_collision_does_not_cross_project_boundaries() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("a").join("b-c");
        let other_project = tmp.path().join("a-b").join("c");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&other_project).unwrap();
        let root = tmp.path().join("claude-projects");
        let encoding = project.to_string_lossy().replace('/', "-");
        assert_eq!(encoding, other_project.to_string_lossy().replace('/', "-"));
        let session = root.join(encoding).join("other.jsonl");
        write_jsonl(
            &session,
            &[
                &serde_json::json!({ "cwd": other_project }).to_string(),
                r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test -p other"}}]}}"#,
            ],
        );

        let roots = AgentHistoryRoots {
            claude_projects: Some(root),
            ..AgentHistoryRoots::default()
        };
        let mined = mine_for_project_with_roots(&project, &roots);

        assert!(mined.is_empty(), "{mined:?}");
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
        let cwd_line = serde_json::json!({ "cwd": project }).to_string();
        write_jsonl(
            &session,
            &[
                &cwd_line,
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
                &cwd_line,
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
        let sibling = tmp.path().join("sibling");
        let sibling_s = sibling.to_string_lossy();
        write_jsonl(
            &file,
            &[
                &format!(
                    r#"{{"type":"session_meta","payload":{{"cwd":"{project_s}","session_id":"x"}}}}"#
                ),
                r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo clippy\"}"}}"#,
                &format!(
                    r#"{{"type":"response_item","payload":{{"type":"function_call","name":"exec_command","arguments":"{{\"cmd\":\"cargo test -p sibling\",\"workdir\":\"{sibling_s}\"}}"}}}}"#
                ),
                r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo check -p escaped\",\"workdir\":\"../sibling\"}"}}"#,
                r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cd ../sibling && ./deploy-prod\"}"}}"#,
                r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"command cd ../sibling && ./wrapped-deploy\"}"}}"#,
                r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"builtin cd ../sibling && ./builtin-deploy\"}"}}"#,
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
                &format!(
                    r#"{{"type":"response_item","payload":{{"type":"function_call","name":"exec_command","arguments":"{{\"cmd\":\"cargo fmt\",\"workdir\":\"{project_s}\"}}"}}}}"#
                ),
            ],
        );

        let roots = AgentHistoryRoots {
            codex_sessions: Some(sessions),
            ..AgentHistoryRoots::default()
        };
        let mined = mine_for_project_with_roots(&project, &roots);
        let commands = mined
            .iter()
            .map(|item| item.command.as_str())
            .collect::<Vec<_>>();
        assert!(commands.contains(&"cargo clippy"), "{commands:?}");
        assert!(commands.contains(&"cargo fmt"), "{commands:?}");
        assert!(!commands.contains(&"cargo test -p sibling"), "{commands:?}");
        assert!(
            !commands.contains(&"cargo check -p escaped"),
            "{commands:?}"
        );
        assert!(!commands.contains(&"./deploy-prod"), "{commands:?}");
        assert!(!commands.contains(&"./wrapped-deploy"), "{commands:?}");
        assert!(!commands.contains(&"./builtin-deploy"), "{commands:?}");
        assert!(
            !commands.contains(&"cargo build -p sibling"),
            "{commands:?}"
        );
        assert!(mined.iter().all(|item| item.sources == ["codex"]));
    }

    #[test]
    fn codex_scanning_keeps_valid_lines_before_corrupt_history_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        fs::create_dir_all(&project).unwrap();
        let sessions = tmp.path().join("sessions");
        let file = sessions.join("rollout.jsonl");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let mut output = fs::File::create(&file).unwrap();
        writeln!(
            output,
            "{}",
            serde_json::json!({
                "type": "session_meta",
                "payload": { "cwd": project, "session_id": "corrupt" }
            })
        )
        .unwrap();
        output
            .write_all(
                br#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo test\"}"}}"#,
            )
            .unwrap();
        output.write_all(b"\n").unwrap();
        output.write_all(&[0xff, b'\n']).unwrap();
        drop(output);

        let roots = AgentHistoryRoots {
            codex_sessions: Some(sessions),
            ..AgentHistoryRoots::default()
        };
        let mined = mine_for_project_with_roots(&project, &roots);

        assert!(
            mined.iter().any(|item| item.command == "cargo test"),
            "{mined:?}"
        );
    }

    #[test]
    fn codex_scan_budget_counts_overlapping_sessions_without_commands() {
        let mut scanned = 0;
        for _ in 0..MAX_SESSION_FILES_PER_AGENT {
            assert!(reserve_codex_session_scan(&mut scanned, true));
        }
        assert!(!reserve_codex_session_scan(&mut scanned, true));
        assert_eq!(scanned, MAX_SESSION_FILES_PER_AGENT);

        let mut unrelated = 0;
        assert!(!reserve_codex_session_scan(&mut unrelated, false));
        assert_eq!(unrelated, 0);
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

    #[test]
    fn merge_does_not_treat_git_branch_names_as_command_roles() {
        let mut profile = crate::project_profile::ProjectProfile {
            name: "x".into(),
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
            entry_points: vec![],
            agent_commands: vec![],
        };
        let mined = vec![
            MinedCommand {
                command: "git checkout test".into(),
                count: 10,
                sources: vec!["codex".into()],
            },
            MinedCommand {
                command: "git switch dev".into(),
                count: 9,
                sources: vec!["codex".into()],
            },
            MinedCommand {
                command: "cargo nextest run".into(),
                count: 2,
                sources: vec!["codex".into()],
            },
        ];

        merge_mined_into_profile(&mut profile, mined);

        assert_eq!(profile.test_command.as_deref(), Some("cargo nextest run"));
        assert!(profile.dev_command.is_none());
    }

    #[test]
    fn merge_does_not_treat_arbitrary_arguments_as_roles() {
        let mut profile = crate::project_profile::ProjectProfile {
            name: "x".into(),
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
            entry_points: vec![],
            agent_commands: vec![],
        };
        let mined = vec![
            MinedCommand {
                command: "docker build --build-arg RUST_BACKTRACE=1 -t test .".into(),
                count: 10,
                sources: vec!["codex".into()],
            },
            MinedCommand {
                command: "kubectl config use-context dev".into(),
                count: 9,
                sources: vec!["codex".into()],
            },
            MinedCommand {
                command: "cargo nextest run".into(),
                count: 2,
                sources: vec!["codex".into()],
            },
            MinedCommand {
                command: "npm run dev".into(),
                count: 1,
                sources: vec!["codex".into()],
            },
            MinedCommand {
                command: "RUST_BACKTRACE=0 cargo run".into(),
                count: 1,
                sources: vec!["codex".into()],
            },
        ];

        merge_mined_into_profile(&mut profile, mined);

        assert_eq!(profile.test_command.as_deref(), Some("cargo nextest run"));
        assert_eq!(
            profile.build_command.as_deref(),
            Some("docker build --build-arg RUST_BACKTRACE=1 -t test .")
        );
        assert_eq!(profile.dev_command.as_deref(), Some("npm run dev"));
        assert!(profile.debug_command.is_none());
    }

    #[test]
    fn debug_role_requires_an_enabled_leading_backtrace_assignment() {
        assert!(command_matches_role(
            "RUST_BACKTRACE=1 cargo run",
            MinedRole::Debug
        ));
        assert!(command_matches_role(
            "env RUST_BACKTRACE=full cargo run",
            MinedRole::Debug
        ));
        assert!(!command_matches_role(
            "RUST_BACKTRACE=0 cargo run",
            MinedRole::Debug
        ));
        assert!(!command_matches_role(
            "RUST_BACKTRACE= cargo run",
            MinedRole::Debug
        ));
        assert!(!command_matches_role(
            "docker build --build-arg RUST_BACKTRACE=1 .",
            MinedRole::Debug
        ));
    }
}
