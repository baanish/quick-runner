use anyhow::Result;

use crate::project_profile::{LearnResult, learn_current_dir};

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
        println!("  → Test: {test_command}");
    }
    if let Some(build_command) = &result.profile.build_command {
        println!("  → Build: {build_command}");
    }
    if let Some(lint_command) = &result.profile.lint_command {
        println!("  → Lint: {lint_command}");
    }
    if let Some(dev_command) = &result.profile.dev_command {
        println!("  → Dev: {dev_command}");
    }
    if let Some(run_command) = &result.profile.run_command {
        println!("  → Run: {run_command}");
    }
    if let Some(debug_command) = &result.profile.debug_command {
        println!("  → Debug: {debug_command}");
    }
    if !result.profile.scripts.is_empty() {
        let names = result.profile.scripts.keys().cloned().collect::<Vec<_>>();
        println!("  → Scripts: {}", names.join(", "));
    }
    if !result.profile.agent_commands.is_empty() {
        println!(
            "  → Agent-mined commands: {} (top: {})",
            result.profile.agent_commands.len(),
            result
                .profile
                .agent_commands
                .iter()
                .take(5)
                .map(|c| c.command.as_str())
                .collect::<Vec<_>>()
                .join(", ")
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
