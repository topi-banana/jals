//! Integration tests driving the built `jals` binary.

use std::io::Write;
use std::path::Path;
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

/// Build a minimal project tree (`jals.toml` + one source) under a fresh tempdir.
fn project(manifest: &str) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("jals.toml"), manifest).unwrap();
    let src = dir.path().join("src/main/java/com/example");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("Main.java"),
        "package com.example;\npublic class Main { public static void main(String[] a) {} }\n",
    )
    .unwrap();
    dir
}

/// Run the `jals` binary with `args`, returning (stdout, exit code).
fn run(args: &[&str]) -> (String, i32) {
    let out = jals().args(args).output().unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        out.status.code().unwrap(),
    )
}

/// Run the `jals` binary with `args`, returning (stdout, stderr, exit code).
fn run_full(args: &[&str]) -> (String, String, i32) {
    let out = jals().args(args).output().unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
        out.status.code().unwrap(),
    )
}

/// A `jals.toml` with two `[[bin]]` entries (`one`/`two`); `extra` is appended to `[package]`
/// (e.g. `"default-run = \"two\"\n"` or `""`).
fn two_bin_manifest(extra: &str) -> String {
    format!(
        "[package]\nname = \"hello\"\n{extra}\n\
         [[bin]]\nname = \"one\"\nmain-class = \"com.example.One\"\n\n\
         [[bin]]\nname = \"two\"\nmain-class = \"com.example.Two\"\n"
    )
}

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(unix)]
fn fake_javac(root: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let program = root.join("fake-javac");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$JALS_CAPTURE_ARGS\"\nprintf '%s' \"$JALS_SCRIPT_ENV\" > \"$JALS_CAPTURE_ENV\"\npwd > \"$JALS_CAPTURE_CWD\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&program, permissions).unwrap();
    program
}

#[cfg(unix)]
fn fake_java(root: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let program = root.join("fake-java");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$JALS_CAPTURE_JAVA_ARGS\"\nprintf '%s' \"$JALS_RUN_ENV\" > \"$JALS_CAPTURE_RUN_ENV\"\npwd > \"$JALS_CAPTURE_JAVA_CWD\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&program, permissions).unwrap();
    program
}

/// Whether a dry-run compile command names `javac` as its program. The `[toolchain]` selector
/// resolves the tool to either the bare `javac` (found on `PATH`) or an absolute path into a
/// discovered JDK (`$JAVA_HOME/bin/javac`, `javac.exe` on Windows), so assert on the program's
/// file name rather than the raw first token.
fn names_javac(cmd_line: &str) -> bool {
    cmd_line
        .split_whitespace()
        .next()
        .and_then(|prog| Path::new(prog).file_stem())
        .is_some_and(|stem| stem == "javac")
}

#[test]
fn build_dry_run_prints_javac_command() {
    let dir = project("[package]\nname = \"hello\"\n[build]\nrelease = 21\n");
    let manifest = dir.path().join("jals.toml");
    let (stdout, code) = run(&[
        "build",
        "--dry-run",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(code, 0);
    assert!(names_javac(&stdout), "got: {stdout}");
    assert!(stdout.contains("-d "), "got: {stdout}");
    assert!(stdout.contains("target/classes"), "got: {stdout}");
    assert!(stdout.contains("--release 21"), "got: {stdout}");
    assert!(stdout.contains("Main.java"), "got: {stdout}");
}

#[test]
fn build_dry_run_executes_and_publishes_build_script_outputs() {
    let dir = project(
        "[package]\nname = \"dry-run-script\"\n\
         [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
    );
    std::fs::write(
        dir.path().join("build.rhai"),
        r#"
            let source = output.write_text(
                "com/example/DryRunGenerated.java",
                "package com.example; public class DryRunGenerated {}\n",
            );
            build.add_source(source);
            build.add_source("src/main/java/com/example/Main.java");
        "#,
    )
    .unwrap();
    let manifest = dir.path().join("jals.toml");

    let output = jals()
        .args(["build", "--dry-run", "--manifest-path"])
        .arg(&manifest)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let generated = dir
        .path()
        .join("target/jals/build/rhai/out/com/example/DryRunGenerated.java");
    assert!(generated.is_file());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(generated.to_string_lossy().as_ref()));
    let authored = dir.path().join("src/main/java/com/example/Main.java");
    assert_eq!(
        stdout.matches(authored.to_string_lossy().as_ref()).count(),
        1
    );
}

#[test]
fn build_out_dir_override_in_dry_run() {
    let dir = project("[package]\nname = \"hello\"\n");
    let manifest = dir.path().join("jals.toml");
    let (stdout, code) = run(&[
        "build",
        "--dry-run",
        "--manifest-path",
        manifest.to_str().unwrap(),
        "--out-dir",
        "custom-out",
    ]);
    assert_eq!(code, 0);
    assert!(stdout.contains("custom-out"), "got: {stdout}");
}

#[test]
fn build_no_manifest_in_tree_errors() {
    let dir = tempdir().unwrap();
    let out = jals()
        .arg("build")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("jals.toml"), "stderr: {stderr}");
}

