mod common;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use quick_runner::scanner::scan_projects;

fn scan_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("scan_projects");

    for (label, projects, nested_dirs) in [
        ("small", 25usize, 1usize),
        ("medium", 100, 2),
        ("large", 300, 3),
    ] {
        let fixture = common::scan_fixture(projects, nested_dirs);
        group.bench_with_input(BenchmarkId::new(label, projects), &fixture, |b, fixture| {
            b.iter(|| scan_projects(&fixture.config).unwrap());
        });
    }

    group.finish();
}

criterion_group!(benches, scan_benchmarks);
criterion_main!(benches);
