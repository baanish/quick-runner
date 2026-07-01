use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Output,
};

use anyhow::{Context, Result, anyhow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
}

impl ShellKind {
    pub fn alias_line(self, name: &str, command: &str) -> String {
        match self {
            Self::Zsh | Self::Bash => format!("alias {name}={}", sh_single_quote(command)),
            Self::Fish => format!("alias {name} {}", fish_single_quote(command)),
        }
    }

    pub fn rc_path(self, home: &Path) -> PathBuf {
        match self {
            Self::Zsh => home.join(".zshrc"),
            Self::Bash => home.join(".bashrc"),
            Self::Fish => home.join(".config/fish/config.fish"),
        }
    }
}

pub fn detect_shell() -> ShellKind {
    detect_shell_from(env::var("SHELL").ok().as_deref())
}

pub fn detect_shell_from(shell: Option<&str>) -> ShellKind {
    match shell.unwrap_or("").rsplit('/').next().unwrap_or("") {
        "fish" => ShellKind::Fish,
        "bash" => ShellKind::Bash,
        _ => ShellKind::Zsh,
    }
}

pub fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow!("Could not resolve home directory"))
}

pub fn load_aliases(path: &Path) -> Result<Vec<(String, String)>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let raw = fs::read_to_string(path)?;
    let aliases = raw.lines().filter_map(parse_alias_line).collect::<Vec<_>>();
    Ok(aliases)
}

pub fn add_or_update_alias(
    path: &Path,
    shell: ShellKind,
    name: &str,
    command: &str,
) -> Result<bool> {
    // The alias *value* is shell-quoted, but the *name* is emitted verbatim and
    // the line is stored in a line-oriented rc file. Reject names that aren't a
    // plain identifier (otherwise `qr alias add 'x; reboot #' …` would write an
    // rc line that runs `reboot` on the next shell start) and commands that span
    // lines (a newline would split one alias across physical rc lines, which
    // `remove_alias` then can't cleanly remove).
    validate_alias_name(name)?;
    validate_alias_command(command)?;

    let mut lines = read_lines(path)?;
    let alias_line = shell.alias_line(name, command);
    let mut replaced = false;

    for line in &mut lines {
        if parse_alias_line(line).is_some_and(|(alias_name, _)| alias_name == name) {
            *line = alias_line.clone();
            replaced = true;
        }
    }

    if !replaced {
        lines.push(alias_line);
    }

    write_lines(path, &lines)?;
    Ok(replaced)
}

pub fn remove_alias(path: &Path, name: &str) -> Result<bool> {
    let lines = read_lines(path)?;
    let original_len = lines.len();
    let filtered = lines
        .into_iter()
        .filter(|line| parse_alias_line(line).is_none_or(|(alias_name, _)| alias_name != name))
        .collect::<Vec<_>>();
    write_lines(path, &filtered)?;
    Ok(filtered.len() != original_len)
}

pub fn shell_wrapper_snippet(shell: ShellKind, binary_name: &str) -> String {
    match shell {
        ShellKind::Fish => format!(
            r#"function {binary_name}
    if test (count $argv) -gt 0; and test $argv[1] = go -o $argv[1] = g
        set -l target (command {binary_name} go $argv[2..-1] --print-path)
        if test -n "$target"
            cd $target
        end
    else
        command {binary_name} $argv
    end
end"#
        ),
        ShellKind::Zsh | ShellKind::Bash => format!(
            r#"{binary_name}() {{
  if [ "$1" = "go" ] || [ "$1" = "g" ]; then
    local dir
    dir=$(command {binary_name} go "${{@:2}}" --print-path)
    if [ -n "$dir" ]; then
      cd "$dir"
    fi
  else
    command {binary_name} "$@"
  fi
}}"#
        ),
    }
}

pub fn ensure_wrapper_present(path: &Path, snippet: &str) -> Result<bool> {
    let mut lines = read_lines(path)?;
    if lines.join("\n").contains(snippet) {
        return Ok(false);
    }
    if !lines.is_empty() && !lines.last().unwrap().is_empty() {
        lines.push(String::new());
    }
    lines.extend(snippet.lines().map(ToOwned::to_owned));
    write_lines(path, &lines)?;
    Ok(true)
}

/// Single-quote a value for a POSIX shell (sh/bash/zsh): wrap in `'…'` and turn
/// each embedded `'` into `'\''`. Nothing inside can break out, so it is safe to
/// embed an arbitrary string as a single shell word. Shared across the alias,
/// cron, and `qr run` log-tee paths.
pub(crate) fn sh_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Single-quote a value for fish, where only `\` and `'` are special inside `'…'`.
fn fish_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', r"\\").replace('\'', r"\'"))
}

