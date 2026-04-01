use std::io::{self, Write};

use anyhow::{Result, anyhow};

use crate::shell::{
    self, add_or_update_alias, detect_shell, ensure_wrapper_present, load_aliases, remove_alias,
};

pub enum AliasCommand {
    Add { name: String, command: String },
    List,
    Remove { name: String },
}

pub fn execute(command: AliasCommand) -> Result<()> {
    let shell = detect_shell();
    let home = shell::home_dir()?;
    let rc_path = shell.rc_path(&home);

    match command {
        AliasCommand::Add { name, command } => {
            let aliases = load_aliases(&rc_path)?;
            if let Some((_, current)) = aliases.iter().find(|(alias_name, _)| alias_name == &name) {
                println!("alias '{name}' already exists: {current}");
                print!("Edit? (y/n) ");
                io::stdout().flush()?;
                let mut answer = String::new();
                io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    println!("left alias '{name}' unchanged");
                    return Ok(());
                }
            }
            let replaced = add_or_update_alias(&rc_path, shell, &name, &command)?;
            if replaced {
                println!("updated alias '{name}' in {}", rc_path.display());
            } else {
                println!("added alias '{name}' to {}", rc_path.display());
            }
            println!(
                "Run `source {}` or open a new terminal to reload aliases.",
                rc_path.display()
            );
        }
        AliasCommand::List => {
            let aliases = load_aliases(&rc_path)?;
            if aliases.is_empty() {
                println!("No aliases found in {}", rc_path.display());
            } else {
                for (name, command) in aliases {
                    println!("{name} -> {command}");
                }
            }
        }
        AliasCommand::Remove { name } => {
            if remove_alias(&rc_path, &name)? {
                println!("removed alias '{name}' from {}", rc_path.display());
                println!(
                    "Run `source {}` or open a new terminal to reload aliases.",
                    rc_path.display()
                );
            } else {
                return Err(anyhow!(
                    "Alias '{name}' was not found in {}",
                    rc_path.display()
                ));
            }
        }
    }

    Ok(())
}

pub fn install_wrapper() -> Result<()> {
    let shell = detect_shell();
    let home = shell::home_dir()?;
    let rc_path = shell.rc_path(&home);
    let snippet = shell::shell_wrapper_snippet(shell, "qr");
    if ensure_wrapper_present(&rc_path, &snippet)? {
        println!("added qr shell wrapper to {}", rc_path.display());
    } else {
        println!("qr shell wrapper already present in {}", rc_path.display());
    }
    Ok(())
}
