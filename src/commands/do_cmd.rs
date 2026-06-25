use std::{
    env,
    io::{self, Write},
    process::Command,
};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::{
    ai::client::{AiClient, AiResponse},
    config::{AgentConfig, AppConfig},
    project_profile::{ProjectProfile, discover_project_root, load_profile_from},
};

const CLASSIFICATION_PROMPT: &str = r#"You are QuickRunner's router.
Classify the user's task as either:
- inline: a single shell command is enough
- delegate: the task is multi-step coding work and should be handed to a coding agent

Return JSON only.
For inline, return {"classification":"inline","command":"...","reason":"..."}.
For delegate, return {"classification":"delegate","reason":"..."}.
Do not include markdown fences or commentary."#;

#[derive(Debug)]
pub struct DoResult {
    pub ai_response: AiResponse,
    pub outcome: DoOutcome,
}

#[derive(Debug)]
pub enum DoOutcome {
    Inline {
        command: String,
        executed: bool,
        exit_code: i32,
    },
    Delegate {
        suggestions: Vec<String>,
    },
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
struct ParsedClassification {
    classification: ClassificationKind,
    command: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ClassificationKind {
    Inline,
    Delegate,
}

pub fn execute(config: &AppConfig, client: &AiClient, task: &str) -> Result<DoResult> {
    let profile = load_current_profile().ok();
    let prompt = build_user_prompt(task, profile.as_ref())?;
    let ai_response = client.execute_prompt(CLASSIFICATION_PROMPT, &prompt)?;
    let parsed = parse_classification(&ai_response.output_text)?;

    let outcome = match parsed.classification {
        ClassificationKind::Inline => {
            let command = parsed
                .command
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow!("AI classified task as inline but did not return a command")
                })?;
            print_inline_preview(&command, parsed.reason.as_deref())?;
            let (executed, exit_code) = confirm_and_run_inline(command.as_str())?;

            DoOutcome::Inline {
                command,
                executed,
                exit_code,
            }
        }
        ClassificationKind::Delegate => {
            let suggestions =
                build_delegate_suggestions(&config.do_config.agents, task, profile.as_ref());
            print_delegate_suggestions(&suggestions, parsed.reason.as_deref())?;
            DoOutcome::Delegate { suggestions }
        }
    };

    Ok(DoResult {
        ai_response,
        outcome,
    })
}

fn parse_classification(raw: &str) -> Result<ParsedClassification> {
    serde_json::from_str(raw).context("Failed to parse AI classification JSON")
}

fn build_user_prompt(task: &str, profile: Option<&ProjectProfile>) -> Result<String> {
    let mut prompt = format!("Task: {task}\n");
    if let Some(profile) = profile {
        prompt.push_str("Project profile:\n");
        prompt.push_str(&serde_json::to_string_pretty(profile)?);
        prompt.push('\n');
    }
    Ok(prompt)
}

fn load_current_profile() -> Result<ProjectProfile> {
    let cwd = env::current_dir()?;
    let root = discover_project_root(&cwd);
    load_profile_from(&root)
}

fn print_inline_preview(command: &str, reason: Option<&str>) -> Result<()> {
    println!("→ {command}");
    if let Some(reason) = reason.filter(|value| !value.trim().is_empty()) {
        println!("  {reason}");
    }
    Ok(())
}

/// Confirm and run an inline command. Every AI-generated command gets the same
/// single confirmation: `confirm` defaults to No and the full command is shown in
/// the preview first, so a command like `git status; rm -rf ~` cannot run unless
/// the user explicitly types `y` after seeing it in full. The security boundary is
/// the default-No prompt plus the preview — there is deliberately no per-command
/// classification or allow-list, since an allow-listed program name doesn't bound
/// what that program does. Returns (executed, exit_code).
fn confirm_and_run_inline(command: &str) -> Result<(bool, i32)> {
    if !confirm("Run this command?")? {
        return Ok((false, 0));
    }
    Ok((true, run_shell_command(command)?))
}

/// Prompt for explicit confirmation; always defaults to NO. AI-generated
/// commands never run on a bare Enter — the user must type `y`/`yes`.
fn confirm(label: &str) -> Result<bool> {
    print!("{label} [y/N] ");
    io::stdout().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn run_shell_command(command: &str) -> Result<i32> {
    let status = Command::new("/bin/sh").arg("-c").arg(command).status()?;
    Ok(super::exit_code(status))
}

fn build_delegate_suggestions(
    agents: &AgentConfig,
    task: &str,
    profile: Option<&ProjectProfile>,
) -> Vec<String> {
    let escaped = crate::shell::sh_single_quote(task);
    let mut suggestions =
        if profile.and_then(|value| value.prefer_agent.as_deref()) == Some("claude") {
            vec![
                format!("{} {}", agents.claude, escaped),
                format!("{} {}", agents.codex, escaped),
            ]
        } else {
            vec![
                format!("{} {}", agents.codex, escaped),
                format!("{} {}", agents.claude, escaped),
            ]
        };
    suggestions.dedup();
    suggestions
}

fn print_delegate_suggestions(suggestions: &[String], reason: Option<&str>) -> Result<()> {
    println!("🧠 This looks like a multi-step coding task.");
    if let Some(reason) = reason.filter(|value| !value.trim().is_empty()) {
        println!("  {reason}");
    }
    for suggestion in suggestions {
        println!("→ Suggested: {suggestion}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_classification_json() {
        let parsed = parse_classification(
            r#"{"classification":"inline","command":"cargo test","reason":"single command"}"#,
        )
        .unwrap();
        assert_eq!(parsed.classification, ClassificationKind::Inline);
        assert_eq!(parsed.command.as_deref(), Some("cargo test"));
    }

    #[test]
    fn prefer_agent_reorders_delegate_suggestions() {
        let mut profile = ProjectProfile {
            name: "demo".into(),
            language: Some("rust".into()),
            framework: None,
            package_manager: Some("cargo".into()),
            test_command: Some("cargo test".into()),
            build_command: Some("cargo build".into()),
            lint_command: Some("cargo clippy".into()),
            scripts: Default::default(),
            prefer_agent: Some("claude".into()),
            entry_points: vec![],
        };
        let suggestions =
            build_delegate_suggestions(&AgentConfig::default(), "refactor auth", Some(&profile));
        assert!(suggestions[0].starts_with("claude "));

        profile.prefer_agent = None;
        let suggestions =
            build_delegate_suggestions(&AgentConfig::default(), "refactor auth", Some(&profile));
        assert!(suggestions[0].starts_with("codex "));
    }
}