pub fn cron_line(binary_path: &Path) -> String {
    // Quote the path: cron runs the line through /bin/sh, so a path with spaces
    // or shell metacharacters would otherwise break the entry or inject.
    format!(
        "0 * * * * {} scan >/dev/null 2>&1",
        sh_single_quote(&binary_path.display().to_string())
    )
}

pub fn merge_cron_entry(existing: &str, new_line: &str) -> Option<String> {
    if existing.contains(new_line) {
        return None;
    }

    let mut merged = existing.to_string();
    if !merged.ends_with('\n') && !merged.is_empty() {
        merged.push('\n');
    }
    merged.push_str(new_line);
    merged.push('\n');
    Some(merged)
}

pub fn crontab_contents_from_list_output(output: &Output) -> Result<String> {
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }

    if is_known_no_crontab_message(&output.stdout) || is_known_no_crontab_message(&output.stderr) {
        return Ok(String::new());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };
    Err(anyhow!("`crontab -l` failed: {detail}"))
}

fn is_known_no_crontab_message(stream: &[u8]) -> bool {
    let message = String::from_utf8_lossy(stream);
    message.to_ascii_lowercase().contains("no crontab for")
}

fn validate_alias_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let valid = match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() || first == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "Invalid alias name '{name}': use letters, digits, '_', '-' or '.', starting with a letter, digit, or '_'"
        ))
    }
}

fn validate_alias_command(command: &str) -> Result<()> {
    if command.contains(['\n', '\r', '\0']) {
        Err(anyhow!(
            "Alias command must be a single line (no newline or NUL characters)"
        ))
    } else {
        Ok(())
    }
}

fn parse_alias_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    let body = trimmed.strip_prefix("alias ")?;

    // POSIX form `alias name=<value>`, but only when `=` precedes any whitespace.
    // Otherwise the `=` belongs to the command (e.g. fish's `alias e 'echo a=b'`)
    // and must not be mistaken for the name/value separator.
    if let Some(split_index) = body.find('=') {
        if !body[..split_index].contains(char::is_whitespace) {
            // Trim quotes so a pre-existing `alias 'gs'='git status'` still
            // matches the bare name `gs` for list/remove/update.
            let name = body[..split_index].trim().trim_matches('\'').to_string();
            let command = decode_posix_value(body[split_index + 1..].trim());
            return Some((name, command));
        }
    }

    // fish form: `alias name command`.
    let mut parts = body.splitn(2, ' ');
    let name = parts.next()?.trim().trim_matches('\'').to_string();
    let command = decode_fish_value(parts.next()?.trim());
    Some((name, command))
}

/// Decode a POSIX-shell single-quoted value back to the original command. Our
/// serializer (`sh_single_quote`) always emits one valid shell word, so shlex
/// inverts it exactly; hand-edited lines that aren't valid shell words fall back
/// to a best-effort quote trim.
fn decode_posix_value(value: &str) -> String {
    match shlex::split(value) {
        Some(tokens) if !tokens.is_empty() => tokens.join(" "),
        _ => value.trim_matches('\'').trim_matches('"').to_string(),
    }
}

/// Decode a fish single-quoted value, reversing `fish_single_quote` (`\\` -> `\`,
/// `\'` -> `'`). Unquoted values pass through unchanged.
fn decode_fish_value(value: &str) -> String {
    let inner = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or(value);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && matches!(chars.peek(), Some('\\') | Some('\'')) {
            out.push(chars.next().unwrap());
        } else {
            out.push(c);
        }
    }
    out
}

fn read_lines(path: &Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))
        .map(|raw| raw.lines().map(ToOwned::to_owned).collect())
}

