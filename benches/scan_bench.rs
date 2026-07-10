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
        {
            let _env = common::scoped_env_var("QR_CONFIG_DIR", fixture.config_dir.as_os_str());
            group.bench_with_input(BenchmarkId::new(label, projects), &fixture, |b, fixture| {
                b.iter(|| scan_projects(&fixture.config).unwrap());
            });
        }
    }

    let hidden = common::hidden_tree_scan_fixture(1_000);
    {
        let _env = common::scoped_env_var("QR_CONFIG_DIR", hidden.config_dir.as_os_str());
        group.bench_function("prune_hidden_tree/1000", |b| {
            b.iter(|| scan_projects(&hidden.config).unwrap());
        });
    }

    let node_modules = common::node_modules_scan_fixture(1_000);
    {
        let _env = common::scoped_env_var("QR_CONFIG_DIR", node_modules.config_dir.as_os_str());
        group.bench_function("prune_node_modules/1000", |b| {
            b.iter(|| scan_projects(&node_modules.config).unwrap());
        });
    }

    group.finish();
}

criterion_group!(benches, scan_benchmarks);
criterion_main!(benches);
