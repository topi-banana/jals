//! Integration tests driving the built `jals` binary.

use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::tempdir;

fn jals() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jals"))
}

/// Run `jals fmt` over stdin and return (stdout, exit code).
fn run_stdin(args: &[&str], input: &str) -> (String, i32) {
    let mut child = jals()
        .arg("fmt")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        out.status.code().unwrap(),
    )
}

#[test]
fn stdin_to_stdout_formats() {
    let (stdout, code) = run_stdin(&[], "class C{int x=1;}");
    assert_eq!(code, 0);
    assert_eq!(stdout, "class C {\n    int x = 1;\n}\n");
}

#[test]
fn check_unformatted_fails_without_writing() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("A.java");
    std::fs::write(&file, "class A{int x=1;}\n").unwrap();

    let status = jals().args(["fmt", "--check"]).arg(&file).status().unwrap();
    assert_eq!(status.code(), Some(1));
    // The file is left untouched in check mode.
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "class A{int x=1;}\n"
    );
}

#[test]
fn formats_in_place_then_check_passes() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("A.java");
    std::fs::write(&file, "class A{int x=1;}\n").unwrap();

    assert!(jals().arg("fmt").arg(&file).status().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "class A {\n    int x = 1;\n}\n"
    );
    // Now formatted, so --check succeeds.
    assert!(
        jals()
            .args(["fmt", "--check"])
            .arg(&file)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn config_override_changes_indent_width() {
    let dir = tempdir().unwrap();
    let cfg = dir.path().join("custom.toml");
    std::fs::write(&cfg, "indent-width = 2\n").unwrap();

    let (stdout, code) = run_stdin(
        &[&format!("--config={}", cfg.display())],
        "class C{void m(){return;}}",
    );
    assert_eq!(code, 0);
    assert_eq!(stdout, "class C {\n  void m() {\n    return;\n  }\n}\n");
}

#[test]
fn deny_warnings_fails_on_syntax_error() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("E.java");
    std::fs::write(&file, "class E { void m( {\n").unwrap();

    // Without -D warnings the run still succeeds (best-effort formatting).
    assert!(jals().arg("fmt").arg(&file).status().unwrap().success());

    // With -D warnings the syntax errors fail the run.
    let status = jals()
        .args(["fmt", "-D", "warnings"])
        .arg(&file)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn deny_warnings_does_not_swallow_positional_path() {
    // `fmt -D warnings <path>` must treat the path as a positional argument.
    let dir = tempdir().unwrap();
    let file = dir.path().join("Ok.java");
    std::fs::write(&file, "class Ok {}\n").unwrap();

    let status = jals()
        .args(["fmt", "-D", "warnings"])
        .arg(&file)
        .status()
        .unwrap();
    // Already formatted and no syntax warnings -> success.
    assert_eq!(status.code(), Some(0));
}
