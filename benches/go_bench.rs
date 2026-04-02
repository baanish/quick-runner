mod common;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use quick_runner::commands::go::{execute, rank_matches};

fn go_benchmarks(c: &mut Criterion) {
    let mut rank_group = c.benchmark_group("go_rank_matches");
    for project_count in [100usize, 1_000, 5_000] {
        let fixture = common::go_fixture(project_count);
        rank_group.bench_with_input(
            BenchmarkId::from_parameter(project_count),
            &fixture,
            |b, fixture| b.iter(|| rank_matches(&fixture.entries, "service-0499")),
        );
    }
    rank_group.finish();

    let mut execute_group = c.benchmark_group("go_execute");
    for (label, query) in [
        ("cache_hit", "service-0042"),
        ("cache_miss", "does-not-exist"),
    ] {
        let fixture = common::go_fixture(1_000);
        execute_group.bench_with_input(BenchmarkId::new(label, query), &fixture, |b, fixture| {
            b.iter(|| execute(&fixture.config, query).ok());
        });
    }
    execute_group.finish();
}

criterion_group!(benches, go_benchmarks);
criterion_main!(benches);
