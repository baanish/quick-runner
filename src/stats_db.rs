use std::{fs, path::Path};

use anyhow::Result;
use rusqlite::{Connection, params};

#[derive(Debug, Clone, Default)]
pub struct CommandStats {
    pub command_type: String,
    pub ai_used: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency_ms: u128,
    pub provider: String,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatsSummary {
    pub total_runs: u64,
    pub ai_assisted_runs: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_ai_latency_ms: u64,
    pub average_ai_latency_ms: u64,
    pub estimated_cost_usd: f64,
    pub last_provider: String,
}

pub struct StatsDb {
    connection: Connection,
}

impl StatsDb {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        let db = Self { connection };
        db.ensure_schema()?;
        Ok(db)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        let db = Self { connection };
        db.ensure_schema()?;
        Ok(db)
    }

    pub fn record(&self, stats: &CommandStats) -> Result<()> {
        self.connection.execute(
            r#"
            INSERT INTO command_runs
            (command_type, ai_used, input_tokens, output_tokens, latency_ms, provider, estimated_cost_usd)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                stats.command_type,
                stats.ai_used as i64,
                stats.input_tokens as i64,
                stats.output_tokens as i64,
                stats.latency_ms as i64,
                stats.provider,
                stats.estimated_cost_usd
            ],
        )?;
        Ok(())
    }

    pub fn summary(&self) -> Result<StatsSummary> {
        let mut stmt = self.connection.prepare(
            r#"
            SELECT
                COUNT(*),
                COALESCE(SUM(CASE WHEN ai_used = 1 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(CASE WHEN ai_used = 1 THEN latency_ms ELSE 0 END), 0),
                COALESCE(AVG(CASE WHEN ai_used = 1 THEN latency_ms ELSE NULL END), 0.0),
                COALESCE(SUM(estimated_cost_usd), 0.0),
                COALESCE(
                    (SELECT provider FROM command_runs WHERE ai_used = 1 ORDER BY id DESC LIMIT 1),
                    'no AI'
                )
            FROM command_runs
            "#,
        )?;
        let summary = stmt.query_row([], |row| {
            Ok(StatsSummary {
                total_runs: row.get::<_, i64>(0)? as u64,
                ai_assisted_runs: row.get::<_, i64>(1)? as u64,
                input_tokens: row.get::<_, i64>(2)? as u64,
                output_tokens: row.get::<_, i64>(3)? as u64,
                total_ai_latency_ms: row.get::<_, i64>(4)? as u64,
                average_ai_latency_ms: row.get::<_, f64>(5)? as u64,
                estimated_cost_usd: row.get(6)?,
                last_provider: row.get(7)?,
            })
        })?;
        Ok(summary)
    }

    fn ensure_schema(&self) -> Result<()> {
        self.connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS command_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                command_type TEXT NOT NULL,
                ai_used INTEGER NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                latency_ms INTEGER NOT NULL,
                provider TEXT NOT NULL,
                estimated_cost_usd REAL NOT NULL
            );
            "#,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_summary_aggregates_runs() {
        let db = StatsDb::open_in_memory().unwrap();
        db.record(&CommandStats {
            command_type: "go".into(),
            ai_used: false,
            latency_ms: 12,
            provider: "no AI".into(),
            ..CommandStats::default()
        })
        .unwrap();
        db.record(&CommandStats {
            command_type: "do".into(),
            ai_used: true,
            input_tokens: 800,
            output_tokens: 400,
            latency_ms: 342,
            provider: "FirePass".into(),
            estimated_cost_usd: 0.001,
        })
        .unwrap();

        let summary = db.summary().unwrap();
        assert_eq!(summary.total_runs, 2);
        assert_eq!(summary.ai_assisted_runs, 1);
        assert_eq!(summary.input_tokens, 800);
        assert_eq!(summary.output_tokens, 400);
        assert_eq!(summary.average_ai_latency_ms, 342);
        assert_eq!(summary.last_provider, "FirePass");
    }
}
