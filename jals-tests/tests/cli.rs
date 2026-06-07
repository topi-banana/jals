//! Integration tests driving the built `jals-tests` binary against a temp corpus.
//! These do not depend on the git submodule, so they run in plain CI.

use std::fs;
use std::process::Command;

use tempfile::tempdir;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jals-tests"))
}

#[test]
fn reports_and_lists_failures_for_a_temp_corpus() {
    let dir = tempdir().unwrap();
    let openjdk = dir.path().join("openjdk");
    fs::create_dir_all(&openjdk).unwrap();
    fs::write(openjdk.join("Good.java"), "class Good {}").unwrap();
    fs::write(openjdk.join("Bad.java"), "class Bad {").unwrap();

    let out = bin()
        .arg("openjdk")
        .arg("--root")
        .arg(dir.path())
        .arg("--list-failures")
        .output()
        .unwrap();

    let stdout = String::from_utf8(out.stdout).unwrap();
    // The failing file is listed; the clean one is not.
    assert!(stdout.contains("Bad.java"), "stdout:\n{stdout}");
    assert!(!stdout.contains("Good.java"), "stdout:\n{stdout}");
    // Syntax errors are not invariant violations, so the run still succeeds.
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn missing_source_reports_a_clear_error() {
    let dir = tempdir().unwrap();
    let out = bin()
        .arg("openjdk")
        .arg("--root")
        .arg(dir.path())
        .output()
        .unwrap();

    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("not found"), "stderr:\n{stderr}");
    assert_eq!(out.status.code(), Some(2));
}