#[test]
fn run_dry_run_prints_javac_and_java_commands() {
    let dir = project("[package]\nname = \"hello\"\n[run]\nmain-class = \"com.example.Main\"\n");
    let manifest = dir.path().join("jals.toml");
    let (stdout, code) = run(&[
        "run",
        "--dry-run",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.contains("javac "), "got: {stdout}");
    assert!(stdout.contains("java -cp "), "got: {stdout}");
    assert!(stdout.contains("com.example.Main"), "got: {stdout}");
}

#[test]
fn run_without_main_class_errors() {
    let dir = project("[package]\nname = \"hello\"\n");
    let manifest = dir.path().join("jals.toml");
    let out = jals()
        .args(["run", "--dry-run", "--manifest-path"])
        .arg(&manifest)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("main class"), "stderr: {stderr}");
}

#[test]
fn build_compiles_when_javac_present() {
    if !javac_available() {
        // No JDK on this machine/CI; the dry-run tests cover command generation.
        return;
    }
    // No explicit `release` so the default JDK's level is used (any JDK works).
    let dir = project("[package]\nname = \"hello\"\n");
    let manifest = dir.path().join("jals.toml");
    let status = jals()
        .args(["build", "--manifest-path"])
        .arg(&manifest)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        dir.path()
            .join("target/classes/com/example/Main.class")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn build_runs_rhai_and_passes_generated_inputs_to_javac() {
    let dir = project(
        "[package]\nname = \"generated\"\n\
         [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
    );
    std::fs::write(
        dir.path().join("build.rhai"),
        r#"
            let source = output.write_text(
                "com/example/Generated.java",
                "package com.example; public class Generated {}\n",
            );
            build.add_source(source);
            build.add_javac_arg("-Agenerated=true");
            build.set_compile_env("JALS_SCRIPT_ENV", "from-rhai");
        "#,
    )
    .unwrap();
    let manifest = dir.path().join("jals.toml");
    let captured_args = dir.path().join("javac.args");
    let captured_env = dir.path().join("javac.env");
    let captured_cwd = dir.path().join("javac.cwd");
    let output = jals()
        .env("JAVAC", fake_javac(dir.path()))
        .env("JALS_CAPTURE_ARGS", &captured_args)
        .env("JALS_CAPTURE_ENV", &captured_env)
        .env("JALS_CAPTURE_CWD", &captured_cwd)
        .args(["build", "--manifest-path"])
        .arg(&manifest)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let generated = dir
        .path()
        .join("target/jals/build/rhai/out/com/example/Generated.java");
    assert!(generated.is_file());
    let args = std::fs::read_to_string(captured_args).unwrap();
    assert!(args.lines().any(|arg| Path::new(arg) == generated));
    assert!(args.lines().any(|arg| arg == "-Agenerated=true"));
    assert_eq!(std::fs::read_to_string(captured_env).unwrap(), "from-rhai");
    assert_eq!(
        std::fs::canonicalize(std::fs::read_to_string(captured_cwd).unwrap().trim()).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn build_with_relative_manifest_path_uses_project_root_once() {
    let dir = project("[package]\nname = \"relative-root\"\n");
    let parent = dir.path().parent().unwrap();
    let relative_manifest = dir.path().file_name().unwrap().to_owned();
    let relative_manifest = Path::new(&relative_manifest).join("jals.toml");
    let captured_args = dir.path().join("relative-javac.args");
    let captured_env = dir.path().join("relative-javac.env");
    let captured_cwd = dir.path().join("relative-javac.cwd");

    let output = jals()
        .current_dir(parent)
        .env("JAVAC", fake_javac(dir.path()))
        .env("JALS_CAPTURE_ARGS", &captured_args)
        .env("JALS_CAPTURE_ENV", &captured_env)
        .env("JALS_CAPTURE_CWD", &captured_cwd)
        .args(["build", "--manifest-path"])
        .arg(relative_manifest)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args: Vec<_> = std::fs::read_to_string(captured_args)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let output_index = args.iter().position(|arg| arg == "-d").unwrap() + 1;
    assert_eq!(
        args[output_index],
        dir.path().join("target/classes").to_string_lossy()
    );
    assert!(
        args.iter().any(|arg| {
            Path::new(arg) == dir.path().join("src/main/java/com/example/Main.java")
        })
    );
    assert_eq!(
        std::fs::canonicalize(std::fs::read_to_string(captured_cwd).unwrap().trim()).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn build_script_skips_non_unicode_environment_entries() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let dir = project(
        "[package]\nname = \"unicode-environment\"\n\
         [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
    );
    std::fs::write(
        dir.path().join("build.rhai"),
        r#"
            if build.env("JALS_UNICODE_ENV") != "visible" {
                build.error("Unicode environment entry was not supplied");
            }
        "#,
    )
    .unwrap();

    let output = jals()
        .env("JALS_UNICODE_ENV", "visible")
        .env(
            OsString::from_vec(b"JALS_NON_UNICODE_\xff".to_vec()),
            "ignored",
        )
        .env(
            "JALS_NON_UNICODE_VALUE",
            OsString::from_vec(vec![b'v', 0xff]),
        )
        .args(["build", "--dry-run", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn run_applies_script_jvm_args_environment_and_ordered_classpath() {
    let dir = project(
        "[package]\nname = \"script-run\"\n\
         [build]\n\
         script = { type = \"rhai\", file = \"build.rhai\" }\n\
         classpath = [\"libs/base.jar\"]\n\
         [run]\nmain-class = \"com.example.Main\"\n\
         [dependencies]\n\
         alpha = { jar = \"libs/alpha.jar\" }\n\
         beta = { jar = \"libs/beta.jar\" }\n",
    );
    let libs = dir.path().join("libs");
    std::fs::create_dir_all(&libs).unwrap();
    std::fs::write(libs.join("base.jar"), b"manifest").unwrap();
    std::fs::write(libs.join("runtime.jar"), b"script").unwrap();
    std::fs::write(libs.join("alpha.jar"), b"alpha dependency").unwrap();
    std::fs::write(libs.join("beta.jar"), b"beta dependency").unwrap();
    std::fs::write(
        dir.path().join("build.rhai"),
        r#"
            build.add_classpath("libs/base.jar");
            build.add_classpath("libs/runtime.jar");
            build.add_jvm_arg("-Dfrom.script=true");
            build.set_compile_env("JALS_SCRIPT_ENV", "compile");
            build.set_run_env("JALS_RUN_ENV", "from-rhai");
        "#,
    )
    .unwrap();

    let manifest = dir.path().join("jals.toml");
    let javac_args = dir.path().join("run-javac.args");
    let javac_env = dir.path().join("run-javac.env");
    let javac_cwd = dir.path().join("run-javac.cwd");
    let java_args = dir.path().join("java.args");
    let java_env = dir.path().join("java.env");
    let java_cwd = dir.path().join("java.cwd");
    let output = jals()
        .env("JAVAC", fake_javac(dir.path()))
        .env("JAVA", fake_java(dir.path()))
        .env("JALS_CAPTURE_ARGS", &javac_args)
        .env("JALS_CAPTURE_ENV", &javac_env)
        .env("JALS_CAPTURE_CWD", &javac_cwd)
        .env("JALS_CAPTURE_JAVA_ARGS", &java_args)
        .env("JALS_CAPTURE_RUN_ENV", &java_env)
        .env("JALS_CAPTURE_JAVA_CWD", &java_cwd)
        .args(["run", "--manifest-path"])
        .arg(&manifest)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args: Vec<_> = std::fs::read_to_string(java_args)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let classpath_flag = args.iter().position(|arg| arg == "-cp").unwrap();
    assert_eq!(args[classpath_flag - 1], "-Dfrom.script=true");
    assert_eq!(args[classpath_flag + 2], "com.example.Main");

    let classpath: Vec<_> = args[classpath_flag + 1].split(':').collect();
    assert_eq!(classpath.len(), 5, "classpath: {classpath:?}");
    assert_eq!(Path::new(classpath[0]), dir.path().join("target/classes"));
    assert_eq!(Path::new(classpath[1]), libs.join("base.jar"));
    assert_eq!(Path::new(classpath[2]), libs.join("runtime.jar"));
    assert_eq!(std::fs::read(classpath[3]).unwrap(), b"alpha dependency");
    assert_eq!(std::fs::read(classpath[4]).unwrap(), b"beta dependency");
    assert_eq!(std::fs::read_to_string(java_env).unwrap(), "from-rhai");
    assert_eq!(
        std::fs::canonicalize(std::fs::read_to_string(java_cwd).unwrap().trim()).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
}

#[test]
fn run_bin_flag_selects_main_class() {
    let dir = project(&two_bin_manifest(""));
    let manifest = dir.path().join("jals.toml");
    let (stdout, code) = run(&[
        "run",
        "--dry-run",
        "--bin",
        "two",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.contains("java -cp "), "got: {stdout}");
    assert!(stdout.contains("com.example.Two"), "got: {stdout}");
    assert!(!stdout.contains("com.example.One"), "got: {stdout}");
}

#[test]
fn run_default_run_picks_default() {
    let dir = project(&two_bin_manifest("default-run = \"two\"\n"));
    let manifest = dir.path().join("jals.toml");
    let (stdout, code) = run(&[
        "run",
        "--dry-run",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.contains("com.example.Two"), "got: {stdout}");
}

#[test]
fn run_ambiguous_bins_errors() {
    let dir = project(&two_bin_manifest(""));
    let manifest = dir.path().join("jals.toml");
    let (_stdout, stderr, code) = run_full(&[
        "run",
        "--dry-run",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(code, 1);
    assert!(stderr.contains("multiple bins"), "stderr: {stderr}");
}

#[test]
fn run_unknown_bin_errors() {
    let dir = project(&two_bin_manifest(""));
    let manifest = dir.path().join("jals.toml");
    let (_stdout, stderr, code) = run_full(&[
        "run",
        "--dry-run",
        "--bin",
        "nope",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(code, 1);
    assert!(stderr.contains("no bin named"), "stderr: {stderr}");
}

#[test]
fn run_main_class_overrides_bins() {
    // `--main-class` short-circuits manifest selection even when `[[bin]]` entries exist.
    let dir = project(&two_bin_manifest("default-run = \"two\"\n"));
    let manifest = dir.path().join("jals.toml");
    let (stdout, code) = run(&[
        "run",
        "--dry-run",
        "--main-class",
        "com.example.Override",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.contains("com.example.Override"), "got: {stdout}");
    assert!(!stdout.contains("com.example.Two"), "got: {stdout}");
}

#[test]
fn run_bin_conflicts_with_main_class() {
    let dir = project(&two_bin_manifest(""));
    let manifest = dir.path().join("jals.toml");
    let (_stdout, stderr, code) = run_full(&[
        "run",
        "--bin",
        "one",
        "--main-class",
        "com.example.Whatever",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    // clap rejects conflicting flags at parse time with usage exit code 2.
    assert_eq!(code, 2);
    assert!(stderr.contains("cannot be used with"), "stderr: {stderr}");
}

#[test]
fn build_unknown_bin_errors() {
    let dir = project(&two_bin_manifest(""));
    let manifest = dir.path().join("jals.toml");
    let (_stdout, stderr, code) = run_full(&[
        "build",
        "--dry-run",
        "--bin",
        "nope",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(code, 1);
    assert!(stderr.contains("no bin named"), "stderr: {stderr}");
}

#[test]
fn build_known_bin_still_compiles_all_sources() {
    // `--bin` validates the name but does not change the compile command.
    let dir = project(&two_bin_manifest(""));
    let manifest = dir.path().join("jals.toml");
    let (stdout, code) = run(&[
        "build",
        "--dry-run",
        "--bin",
        "one",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(code, 0);
    assert!(names_javac(&stdout), "got: {stdout}");
    assert!(stdout.contains("Main.java"), "got: {stdout}");
}

#[test]
fn invalid_manifest_duplicate_bin_errors_early() {
    // A structurally invalid manifest fails on load, for any command (here `build --dry-run`).
    let manifest = "[package]\nname = \"hello\"\n\n\
         [[bin]]\nname = \"dup\"\nmain-class = \"com.example.A\"\n\n\
         [[bin]]\nname = \"dup\"\nmain-class = \"com.example.B\"\n";
    let dir = project(manifest);
    let path = dir.path().join("jals.toml");
    let (_stdout, stderr, code) = run_full(&[
        "build",
        "--dry-run",
        "--manifest-path",
        path.to_str().unwrap(),
    ]);
    assert_eq!(code, 1);
    assert!(stderr.contains("duplicate"), "stderr: {stderr}");
}
