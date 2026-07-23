use anyhow::Result;

use crate::agent_history::MinedCommand;
use crate::project_profile::{LearnResult, learn_current_dir};
use crate::terminal;

pub fn execute() -> Result<LearnResult> {
    learn_current_dir()
}

pub fn print_summary(result: &LearnResult) {
    println!("📖 Learning {}...", result.profile.name);
    println!("  → Root: {}", result.project_root.display());
    if let Some(language) = &result.profile.language {
        if let Some(framework) = &result.profile.framework {
            println!("  → Detected: {language} + {framework}");
        } else {
            println!("  → Detected: {language}");
        }
    }
    if let Some(package_manager) = &result.profile.package_manager {
        println!("  → Package manager: {package_manager}");
    }
    if let Some(test_command) = &result.profile.test_command {
        println!("  → Test: {}", role_command_preview(test_command));
    }
    if let Some(build_command) = &result.profile.build_command {
        println!("  → Build: {}", role_command_preview(build_command));
    }
    if let Some(lint_command) = &result.profile.lint_command {
        println!("  → Lint: {}", role_command_preview(lint_command));
    }
    if let Some(dev_command) = &result.profile.dev_command {
        println!("  → Dev: {}", role_command_preview(dev_command));
    }
    if let Some(run_command) = &result.profile.run_command {
        println!("  → Run: {}", role_command_preview(run_command));
    }
    if let Some(debug_command) = &result.profile.debug_command {
        println!("  → Debug: {}", role_command_preview(debug_command));
    }
    if !result.profile.scripts.is_empty() {
        let names = result.profile.scripts.keys().cloned().collect::<Vec<_>>();
        println!("  → Scripts: {}", names.join(", "));
    }
    if !result.profile.agent_commands.is_empty() {
        println!(
            "  → Agent-mined commands: {} (top: {})",
            result.profile.agent_commands.len(),
            agent_command_preview(&result.profile.agent_commands)
        );
    }
    if !result.profile.entry_points.is_empty() {
        println!(
            "  → Entry points: {}",
            result.profile.entry_points.join(", ")
        );
    }
    println!("✅ Saved to {}", result.profile_path.display());
}

fn agent_command_preview(commands: &[MinedCommand]) -> String {
    commands
        .iter()
        .take(5)
        .map(|command| terminal::escape_untrusted(&command.command))
        .collect::<Vec<_>>()
        .join(", ")
}

fn role_command_preview(command: &str) -> String {
    terminal::escape_untrusted(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_command_preview_escapes_terminal_controls() {
        let commands = vec![MinedCommand {
            command: "cargo test\u{1b}]52;c;payload\u{7}".into(),
            count: 1,
            sources: vec!["codex".into()],
        }];

        let preview = agent_command_preview(&commands);

        assert!(!preview.contains('\u{1b}'));
        assert!(!preview.contains('\u{7}'));
        assert!(preview.contains("\\u{1b}"));
        assert!(preview.contains("\\u{7}"));
    }

    #[test]
    fn role_command_preview_escapes_terminal_controls() {
        let preview = role_command_preview("cargo test\u{1b}[2J");

        assert!(!preview.contains('\u{1b}'));
        assert!(preview.contains("\\u{1b}"));
    }
}
