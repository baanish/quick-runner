use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn help_lists_core_commands() {
    let mut cmd = Command::cargo_bin("qr").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(contains("go"))
        .stdout(contains("run"))
        .stdout(contains("alias"))
        .stdout(contains("stats"))
        .stdout(contains("scan"))
        .stdout(contains("init"));
}
