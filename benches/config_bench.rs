mod common;

use criterion::{Criterion, criterion_group, criterion_main};
use quick_runner::config::{AppConfig, default_config_str};

fn config_benchmarks(c: &mut Criterion) {
    c.bench_function("config_parse_default_toml", |b| {
        b.iter(|| AppConfig::load_from_str(default_config_str()).unwrap());
    });

    let (_tmp, path) = common::config_file_with_contents(default_config_str());
    c.bench_function("config_load_from_path", |b| {
        b.iter(|| AppConfig::load_from_env_with_path(path.clone()).unwrap());
    });

    let _guard = common::env_lock().lock().unwrap();
    unsafe {
        std::env::set_var("QR_SCAN_DEPTH", "7");
        std::env::set_var("QR_PROJECT_ROOTS", "~/dev:/tmp/projects");
    }
    c.bench_function("config_load_with_env_overrides", |b| {
        b.iter(|| AppConfig::load_from_env_with_path(path.clone()).unwrap());
    });
    unsafe {
        std::env::remove_var("QR_SCAN_DEPTH");
        std::env::remove_var("QR_PROJECT_ROOTS");
    }
}

criterion_group!(benches, config_benchmarks);
criterion_main!(benches);
