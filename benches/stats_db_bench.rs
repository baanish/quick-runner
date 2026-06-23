mod common;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use quick_runner::stats_db::{CommandStats, StatsDb};

fn stats_db_benchmarks(c: &mut Criterion) {
    c.bench_function("stats_record_in_memory", |b| {
        b.iter(|| {
            let tmp = tempfile::tempdir().unwrap();
            let db = StatsDb::open(&tmp.path().join("stats.db")).unwrap();
            db.record(&CommandStats {
                command_type: "go".into(),
                ai_used: true,
                input_tokens: 100,
                output_tokens: 50,
                latency_ms: 12,
                provider: "FirePass".into(),
                estimated_cost_usd: 0.0005,
                cost_known: true,
            })
            .unwrap();
        });
    });

    let mut summary_group = c.benchmark_group("stats_summary");
    for run_count in [10usize, 100, 1_000] {
        let (_tmp, path) = common::seeded_stats_db(run_count);
        summary_group.bench_with_input(BenchmarkId::from_parameter(run_count), &path, |b, path| {
            b.iter(|| {
                let db = StatsDb::open(path).unwrap();
                db.summary().unwrap()
            })
        });
    }
    summary_group.finish();
}

criterion_group!(benches, stats_db_benchmarks);
criterion_main!(benches);
