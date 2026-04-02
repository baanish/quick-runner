use std::{
    env,
    io::{self, Write},
    process::{Command, ExitCode},
    time::Instant,
};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use quick_runner::{
    ai, commands,
    commands::{alias::AliasCommand, go::GoResult},
    config::{AppConfig, config_file_path, write_default_config_if_missing},
    scanner, shell,
    stats_db::{CommandStats, StatsDb},
};

#[derive(Parser)]
#[command(name = "qr", version, about = "QuickRunner")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(alias = "g")]
    Go(GoArgs),
    #[command(alias = "r")]
    Run(RunArgs),
    #[command(alias = "a")]
    Alias(AliasArgs),
    #[command(alias = "s")]
    Stats,
    #[command(alias = "x")]
    Scan,
    #[command(alias = "i")]
    Init(InitArgs),
    #[command(alias = "d", hide = true)]
    Do {
        #[arg(required = true)]
        task: Vec<String>,
    },
}

#[derive(Args)]
struct GoArgs {
    project: String,
    #[arg(long, hide = true)]
    print_path: bool,
}

#[derive(Args)]
struct RunArgs {
    #[arg(required = true)]
    parts: Vec<String>,
}

#[derive(Args)]
struct AliasArgs {
    #[command(subcommand)]
    command: AliasSubcommand,
}

#[derive(Subcommand)]
enum AliasSubcommand {
    Add {
        name: String,
        #[arg(required = true)]
        command: Vec<String>,
    },
    List,
    Remove {
        name: String,
    },
}

#[derive(Args)]
struct InitArgs {
    #[arg(long)]
    no_shell_wrapper: bool,
    #[arg(long)]
    no_cron: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let start = Instant::now();
    let config = AppConfig::load()?;
    config.ensure_parent_dirs()?;

    let mut stats = CommandStats {
        command_type: command_name(&cli.command).to_string(),
        provider: "no AI".into(),
        ..CommandStats::default()
    };

