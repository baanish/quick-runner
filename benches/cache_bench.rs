mod common;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use quick_runner::scanner::{read_project_cache, write_project_cache};

fn cache_benchmarks(c: &mut Criterion) {
    let mut write_group = c.benchmark_group("cache_write");
    for project_count in [100usize, 1_000, 5_000] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("projects-cache.json");
        let cache = common::sample_cache(project_count);
        write_group.bench_with_input(
            BenchmarkId::from_parameter(project_count),
            &(path, cache),
            |b, (path, cache)| b.iter(|| write_project_cache(path, cache).unwrap()),
        );
    }
    write_group.finish();

    let mut read_group = c.benchmark_group("cache_read");
    for project_count in [100usize, 1_000, 5_000] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("projects-cache.json");
        let cache = common::sample_cache(project_count);
        write_project_cache(&path, &cache).unwrap();
        read_group.bench_with_input(
            BenchmarkId::from_parameter(project_count),
            &path,
            |b, path| b.iter(|| read_project_cache(path).unwrap()),
        );
    }
    read_group.finish();
}

criterion_group!(benches, cache_benchmarks);
criterion_main!(benches);
