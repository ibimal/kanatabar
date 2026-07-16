//! Smoke tests for the test double itself. Having an integration test also
//! guarantees cargo builds the mock-kanata *binary* during
//! `cargo test --workspace`, which kanatad's integration tests execute.

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mock-kanata"))
        .args(args)
        .output()
        .expect("run mock-kanata")
}

#[test]
fn help_works() {
    let out = run(&["--help"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("--check"));
}

#[test]
fn check_passes_by_default() {
    assert!(run(&["--cfg", "/tmp/whatever.kbd", "--check"])
        .status
        .success());
}

#[test]
fn check_fails_when_told_to() {
    let out = run(&["--check", "--mock-fail-check"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn timed_exit_uses_requested_code() {
    let out = run(&["--mock-exit-after-ms", "10", "--mock-exit-code", "7"]);
    assert_eq!(out.status.code(), Some(7));
}
