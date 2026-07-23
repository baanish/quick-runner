use std::{fs, path::Path, time::Duration};

use anyhow::Result;
use rusqlite::{Connection, params};

const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const TELEMETRY_BUSY_TIMEOUT: Duration = Duration::from_millis(5);

#[derive(Debug, Clone, Default)]
pub struct CommandStats {
    pub command_type: String,
    pub ai_used: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency_ms: u128,
    pub provider: String,
    pub estimated_cost_usd: f64,
    /// Whether a price was resolved (vs. unknown). Transient — not persisted; it
    /// only controls whether the stats line shows a dollar figure or `cost n/a`.
    pub cost_known: bool,
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
        Self::open_with_busy_timeout(path, DEFAULT_BUSY_TIMEOUT)
    }

    pub fn open_for_telemetry(path: &Path) -> Result<Self> {
        Self::open_with_busy_timeout(path, TELEMETRY_BUSY_TIMEOUT)
    }

    fn open_with_busy_timeout(path: &Path, busy_timeout: Duration) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        // Wait for a concurrent writer rather than failing immediately with
        // "database is locked" — several qr invocations can record at once.
        connection.busy_timeout(busy_timeout)?;
        // WAL lets a reader (qr stats) run alongside a writer. Best-effort: it is
        // unsupported on some filesystems, and stats are non-critical.
        let _ = connection.pragma_update(None, "journal_mode", "WAL");
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
                saturating_i64(stats.input_tokens as u128),
                saturating_i64(stats.output_tokens as u128),
                saturating_i64(stats.latency_ms),
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
                -- `total()` sums as a float, so saturated i64::MAX rows can't
                -- overflow the way integer `SUM()` does (which raises on overflow).
                total(input_tokens),
                total(output_tokens),
                total(CASE WHEN ai_used = 1 THEN latency_ms ELSE 0 END),
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
                input_tokens: saturating_u64_from_f64(row.get::<_, f64>(2)?),
                output_tokens: saturating_u64_from_f64(row.get::<_, f64>(3)?),
                total_ai_latency_ms: saturating_u64_from_f64(row.get::<_, f64>(4)?),
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

/// Clamp a count to SQLite's signed 64-bit range. Token counts and latencies
/// this large are not physically reachable, but a bare `as i64` would wrap them
/// to a negative number and silently corrupt later `SUM`/`AVG` aggregates.
fn saturating_i64(value: u128) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Clamp a floating aggregate (from SQLite `total()`) back to `u64`. Normal sums
/// are well within f64's exact-integer range; only absurd saturated totals reach
/// the clamp.
fn saturating_u64_from_f64(value: f64) -> u64 {
    if value <= 0.0 {
        0
    } else if value >= u64::MAX as f64 {
        u64::MAX
    } else {
        value as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_record_fails_fast_when_another_writer_holds_the_database() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("stats.db");
        drop(StatsDb::open(&path).unwrap());

        let lock = Connection::open(&path).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();

        let started = std::time::Instant::now();
        let result =
            StatsDb::open_for_telemetry(&path).and_then(|db| db.record(&CommandStats::default()));

        assert!(result.is_err());
        assert!(started.elapsed() < std::time::Duration::from_millis(250));
    }

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
            cost_known: true,
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

    #[test]
    fn huge_counts_saturate_and_summary_does_not_overflow() {
        // A bare `as i64` turns u64::MAX into -1 on insert; and once saturated to
        // i64::MAX, integer SUM() of multiple such rows would raise SQLite's
        // "integer overflow". Record TWO oversized rows and require summary() to
        // succeed with a large, non-negative total rather than wrapping or erroring.
        let db = StatsDb::open_in_memory().unwrap();
        for _ in 0..2 {
            db.record(&CommandStats {
                command_type: "do".into(),
                ai_used: true,
                input_tokens: u64::MAX,
                output_tokens: u64::MAX,
                latency_ms: u128::MAX,
                provider: "x".into(),
                estimated_cost_usd: 0.0,
                cost_known: true,
            })
            .unwrap();
        }

        let summary = db.summary().unwrap();
        assert_eq!(summary.total_runs, 2);
        // Two i64::MAX rows -> well above i64::MAX, clamped into u64 without wrap.
        assert!(summary.input_tokens >= i64::MAX as u64);
        assert!(summary.output_tokens >= i64::MAX as u64);
        assert!(summary.total_ai_latency_ms >= i64::MAX as u64);
    }
}
