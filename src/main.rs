use std::{
    env,
    io::{self, IsTerminal, Write},
    process::{Command, ExitCode},
    time::Instant,
};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use quick_runner::{
    ai,
    ai::providers::AiProtocol,
    atomic, commands,
    commands::{alias::AliasCommand, config_cmd::ConfigArgs, go::GoResult},
    config::{
        AgentConfig, AiConfig, AppConfig, DoConfig, FallbackAiConfig, GeneralConfig,
        ProjectsConfig, StatsConfig, config_dir, config_file_path,
    },
    pricing, scanner, secret, shell,
    stats_db::{CommandStats, StatsDb},
    terminal,
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
    #[command(alias = "c")]
    Config(ConfigArgs),
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
    Doctor,
    /// Show or refresh the AI token prices used for cost estimates
    Cost {
        /// Re-fetch the price table from models.dev
        #[arg(long)]
        refresh: bool,
    },
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
    /// Run silently and report only pass/fail
    #[arg(long, group = "run_mode")]
    watch: bool,
    /// Write output to a timestamped log file
    #[arg(long, group = "run_mode")]
    log: bool,
    /// Stream output to the terminal (default)
    #[arg(long, group = "run_mode")]
    output: bool,
    #[arg(required = true, trailing_var_arg = true)]
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
    /// Skip fetching the models.dev price snapshot used for cost estimates
    #[arg(long)]
    no_prices: bool,
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

    // Config-independent commands must work even when config.toml is missing or
    // unparseable — they exist to inspect, edit, or repair it. Dispatch them
    // before AppConfig::load() so a broken config cannot brick its own recovery.
    match cli.command {
        Commands::Config(args) => {
            commands::config_cmd::execute(args)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Doctor => commands::doctor::run().map(|()| ExitCode::SUCCESS),
        Commands::Learn => {
            // learn profiles the current project and writes to cwd/.qr; it does
            // not use qr's config, so it must not fail when config.toml is broken.
            let result = commands::learn::execute()?;
            commands::learn::print_summary(&result);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Init(args) => {
            execute_init(args)?;
            Ok(ExitCode::SUCCESS)
        }
        command => run_with_config(command),
    }
}

/// Map a child process exit code to this process's `ExitCode` (0 → success).
fn exit_status(code: i32) -> ExitCode {
    if code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(code as u8)
    }
}

fn run_with_config(command: Commands) -> Result<ExitCode> {
    let start = Instant::now();
    let config = AppConfig::load()
        .context("Run `qr doctor` to diagnose, or `qr config` to edit the config file")?;
    config.ensure_parent_dirs()?;

    let mut stats = CommandStats {
        command_type: command_name(&command).to_string(),
        provider: "no AI".into(),
        ..CommandStats::default()
    };
    let mut interactive_ms: u128 = 0;

    let execution = match command {
        Commands::Go(args) => {
            let query = args.project.join("-");
            let result = if query.is_empty() {
                commands::go::execute_live(&config)?
            } else {
                commands::go::execute(&config, &query)?
            };
            interactive_ms = result.interactive_ms;
            print_go_result(&result, args.print_path)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Config(_) | Commands::Doctor | Commands::Learn => {
            unreachable!("config-independent commands are dispatched before config load")
        }
        Commands::Run(args) => {
            let mode = commands::run::resolve_mode(&config, args.watch, args.log, args.output)?;
            let script = args.parts.join(" ");
            let result = commands::run::execute(mode, &script)?;
            if let Some(path) = result.log_path {
                let _ = path;
            }
            Ok(exit_status(result.exit_code))
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
            commands::stats::display(&config.stats_db_path(), config.stats.enabled)?;
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
        Commands::Init(_) => {
            unreachable!("config-independent commands are dispatched before config load")
        }
        Commands::Cost { refresh } => {
            run_cost(&config, refresh)?;
            stats.command_type = "__skip_stats__".into();
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
            // Estimate cost from the configured price override, else the
            // models.dev snapshot. Unknown stays 0.0 and is shown as `cost n/a`.
            if let Some(price) = config.ai.cost.or_else(|| {
                pricing::load(&config_dir()).and_then(|table| table.get(&config.ai.model))
            }) {
                stats.estimated_cost_usd = price.cost(
                    result.ai_response.input_tokens,
                    result.ai_response.output_tokens,
                );
                stats.cost_known = true;
            }

            match result.outcome {
                commands::do_cmd::DoOutcome::Inline { exit_code, .. } => Ok(exit_status(exit_code)),
                commands::do_cmd::DoOutcome::Delegate { .. } => Ok(ExitCode::SUCCESS),
            }
        }
    };

    stats.latency_ms = start.elapsed().as_millis().saturating_sub(interactive_ms);
    if stats.command_type != "__skip_stats__" {
        if config.stats.enabled || stats.ai_used {
            // Best-effort: a stats-DB failure (SQLITE_BUSY from a concurrent qr,
            // a read-only or full disk) must never turn a command that already
            // succeeded into a failure. Warn and carry on.
            if let Err(error) = record_stats(&config, &stats) {
                eprintln!("warning: could not record stats: {error:#}");
            }
        }
        print_stats_line(&stats, config.stats.enabled)?;
    }
    execution
}

fn record_stats(config: &AppConfig, stats: &CommandStats) -> Result<()> {
    let db = StatsDb::open_for_telemetry(&config.stats_db_path())?;
    db.record(stats)
}

fn execute_init(args: InitArgs) -> Result<()> {
    let config_path = config_file_path();
    let existing = if config_path.exists() {
        Some(AppConfig::load_file_without_env(&config_path))
    } else {
        None
    };
    let needs_recreate = !config_path.exists() || existing.as_ref().is_some_and(Result::is_err);

    let effective_config = if needs_recreate {
        if let Some(Err(error)) = existing {
            println!(
                "config at {} is invalid; recreating it ({error:#})",
                config_path.display()
            );
        }
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
                agents: AgentConfig::default(),
            },
        })?;

        atomic::write_private(&config_path, config_content.as_bytes())?;
        println!("created {}", config_path.display());
        AppConfig::load_from_env_with_path(config_path.clone())?
    } else {
        println!("config already present at {}", config_path.display());
        AppConfig::load_from_env_with_path(config_path.clone())?
    };

    if !args.no_shell_wrapper {
        commands::alias::install_wrapper()?;
    }

    // `--no-cron` forces a skip without prompting, keeping non-interactive
    // `qr init` scriptable. Otherwise ask, defaulting to no since installing the
    // cron modifies the user's crontab.
    if !args.no_cron && prompt_bool("Install hourly project rescan cron?", false)? {
        install_cron()?;
    }

    let cache = scanner::scan_projects(&effective_config)?;
    println!("initial scan found {} projects", cache.projects.len());

    if !args.no_prices {
        // Best-effort: a network failure here must not block init.
        match pricing::refresh(&config_dir()) {
            Ok(count) => println!("fetched prices for {count} models from models.dev"),
            Err(error) => {
                println!("⚠ could not fetch model prices ({error}); run `qr cost --refresh` later")
            }
        }
    }

    Ok(())
}

