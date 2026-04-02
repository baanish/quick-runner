use criterion::{Criterion, criterion_group, criterion_main};
use quick_runner::shell::{ShellKind, detect_shell_from, shell_wrapper_snippet};

fn shell_benchmarks(c: &mut Criterion) {
    c.bench_function("shell_detect_zsh", |b| {
        b.iter(|| detect_shell_from(Some("/bin/zsh")));
    });
    c.bench_function("shell_detect_fish", |b| {
        b.iter(|| detect_shell_from(Some("/opt/homebrew/bin/fish")));
    });
    c.bench_function("shell_wrapper_zsh", |b| {
        b.iter(|| shell_wrapper_snippet(ShellKind::Zsh, "qr"));
    });
    c.bench_function("shell_wrapper_fish", |b| {
        b.iter(|| shell_wrapper_snippet(ShellKind::Fish, "qr"));
    });
}

criterion_group!(benches, shell_benchmarks);
criterion_main!(benches);
