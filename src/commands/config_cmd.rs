use std::{env, fs, path::PathBuf, process::Command};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Subcommand};

use crate::config::config_file_path;

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: Option<ConfigSubcommand>,
}

#[derive(Subcommand)]
pub enum ConfigSubcommand {
    Path,
}

pub fn execute(args: ConfigArgs) -> Result<()> {
    let path = config_file_path();

    match args.command {
        Some(ConfigSubcommand::Path) => {
            println!("{}", path.display());
            Ok(())
        }
        None => open_in_editor(path),
    }
}

fn open_in_editor(path: PathBuf) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory {}", parent.display()))?;
    }

    println!("{}", path.display());

    let editor = env::var("EDITOR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env::var("VISUAL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| {
            if command_exists("vim") {
                "vim".into()
            } else {
                "nano".into()
            }
        });

    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("editor command cannot be empty"))?;
    let status = Command::new(program)
        .args(parts)
        .arg(&path)
        .status()
        .with_context(|| format!("Failed to launch editor '{program}'"))?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("Editor exited with status {status}"))
    }
}

fn command_exists(program: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file()
        })
    })
}
