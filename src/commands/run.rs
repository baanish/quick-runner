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

pub fn parse_args(config: &AppConfig, parts: &[String]) -> Result<(RunMode, String)> {
    if parts.is_empty() {
        return Err(anyhow!("Usage: qr run [watch|log|output] <script>"));
    }

    if let Some(mode) = RunMode::parse(&parts[0]) {
        if parts.len() < 2 {
            return Err(anyhow!("Missing script after mode '{}'", parts[0]));
        }
        return Ok((mode, parts[1..].join(" ")));
    }

    let default = RunMode::parse(&config.general.default_run_mode).ok_or_else(|| {
        anyhow!(
            "Invalid default_run_mode '{}'",
            config.general.default_run_mode
        )
    })?;
    Ok((default, parts.join(" ")))
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
    let mut file = File::create(&log_path)?;
    let output = shell_command(script)
        .output()
        .with_context(|| format!("Failed to execute script '{script}'"))?;
    file.write_all(&output.stdout)?;
    file.write_all(&output.stderr)?;
    writeln!(io::stdout(), "log written to {log_path}")?;

    Ok(result_from_status(output.status, Some(log_path)))
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
    fn run_args_use_default_mode() {
        let mut config = AppConfig::load_from_env_with_path(
            tempfile::tempdir().unwrap().path().join("config.toml"),
        )
        .unwrap();
        config.general.default_run_mode = "watch".into();

        let (mode, script) = parse_args(&config, &["echo".into(), "hi".into()]).unwrap();
        assert_eq!(mode, RunMode::Watch);
        assert_eq!(script, "echo hi");
    }

    #[test]
    fn run_args_detect_explicit_mode() {
        let config = AppConfig::load_from_env_with_path(
            tempfile::tempdir().unwrap().path().join("config.toml"),
        )
        .unwrap();

        let (mode, script) =
            parse_args(&config, &["log".into(), "cargo".into(), "test".into()]).unwrap();
        assert_eq!(mode, RunMode::Log);
        assert_eq!(script, "cargo test");
    }
}