    let execution = match cli.command {
        Commands::Go(args) => {
            let result = commands::go::execute(&config, &args.project)?;
            print_go_result(&result, args.print_path)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Run(args) => {
            let (mode, script) = commands::run::parse_args(&config, &args.parts)?;
            let result = commands::run::execute(mode, &script)?;
            if let Some(path) = result.log_path {
                let _ = path;
            }
            if result.exit_code == 0 {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(result.exit_code as u8))
            }
        }
        Commands::Alias(args) => {
            let alias_command = match args.command {
                AliasSubcommand::Add { name, command } => AliasCommand::Add {
                    name,
                    command: command.join(" "),
                },
                AliasSubcommand::List => AliasCommand::List,
                AliasSubcommand::Remove { name } => AliasCommand::Remove { name },
            };
            commands::alias::execute(alias_command)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Stats => {
            commands::stats::display(&config.stats_db_path())?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Scan => {
            let cache = scanner::scan_projects(&config)?;
            println!(
                "scanned {} projects into {}",
                cache.projects.len(),
                config.cache_path().display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Commands::Init(args) => {
            execute_init(&config, args)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Do { task } => {
            let client = ai::client::AiClient::new(
                config.ai_primary_provider(),
                config.ai_fallback_provider(),
            );
            let prompt = task.join(" ");
            let _ = prompt;
            let _ = client.primary_provider_label();
            Err(anyhow!(
                "`qr do` is reserved for v2; architecture is present in v1"
            ))
        }
    };

    stats.latency_ms = start.elapsed().as_millis();
    if config.stats.enabled {
        let db = StatsDb::open(&config.stats_db_path())?;
        db.record(&stats)?;
    }
    print_stats_line(&stats)?;
    execution
}

fn execute_init(config: &AppConfig, args: InitArgs) -> Result<()> {
    let config_path = config_file_path();
    let is_new = !config_path.exists();

    if is_new {
        // Ask for project roots interactively
        println!("Welcome to QuickRunner! Let's set up your project roots.");
        println!("Enter directories to scan for projects (one per line, empty line to finish):");
        println!("  Example: ~/Development");
        println!("  Example: /Volumes/Delos/Development");
        println!();

        let mut roots: Vec<String> = Vec::new();
        loop {
            print!("  root {}: ", roots.len() + 1);
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                if roots.is_empty() {
                    println!("  (defaulting to ~/Development)");
                    roots.push("~/Development".into());
                }
                break;
            }
            roots.push(trimmed.to_string());
        }

        // Build config with user-provided roots
        let roots_toml: Vec<String> = roots.iter().map(|r| format!("\"{r}\"")).collect();
        let config_content = format!(
            r#"[general]
default_run_mode = "output"

[projects]
roots = [{roots}]
scan_depth = 2
scan_interval_hours = 1

[ai]
protocol = "openai"
base_url = "https://api.fireworks.ai/inference/v1"
model = "accounts/fireworks/models/llama-v3p1-70b-instruct"
api_key_env = "QR_API_KEY"

[stats]
enabled = true
db_path = "~/.config/qr/stats.db"
"#,
            roots = roots_toml.join(", ")
        );

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&config_path, &config_content)?;
        println!("created {}", config_path.display());
    } else {
        println!("config already present at {}", config_path.display());
    }

    // Reload config in case we just wrote a new one with custom roots
    let effective_config = if is_new {
        AppConfig::load()?
    } else {
        config.clone()
    };

    if !args.no_shell_wrapper {
        commands::alias::install_wrapper()?;
    }

    if !args.no_cron {
        install_cron()?;
    }

    let cache = scanner::scan_projects(&effective_config)?;
    println!("initial scan found {} projects", cache.projects.len());
    Ok(())
}

fn install_cron() -> Result<()> {
    let exe = env::current_exe().context("Could not resolve qr binary path")?;
    let cron_line = shell::cron_line(&exe);
    let current = Command::new("crontab").arg("-l").output();

    match current {
        Ok(output) => {
            let existing = String::from_utf8_lossy(&output.stdout);
            if existing.contains(&cron_line) {
                println!("cron entry already present");
                return Ok(());
            }

            let mut merged = existing.to_string();
            if !merged.ends_with('\n') && !merged.is_empty() {
                merged.push('\n');
            }
            merged.push_str(&cron_line);
            merged.push('\n');

            let mut child = Command::new("crontab")
                .arg("-")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("Failed to update crontab")?;
            child.stdin.take().unwrap().write_all(merged.as_bytes())?;
            let status = child.wait()?;
            if !status.success() {
                return Err(anyhow!("crontab rejected the new entry"));
            }
            println!("installed hourly scan cron");
        }
        Err(_) => {
            println!("Could not update crontab automatically. Add this entry manually:");
            println!("{}", cron_line);
        }
    }

    Ok(())
}

fn print_go_result(result: &GoResult, print_path: bool) -> Result<()> {
    if print_path {
        println!("{}", result.path);
    } else {
        println!("→ cd {}", result.path);
    }
    Ok(())
}

fn print_stats_line(stats: &CommandStats) -> Result<()> {
    let mut stderr = io::stderr().lock();
    if stats.ai_used {
        writeln!(
            stderr,
            "⚡ {}ms | {} tok (in: {} / out: {}) | {} | ~${:.3}",
            stats.latency_ms,
            stats.input_tokens + stats.output_tokens,
            stats.input_tokens,
            stats.output_tokens,
            stats.provider,
            stats.estimated_cost_usd
        )?;
    } else {
        writeln!(stderr, "⚡ {}ms | no AI", stats.latency_ms)?;
    }
    Ok(())
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Go(_) => "go",
        Commands::Run(_) => "run",
        Commands::Alias(_) => "alias",
        Commands::Stats => "stats",
        Commands::Scan => "scan",
        Commands::Init(_) => "init",
        Commands::Do { .. } => "do",
    }
}