fn write_lines(path: &Path, lines: &[String]) -> Result<()> {
    let mut output = lines.join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    // Atomic write so an interrupted update can never truncate/corrupt the
    // user's shell rc file (it also creates the parent directory).
    crate::atomic::write(path, output.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::{os::unix::process::ExitStatusExt, process::Output};

    #[cfg(unix)]
    fn output(status: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(status << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn alias_helpers_add_list_and_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join(".zshrc");

        let replaced = add_or_update_alias(&rc, ShellKind::Zsh, "ll", "ls -la").unwrap();
        assert!(!replaced);

        let aliases = load_aliases(&rc).unwrap();
        assert_eq!(aliases, vec![("ll".to_string(), "ls -la".to_string())]);

        let removed = remove_alias(&rc, "ll").unwrap();
        assert!(removed);
        assert!(load_aliases(&rc).unwrap().is_empty());
    }

    #[test]
    fn wrapper_generation_matches_shell() {
        assert!(shell_wrapper_snippet(ShellKind::Zsh, "qr").contains(r#"dir=$(command qr go"#));
        assert!(shell_wrapper_snippet(ShellKind::Fish, "qr").contains("function qr"));
    }

    #[test]
    fn fish_alias_lines_are_parsed() {
        let alias = parse_alias_line("alias gs 'git status'").unwrap();
        assert_eq!(alias, ("gs".to_string(), "git status".to_string()));
    }

    #[test]
    fn sh_single_quote_is_injection_safe() {
        // Quoting must let /bin/sh treat any string as a single literal word —
        // metacharacters and quotes included — so nothing can break out and run.
        for value in [
            "plain",
            "two words",
            "has 'single' quotes",
            "back\\slash",
            "; rm -rf /",
            "$(whoami)",
            "tick`s`",
        ] {
            let quoted = sh_single_quote(value);
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("printf %s {quoted}"))
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                value,
                "round-trip failed for {value:?} (quoted: {quoted})"
            );
        }
    }

    #[test]
    fn alias_line_keeps_an_embedded_quote_contained() {
        // A command containing a quote (and a chained command) must stay inside
        // the alias quoting rather than injecting into the rc file.
        let cmd = "echo 'hi' && date";
        let line = ShellKind::Zsh.alias_line("g", cmd);
        let quoted = line.strip_prefix("alias g=").unwrap();
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf %s {quoted}"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), cmd);
    }

    #[test]
    fn cron_line_quotes_the_binary_path() {
        let line = cron_line(Path::new("/opt/my tools/qr"));
        assert!(
            line.contains("'/opt/my tools/qr'"),
            "binary path not quoted: {line}"
        );
    }

    #[test]
    fn alias_command_with_single_quotes_round_trips() {
        // Writing then loading an alias whose command contains single quotes must
        // return the command verbatim, not a quote-stripped corruption.
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join(".zshrc");
        for command in ["echo 'hi'", "'", "git commit -m 'wip'", "a'b'c"] {
            add_or_update_alias(&rc, ShellKind::Zsh, "g", command).unwrap();
            let aliases = load_aliases(&rc).unwrap();
            let loaded = aliases.iter().find(|(n, _)| n == "g").map(|(_, c)| c);
            assert_eq!(
                loaded.map(String::as_str),
                Some(command),
                "round-trip lost data for {command:?}"
            );
        }
    }

    #[test]
    fn alias_name_with_shell_metacharacters_is_rejected() {
        // A name carrying a chained command must be refused before it can be
        // written raw into the rc file and executed on the next shell start.
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join(".zshrc");
        let result = add_or_update_alias(&rc, ShellKind::Zsh, "x; touch pwn #", "echo ok");
        assert!(
            result.is_err(),
            "injection-shaped alias name must be rejected"
        );
        assert!(!rc.exists(), "rejected alias must not write the rc file");
    }

    #[test]
    fn alias_command_with_newline_is_rejected() {
        // A newline would split one alias across physical rc lines, leaving an
        // orphan line that `remove_alias` cannot cleanly remove.
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join(".zshrc");
        let result = add_or_update_alias(&rc, ShellKind::Zsh, "n", "echo\ntouch pwn #");
        assert!(result.is_err(), "multi-line alias command must be rejected");
    }

    #[test]
    fn quoted_alias_name_is_normalized_on_parse() {
        // A pre-existing rc line with a quoted name must still resolve to the
        // bare name so list/remove/update keep matching it.
        let parsed = parse_alias_line("alias 'gs'='git status'").unwrap();
        assert_eq!(parsed, ("gs".to_string(), "git status".to_string()));
    }

    #[test]
    fn fish_alias_with_equals_in_command_parses() {
        // The `=` inside the command must not be mistaken for the POSIX
        // name/value separator.
        let parsed = parse_alias_line("alias e 'echo a=b'").unwrap();
        assert_eq!(parsed, ("e".to_string(), "echo a=b".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn merge_cron_entry_adds_new_line() {
        let merged = merge_cron_entry("MAILTO=user@example.com\n", "0 * * * * '/tmp/qr' scan")
            .expect("new cron entry should be appended");
        assert_eq!(
            merged,
            "MAILTO=user@example.com\n0 * * * * '/tmp/qr' scan\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn crontab_list_output_treats_no_crontab_as_empty() {
        for stderr in [
            "no crontab for ubuntu\n",
            "crontab: no crontab for ubuntu\n",
        ] {
            let existing = crontab_contents_from_list_output(&output(1, "", stderr))
                .expect("known no-crontab message should map to an empty crontab");
            assert!(existing.is_empty(), "expected empty crontab for {stderr:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn crontab_list_output_rejects_other_failures() {
        let err = crontab_contents_from_list_output(&output(1, "", "permission denied\n"))
            .expect_err("unexpected crontab -l failures must abort installation");
        let message = err.to_string();
        assert!(
            message.contains("crontab -l"),
            "missing command context: {message}"
        );
        assert!(
            message.contains("permission denied"),
            "missing stderr detail: {message}"
        );
    }
}
