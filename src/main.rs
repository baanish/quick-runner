use std::{
    env, fs,
    io::{self, Write},
    process::{Command, ExitCode},
    time::Instant,
};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use quick_runner::{
    ai,
    ai::providers::AiProtocol,
    commands,
    commands::{alias::AliasCommand, go::GoResult},
    config::{
        AgentConfig, AiConfig, AppConfig, DoConfig, FallbackAiConfig, GeneralConfig,
        ProjectsConfig, StatsConfig, config_file_path,
    },
    scanner, shell,
    stats_db::{CommandStats, StatsDb},
};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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
    #[command(alias = "l")]
    Learn,
    #[command(alias = "d")]
    Do {
        #[arg(required = true)]
        task: Vec<String>,
    },
}

#[derive(Args)]
struct GoArgs {
    /// Project name (multiple words are joined with hyphens)
    project: Vec<String>,
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
    let mut interactive_ms: u128 = 0;

    let execution = match cli.command {
        Commands::Go(args) => {
            let query = args.project.join("-");
            if query.is_empty() {
                anyhow::bail!("project name required");
            }
            let result = commands::go::execute(&config, &query)?;
            interactive_ms = result.interactive_ms;
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
            stats.command_type = "__skip_stats__".into();
            Ok(ExitCode::SUCCESS)
        }
        Commands::Learn => {
            let result = commands::learn::execute()?;
            commands::learn::print_summary(&result);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Do { task } => {
            let client = ai::client::AiClient::new(
                config.ai_primary_provider(),
                config.ai_fallback_provider(),
            );
            let prompt = task.join(" ");
            let result = commands::do_cmd::execute(&config, &client, &prompt)?;
            stats.ai_used = true;
            stats.input_tokens = result.ai_response.input_tokens;
            stats.output_tokens = result.ai_response.output_tokens;
            stats.provider = result.ai_response.provider_label.clone();
            stats.estimated_cost_usd = result.ai_response.estimated_cost_usd;

            match result.outcome {
                commands::do_cmd::DoOutcome::Inline { exit_code, .. } => {
                    if exit_code == 0 {
                        Ok(ExitCode::SUCCESS)
                    } else {
                        Ok(ExitCode::from(exit_code as u8))
                    }
                }
                commands::do_cmd::DoOutcome::Delegate { .. } => Ok(ExitCode::SUCCESS),
            }
        }
    };

    stats.latency_ms = start.elapsed().as_millis().saturating_sub(interactive_ms);
    if stats.command_type != "__skip_stats__" {
        if config.stats.enabled {
            let db = StatsDb::open(&config.stats_db_path())?;
            db.record(&stats)?;
        }
        print_stats_line(&stats, config.stats.enabled)?;
    }
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

        println!();
        println!("Now let's configure your AI provider.");
        let ai_config = prompt_ai_config("primary")?;

        let config_content = toml::to_string_pretty(&AppConfig {
            general: GeneralConfig {
                default_run_mode: "output".into(),
            },
            projects: ProjectsConfig {
                roots,
                scan_depth: 2,
                scan_interval_hours: 1,
            },
            ai: ai_config,
            stats: StatsConfig {
                enabled: false,
                db_path: "__default__".into(),
            },
            do_config: DoConfig {
                auto_approve: DoConfig::default().auto_approve,
                agents: AgentConfig::default(),
            },
        })?;

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&config_path, &config_content)?;
        restrict_config_permissions(&config_path)?;
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

fn prompt_ai_config(label: &str) -> Result<AiConfig> {
    let protocol = prompt_protocol(label)?;
    let base_url = prompt_with_default(&format!("{label} base URL"), protocol.default_base_url())?;
    let model = prompt_required(&format!("{label} model name"))?;
    let api_key = prompt_required(&format!("{label} API key"))?;
    let api_key_env = prompt_with_default(
        &format!("{label} API key env var override"),
        protocol.default_api_key_env(),
    )?;

    let fallback = if prompt_bool("Configure a fallback provider?", false)? {
        Some(prompt_fallback_ai_config()?)
    } else {
        None
    };

    Ok(AiConfig {
        protocol,
        base_url,
        model,
        api_key,
        api_key_env,
        fallback,
    })
}

fn prompt_fallback_ai_config() -> Result<FallbackAiConfig> {
    let protocol = prompt_protocol("fallback")?;
    let base_url = prompt_with_default("fallback base URL", protocol.default_base_url())?;
    let model = prompt_required("fallback model name")?;
    let api_key = prompt_required("fallback API key")?;
    let api_key_env = prompt_with_default(
        "fallback API key env var override",
        protocol.default_api_key_env(),
    )?;

    Ok(FallbackAiConfig {
        protocol,
        base_url,
        model,
        api_key,
        api_key_env,
    })
}

fn prompt_protocol(label: &str) -> Result<AiProtocol> {
    loop {
        let raw = prompt_with_default(
            &format!("{label} protocol (openai-compatible or anthropic-compatible)"),
            "openai-compatible",
        )?;
        match normalize_protocol_input(&raw) {
            Ok(protocol) => return Ok(protocol),
            Err(error) => println!("{error}"),
        }
    }
}

fn normalize_protocol_input(raw: &str) -> Result<AiProtocol> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "openai" | "openai-compatible" => Ok(AiProtocol::OpenAi),
        "anthropic" | "anthropic-compatible" => Ok(AiProtocol::Anthropic),
        other => Err(anyhow!(
            "Unsupported protocol '{other}'. Use openai-compatible or anthropic-compatible."
        )),
    }
}

fn prompt_required(label: &str) -> Result<String> {
    loop {
        let value = prompt(label)?;
        if !value.trim().is_empty() {
            return Ok(value);
        }
        println!("{label} is required.");
    }
}

fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    let value = prompt(&format!("{label} [{default}]"))?;
    if value.trim().is_empty() {
        Ok(default.to_string())
    } else {
        Ok(value)
    }
}

fn prompt_bool(label: &str, default: bool) -> Result<bool> {
    let default_label = if default { "y" } else { "n" };

    loop {
        let value = prompt(&format!("{label} (y/n) [{default_label}]"))?;
        let trimmed = value.trim().to_ascii_lowercase();
        match trimmed.as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Please enter y or n."),
        }
    }
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn restrict_config_permissions(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)?;
    }

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

fn print_stats_line(stats: &CommandStats, stats_enabled: bool) -> Result<()> {
    // Always show stats for AI commands; otherwise only when stats are enabled
    if !stats.ai_used && !stats_enabled {
        return Ok(());
    }
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
        Commands::Learn => "learn",
        Commands::Do { .. } => "do",
    }
}
