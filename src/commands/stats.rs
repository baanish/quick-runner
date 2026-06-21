use std::path::Path;

use anyhow::Result;

use crate::stats_db::{StatsDb, StatsSummary};

pub fn display(path: &Path, stats_enabled: bool) -> Result<StatsSummary> {
    let db = StatsDb::open(path)?;
    let summary = db.summary()?;

    println!("QuickRunner Stats");
    println!("─────────────────");
    println!("Total runs:        {}", summary.total_runs);
    println!("AI-assisted runs:  {}", summary.ai_assisted_runs);
    println!(
        "Tokens used:       {} (in: {} / out: {})",
        summary.input_tokens + summary.output_tokens,
        summary.input_tokens,
        summary.output_tokens
    );
    println!(
        "Total AI time:     {:.1}s (avg: {}ms)",
        summary.total_ai_latency_ms as f64 / 1000.0,
        summary.average_ai_latency_ms
    );
    println!("Est. cost:         ${:.3}", summary.estimated_cost_usd);
    println!("Provider:          {}", summary.last_provider);
    println!("─────────────────");
    if stats_enabled {
        println!(
            "ℹ Recording stats writes to SQLite on every command, which adds a few ms. Set stats.enabled = false to disable."
        );
    } else {
        println!("ℹ Stats tracking is disabled for non-AI commands. Run qr config to enable.");
    }

    Ok(summary)
}