/// Prompt for the five fields shared by a primary and a fallback AI provider.
fn prompt_provider_fields(label: &str) -> Result<(AiProtocol, String, String, String, String)> {
    let protocol = prompt_protocol(label)?;
    let base_url = prompt_with_default(&format!("{label} base URL"), protocol.default_base_url())?;
    let model = prompt_required(&format!("{label} model name"))?;
    let api_key = prompt_required_for_provider_field(&format!("{label} API key"))?;
    let api_key_env = prompt_with_default(
        &format!("{label} API key env var override"),
        protocol.default_api_key_env(),
    )?;
    Ok((protocol, base_url, model, api_key, api_key_env))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptSensitivity {
    Plain,
    Secret,
}

fn provider_field_prompt_sensitivity(label: &str) -> PromptSensitivity {
    if label.trim_end().ends_with("API key") {
        PromptSensitivity::Secret
    } else {
        PromptSensitivity::Plain
    }
}

fn prompt_required_for_provider_field(label: &str) -> Result<String> {
    match provider_field_prompt_sensitivity(label) {
        PromptSensitivity::Plain => prompt_required(label),
        PromptSensitivity::Secret => prompt_secret_required(label),
    }
}

fn prompt_ai_config(label: &str) -> Result<AiConfig> {
    let (protocol, base_url, model, api_key, api_key_env) = prompt_provider_fields(label)?;
    let api_key = maybe_store_key_in_keychain("primary", protocol, &api_key_env, api_key)?;

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
        cost: None,
    })
}

fn run_cost(config: &AppConfig, refresh: bool) -> Result<()> {
    let dir = config_dir();
    if refresh {
        let count = pricing::refresh(&dir)?;
        println!("✔ saved prices for {count} models from models.dev");
    }
    let model = &config.ai.model;
    let price = config
        .ai
        .cost
        .or_else(|| pricing::load(&dir).and_then(|table| table.get(model)));
    match price {
        Some(price) => {
            let source = if config.ai.cost.is_some() {
                "config override"
            } else {
                "models.dev"
            };
            println!(
                "{model}: ${:.2} per Mtok in, ${:.2} per Mtok out  ({source})",
                price.input, price.output
            );
        }
        None => println!(
            "{model}: no price found — run `qr cost --refresh`, or set [ai].cost in config.toml"
        ),
    }
    Ok(())
}

/// Offer to store the API key in the OS keychain instead of the config file.
/// Returns the value to write into config: empty when the key was stored in the
/// keychain, otherwise the key itself (config storage is the fallback when the
/// user declines or the keychain is unavailable).
fn maybe_store_key_in_keychain(
    role: &str,
    protocol: AiProtocol,
    api_key_env: &str,
    api_key: String,
) -> Result<String> {
    if !prompt_bool(
        "Store the API key in your OS keychain (recommended; keeps it out of config.toml)?",
        true,
    )? {
        println!("API key will be stored in config.toml");
        return Ok(api_key);
    }
    let account = secret::account_for(role, api_key_env, protocol.default_api_key_env());
    match secret::set(&account, &api_key) {
        Ok(()) => {
            println!("✔ stored API key in the OS keychain (account \"{account}\")");
            Ok(String::new())
        }
        Err(error) => {
            println!(
                "⚠ could not use the keychain ({error}); storing the key in config.toml instead"
            );
            println!("API key will be stored in config.toml");
            Ok(api_key)
        }
    }
}

