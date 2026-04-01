use std::{
    env, fs,
    path::{Path, PathBuf},
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
            Self::Zsh | Self::Bash => format!("alias {name}='{command}'"),
            Self::Fish => format!("alias {name} '{command}'"),
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
    if test (count $argv) -gt 0; and test $argv[1] = go
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
  if [ "$1" = "go" ]; then
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

pub fn cron_line(binary_path: &Path) -> String {
    format!("0 * * * * {} scan >/dev/null 2>&1", binary_path.display())
}

fn parse_alias_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    let body = trimmed.strip_prefix("alias ")?;
    if let Some(split_index) = body.find('=') {
        let name = body[..split_index].trim().trim_matches('\'').to_string();
        let command = body[split_index + 1..]
            .trim()
            .trim_matches('\'')
            .trim_matches('"')
            .to_string();
        return Some((name, command));
    }

    let mut parts = body.splitn(2, ' ');
    let name = parts.next()?.trim().trim_matches('\'').to_string();
    let command = parts
        .next()?
        .trim()
        .trim_matches('\'')
        .trim_matches('"')
        .to_string();
    Some((name, command))
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = lines.join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    fs::write(path, output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
