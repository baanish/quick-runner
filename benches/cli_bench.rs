mod common;

use std::{
    path::Path,
    process::{Command, ExitStatus, Stdio},
};

use criterion::{Criterion, criterion_group, criterion_main};

fn run_qr(config_dir: &Path, args: &[&str]) -> ExitStatus {
    qr_command(config_dir, args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
}

fn qr_command(config_dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_qr"));
    command
        .args(args)
        .env_clear()
        .env("QR_CONFIG_DIR", config_dir)
        .env("QR_STATS_ENABLED", "false");
    command
}

fn cli_benchmarks(c: &mut Criterion) {
    let config_tmp = tempfile::tempdir().unwrap();
    c.bench_function("cli/config_path", |b| {
        b.iter(|| {
            assert!(run_qr(config_tmp.path(), &["config", "path"]).success());
        });
    });

    let exact = common::go_fixture(1_000);
    c.bench_function("cli/go_exact_1000", |b| {
        b.iter(|| {
            assert!(run_qr(&exact.config_dir, &["go", "service-0042", "--print-path"]).success());
        });
    });

    let miss = common::go_fixture(5_000);
    let miss_args = ["go", "zzzzzzzz", "--print-path"];
    let preflight = qr_command(&miss.config_dir, &miss_args).output().unwrap();
    assert_eq!(preflight.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&preflight.stderr).contains("No project matching 'zzzzzzzz' found")
    );
    c.bench_function("cli/go_no_match_5000", |b| {
        b.iter(|| {
            assert_eq!(run_qr(&miss.config_dir, &miss_args).code(), Some(1));
        });
    });

    #[cfg(unix)]
    c.bench_function("cli/run_true", |b| {
        b.iter(|| {
            assert!(run_qr(config_tmp.path(), &["run", "--output", "true"]).success());
        });
    });
}

criterion_group!(benches, cli_benchmarks);
criterion_main!(benches);