fn prompt_fallback_ai_config() -> Result<FallbackAiConfig> {
    let (protocol, base_url, model, api_key, api_key_env) = prompt_provider_fields("fallback")?;
    let api_key = maybe_store_key_in_keychain("fallback", protocol, &api_key_env, api_key)?;
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
        match prompt(label)? {
            Some(value) if !value.trim().is_empty() => return Ok(value),
            // A closed/empty stdin can never satisfy a required field, so error
            // out instead of spinning the retry loop forever on an empty read.
            None => {
                anyhow::bail!("Unexpected end of input while reading '{label}' (is stdin closed?)")
            }
            Some(_) => println!("{label} is required."),
        }
    }
}

fn prompt_secret_required(label: &str) -> Result<String> {
    loop {
        match prompt_secret(label)? {
            Some(value) if !value.trim().is_empty() => return Ok(value),
            None => {
                anyhow::bail!("Unexpected end of input while reading '{label}' (is stdin closed?)")
            }
            Some(_) => println!("{label} is required."),
        }
    }
}

fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    // A blank line or EOF both fall back to the default.
    let value = prompt(&format!("{label} [{default}]"))?.unwrap_or_default();
    if value.trim().is_empty() {
        Ok(default.to_string())
    } else {
        Ok(value)
    }
}

fn prompt_bool(label: &str, default: bool) -> Result<bool> {
    let default_label = if default { "y" } else { "n" };

    loop {
        // A blank line or EOF both accept the default.
        let value = prompt(&format!("{label} (y/n) [{default_label}]"))?.unwrap_or_default();
        match value.trim().to_ascii_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Please enter y or n."),
        }
    }
}

/// Read one line from stdin. Returns `None` on EOF (a closed/empty stdin) so
/// callers can distinguish "the user pressed Enter" (Some("")) from "there is no
/// more input" — the latter must stop required-field retry loops rather than
/// letting them spin forever.
fn prompt(label: &str) -> Result<Option<String>> {
    print!("{label}: ");
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

fn prompt_secret(label: &str) -> Result<Option<String>> {
    if !io::stdin().is_terminal() {
        return prompt(label);
    }
    let value = rpassword::prompt_password(format!("{label}: "))?;
    Ok(Some(value.trim().to_string()))
}

fn install_cron() -> Result<()> {
    let exe = env::current_exe().context("Could not resolve qr binary path")?;
    let cron_line = shell::cron_line(&exe);
    let output = match Command::new("crontab").arg("-l").output() {
        Ok(output) => output,
        // Cannot spawn `crontab` (binary missing on minimal images, etc.) — nothing
        // to overwrite, so fall back to printing the line for manual install.
        Err(error) => {
            print_manual_cron_hint(&cron_line);
            eprintln!("warning: could not run `crontab -l`: {error:#}");
            return Ok(());
        }
    };
    let existing = shell::crontab_contents_from_list_output(&output)?;

    let Some(merged) = shell::merge_cron_entry(&existing, &cron_line) else {
        println!("cron entry already present");
        return Ok(());
    };

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

    Ok(())
}

fn print_manual_cron_hint(cron_line: &str) {
    println!("Could not update crontab automatically. Add this entry manually:");
    println!("{cron_line}");
}

fn print_go_result(result: &GoResult, print_path: bool) -> Result<()> {
    if print_path {
        println!("{}", result.path);
    } else {
        println!("→ cd {}", terminal::escape_untrusted(&result.path));
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
        let cost = if stats.cost_known {
            format!("~${:.4}", stats.estimated_cost_usd)
        } else {
            "cost n/a".to_string()
        };
        writeln!(
            stderr,
            "⚡ {}ms | {} tok (in: {} / out: {}) | {} | {}",
            stats.latency_ms,
            stats.input_tokens + stats.output_tokens,
            stats.input_tokens,
            stats.output_tokens,
            stats.provider,
            cost
        )?;
    } else {
        writeln!(stderr, "⚡ {}ms | no AI", stats.latency_ms)?;
    }
    Ok(())
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Go(_) => "go",
        Commands::Config(_) => "config",
        Commands::Run(_) => "run",
        Commands::Alias(_) => "alias",
        Commands::Stats => "stats",
        Commands::Scan => "scan",
        Commands::Init(_) => "init",
        Commands::Learn => "learn",
        Commands::Doctor => "doctor",
        Commands::Cost { .. } => "cost",
        Commands::Do { .. } => "do",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_provider_field_uses_secret_prompting() {
        assert_eq!(
            provider_field_prompt_sensitivity("primary API key"),
            PromptSensitivity::Secret
        );
        assert_eq!(
            provider_field_prompt_sensitivity("fallback API key"),
            PromptSensitivity::Secret
        );
        assert_eq!(
            provider_field_prompt_sensitivity("primary model name"),
            PromptSensitivity::Plain
        );
    }
}
