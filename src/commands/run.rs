use std::{
    fs::File,
    io::{self, Write},
    process::{Command, ExitStatus, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use indicatif::{ProgressBar, ProgressStyle};

use crate::config::AppConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Watch,
    Log,
    Output,
}

impl RunMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "watch" => Some(Self::Watch),
            "log" => Some(Self::Log),
            "output" => Some(Self::Output),
            _ => None,
        }
    }
}

pub struct RunResult {
    pub exit_code: i32,
    pub log_path: Option<String>,
}

/// Resolve the run mode from the mutually-exclusive `--watch`/`--log`/`--output`
/// flags, falling back to the configured default when none is set. The script is
/// always the positional argument(s), so a command starting with `watch`/`log`/
/// `output` is no longer mistaken for a mode (clap guarantees at most one flag).
pub fn resolve_mode(config: &AppConfig, watch: bool, log: bool, output: bool) -> Result<RunMode> {
    if watch {
        Ok(RunMode::Watch)
    } else if log {
        Ok(RunMode::Log)
    } else if output {
        Ok(RunMode::Output)
    } else {
        RunMode::parse(&config.general.default_run_mode).ok_or_else(|| {
            anyhow!(
                "Invalid default_run_mode '{}'",
                config.general.default_run_mode
            )
        })
    }
}

pub fn execute(mode: RunMode, script: &str) -> Result<RunResult> {
    match mode {
        RunMode::Output => run_output(script),
        RunMode::Watch => run_watch(script),
        RunMode::Log => run_log(script),
    }
}

fn run_output(script: &str) -> Result<RunResult> {
    let status = shell_command(script)
        .status()
        .with_context(|| format!("Failed to execute script '{script}'"))?;
    Ok(result_from_status(status, None))
}

fn run_watch(script: &str) -> Result<RunResult> {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(ProgressStyle::with_template("{spinner} {msg}")?);
    spinner.set_message("Running...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    let output = shell_command(script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("Failed to execute script '{script}'"))?;

    spinner.finish_and_clear();
    if output.status.success() {
        println!("✅ exit 0");
    } else {
        println!("❌ exit {}", output.status.code().unwrap_or(1));
    }

    Ok(result_from_status(output.status, None))
}

fn run_log(script: &str) -> Result<RunResult> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let log_path = format!("qr-log-{timestamp}.log");
    let file = File::create(&log_path)?;

    // Use shell-level tee so output streams live to terminal AND to the log file
    let tee_script = format!(
        "{{ {} ; }} 2>&1 | tee -a {}",
        script,
        shell_escape(&log_path)
    );
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(&tee_script)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to execute script '{script}'"))?;

    let status = child.wait()?;
    drop(file);
    writeln!(io::stdout(), "log written to {log_path}")?;

    Ok(result_from_status(status, Some(log_path)))
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\'"))
}

fn shell_command(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(script);
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());
    command
}

fn result_from_status(status: ExitStatus, log_path: Option<String>) -> RunResult {
    RunResult {
        exit_code: status.code().unwrap_or(1),
        log_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn resolve_mode_uses_default_when_no_flag() {
        let mut config = AppConfig::load_from_env_with_path(
            tempfile::tempdir().unwrap().path().join("config.toml"),
        )
        .unwrap();
        config.general.default_run_mode = "watch".into();

        assert_eq!(
            resolve_mode(&config, false, false, false).unwrap(),
            RunMode::Watch
        );
    }

    #[test]
    fn resolve_mode_flag_overrides_default() {
        let config = AppConfig::load_from_env_with_path(
            tempfile::tempdir().unwrap().path().join("config.toml"),
        )
        .unwrap();

        assert_eq!(resolve_mode(&config, false, true, false).unwrap(), RunMode::Log);
        assert_eq!(
            resolve_mode(&config, true, false, false).unwrap(),
            RunMode::Watch
        );
    }
}
