//! Integration tests driving the built `jals` binary.

// Ungated since `lint_reads_stdin` writes to the child's stdin on every platform. The gate this
// import used to carry was for the `#[cfg(unix)]` build-script tests, which were its only readers.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::tempdir;

/// Only the `#[cfg(unix)]` build-script tests below package a jar, so the helper carries the same
/// gate: on Windows an ungated one is an item with no caller, which `-D warnings` rejects.
#[cfg(unix)]
fn write_source_jar(path: &Path, entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut archive = zip::ZipWriter::new(file);
    for (name, bytes) in entries {
        archive
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.finish().unwrap();
}

fn jals() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jals"))
}

#[cfg(unix)]
fn read_arg_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
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

/// `base` extended by each `/`-separated segment of `relative`, spelled with the host's separator.
///
/// `Path::join` keeps a `/` inside the string it is handed, so a joined path only *reads* like a
/// host path — matching it against one the CLI printed needs every separator to be the host's.
fn host_join(base: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(base.to_path_buf(), |path, segment| path.join(segment))
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

#[cfg(unix)]
fn snapshot_tree(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                ));
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files
}

#[cfg(unix)]
fn build_with_fake_javac(root: &Path) -> std::process::Output {
    jals()
        .env("JAVAC", fake_javac(root))
        .env("JALS_CAPTURE_ARGS", root.join("failed-javac.args"))
        .env("JALS_CAPTURE_ENV", root.join("failed-javac.env"))
        .env("JALS_CAPTURE_CWD", root.join("failed-javac.cwd"))
        .args(["build", "--manifest-path"])
        .arg(root.join("jals.toml"))
        .output()
        .unwrap()
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

#[cfg(unix)]
#[test]
fn build_tasks_publish_replace_remove_and_clean_an_exclusive_source_root() {
    let manifest = "[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n";
    let dir = project(manifest);
    let jar = dir.path().join("sources.jar");
    let generated = b"package net.example;\npublic class Generated {}\n";
    write_source_jar(
        &jar,
        &[
            ("net/example/Generated.java", generated),
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
        ],
    );
    let script = dir.path().join("build.rhai");
    std::fs::write(
        &script,
        r#"
            let jar = tasks.project_jar("sources.jar");
            let sources = tasks.extract_java(jar, "net/example");
            tasks.publish_tree(
                "example-sources",
                sources,
                "src/main/java/net/example",
                "replace-root",
                "navigation"
            );
        "#,
    )
    .unwrap();
    let manifest_path = dir.path().join("jals.toml");
    let destination = dir.path().join("src/main/java/net/example/Generated.java");

    assert!(build_with_fake_javac(dir.path()).status.success());
    assert_eq!(std::fs::read(&destination).unwrap(), generated);

    std::fs::write(&destination, "user edit\n").unwrap();
    std::fs::write(
        destination.parent().unwrap().join("Manual.txt"),
        "remove me",
    )
    .unwrap();
    assert!(build_with_fake_javac(dir.path()).status.success());
    assert_eq!(std::fs::read(&destination).unwrap(), generated);
    assert!(!destination.parent().unwrap().join("Manual.txt").exists());

    std::fs::write(&script, "let no_tasks = true;\n").unwrap();
    assert!(build_with_fake_javac(dir.path()).status.success());
    assert!(!destination.parent().unwrap().exists());

    std::fs::write(
        &script,
        r#"
            let jar = tasks.project_jar("sources.jar");
            let sources = tasks.extract_java(jar, "net/example");
            tasks.publish_tree("example-sources", sources, "src/main/java/net/example", "replace-root", "navigation");
        "#,
    )
    .unwrap();
    assert!(build_with_fake_javac(dir.path()).status.success());
    assert!(destination.exists());
    assert!(
        jals()
            .args(["clean", "--manifest-path"])
            .arg(&manifest_path)
            .status()
            .unwrap()
            .success()
    );
    assert!(!destination.parent().unwrap().exists());
}

/// `--dry-run` previews a command. A `replace-root` publication owns its destination completely,
/// so applying one during a preview would delete whatever the user has there — including files
/// they wrote by hand and never checked in. Evaluate the plan, publish managed output, and leave
/// the source tree alone.
#[cfg(unix)]
#[test]
fn build_dry_run_leaves_an_exclusive_publication_root_untouched() {
    let manifest = "[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n";
    let dir = project(manifest);
    let jar = dir.path().join("sources.jar");
    write_source_jar(
        &jar,
        &[(
            "net/example/Generated.java",
            b"package net.example;\npublic class Generated {}\n",
        )],
    );
    std::fs::write(
        dir.path().join("build.rhai"),
        r#"
            let jar = tasks.project_jar("sources.jar");
            let sources = tasks.extract_java(jar, "net/example");
            tasks.publish_tree("example-sources", sources, "src/main/java/net/example", "replace-root", "navigation");
        "#,
    )
    .unwrap();

    // Someone's own work sits in the root the script claims.
    let owned = dir.path().join("src/main/java/net/example");
    std::fs::create_dir_all(&owned).unwrap();
    std::fs::write(owned.join("Manual.txt"), "keep me").unwrap();

    assert!(
        jals()
            .args(["build", "--dry-run", "--manifest-path"])
            .arg(dir.path().join("jals.toml"))
            .status()
            .unwrap()
            .success()
    );

    assert_eq!(std::fs::read(owned.join("Manual.txt")).unwrap(), b"keep me");
    assert!(
        !owned.join("Generated.java").exists(),
        "a preview must not publish into the source tree"
    );
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
    // Both the authored and the script-generated source reach javac through the frontend's
    // staging tree, each exactly once.
    let staged = dir.path().join("target/jals/build/frontend");
    for source in [
        host_join(&staged, "src/main/java/com/example/Main.java"),
        host_join(
            &staged,
            "target/jals/build/rhai/out/com/example/DryRunGenerated.java",
        ),
    ] {
        assert_eq!(
            stdout.matches(source.to_string_lossy().as_ref()).count(),
            1,
            "expected {} exactly once in: {stdout}",
            source.display()
        );
    }
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

/// The headline acceptance test: a grouped import desugars through the real frontend + `javac`, and
/// a runtime exception's stack trace names the *original* source line. This is the whole point of
/// preserving line numbers during expansion; the fast desugar unit tests are only a proxy for it.
#[test]
fn grouped_import_preserves_stack_trace_line_through_real_javac() {
    if !javac_available() {
        // No JDK on this machine/CI; the desugar unit tests cover line preservation as a proxy.
        return;
    }
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("jals.toml"),
        "[package]\nname = \"hello\"\nfeatures = [\"grouped-imports\"]\n\
         [run]\nmain-class = \"com.example.Main\"\n",
    )
    .unwrap();
    let src = dir.path().join("src/main/java/com/example");
    std::fs::create_dir_all(&src).unwrap();
    // The grouped import is on line 2; the `throw` is on line 6. Desugaring `.{List, Map}` into two
    // plain imports must stay on line 2 so the throw keeps line 6.
    std::fs::write(
        src.join("Main.java"),
        "package com.example;\n\
         import java.util.{List, Map};\n\
         public class Main {\n\
         \x20\x20\x20\x20public static void main(String[] a) {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20List<String> xs = new java.util.ArrayList<>();\n\
         \x20\x20\x20\x20\x20\x20\x20\x20throw new RuntimeException(\"boom\" + xs + Map.class);\n\
         \x20\x20\x20\x20}\n\
         }\n",
    )
    .unwrap();
    let manifest = dir.path().join("jals.toml");
    let (_stdout, stderr, code) = run_full(&["run", "--manifest-path", manifest.to_str().unwrap()]);
    // The program throws, so the run exits non-zero and the JVM prints a stack trace. The frame
    // must name the original throw line (6); a shifted line would mean expansion broke the mapping.
    assert_ne!(
        code, 0,
        "expected the thrown exception to fail the run; stderr: {stderr}"
    );
    assert!(
        stderr.contains("Main.java:6"),
        "stack trace should name the original throw line 6; stderr: {stderr}"
    );
}

/// The attributes acceptance test: `#[cfg(feature = "…")]` selects between two same-name methods
/// through the real frontend + `javac`, and the surviving `throw`'s stack trace names its
/// *original* line — proving both the duplicate elimination and that blanking the disabled
/// definition (and the attributes themselves) shifted nothing.
#[test]
fn cfg_attribute_selects_code_and_preserves_lines_through_real_javac() {
    if !javac_available() {
        // No JDK on this machine/CI; the strip unit tests cover line preservation as a proxy.
        return;
    }
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("jals.toml"),
        "[package]\nname = \"hello\"\nfeatures = [\"attributes\"]\n\
         [features]\nfancy = []\n\
         [run]\nmain-class = \"com.example.Main\"\n",
    )
    .unwrap();
    let src = dir.path().join("src/main/java/com/example");
    std::fs::create_dir_all(&src).unwrap();
    // Two mutually exclusive definitions of `go()`: without `--features` the "plain" one on
    // line 7 survives; with `--features fancy` the "fancy" one on line 5 does. A statement-level
    // cfg on line 9 additionally exercises the `;`-preserving sole-body strip.
    std::fs::write(
        src.join("Main.java"),
        "package com.example;\n\
         public class Main {\n\
         \x20\x20\x20\x20public static void main(String[] a) { go(); }\n\
         \x20\x20\x20\x20#[cfg(feature = \"fancy\")]\n\
         \x20\x20\x20\x20static void go() { hint(); throw new RuntimeException(\"fancy\"); }\n\
         \x20\x20\x20\x20#[cfg(not(feature = \"fancy\"))]\n\
         \x20\x20\x20\x20static void go() { hint(); throw new RuntimeException(\"plain\"); }\n\
         \x20\x20\x20\x20static void hint() {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20if (a() > 0) #[cfg(feature = \"fancy\")] p();\n\
         \x20\x20\x20\x20}\n\
         \x20\x20\x20\x20static int a() { return 0; }\n\
         \x20\x20\x20\x20static void p() {}\n\
         }\n",
    )
    .unwrap();
    let manifest = dir.path().join("jals.toml");

    let (_stdout, stderr, code) = run_full(&["run", "--manifest-path", manifest.to_str().unwrap()]);
    assert_ne!(
        code, 0,
        "expected the thrown exception to fail the run; stderr: {stderr}"
    );
    assert!(
        stderr.contains("plain") && stderr.contains("Main.java:7"),
        "the plain `go()` on line 7 should have survived; stderr: {stderr}"
    );

    let (_stdout, stderr, code) = run_full(&[
        "run",
        "--features",
        "fancy",
        "--manifest-path",
        manifest.to_str().unwrap(),
    ]);
    assert_ne!(
        code, 0,
        "expected the thrown exception to fail the run; stderr: {stderr}"
    );
    assert!(
        stderr.contains("fancy") && stderr.contains("Main.java:5"),
        "the fancy `go()` on line 5 should have survived; stderr: {stderr}"
    );
}

#[test]
fn a_root_build_script_error_reports_every_diagnostic_it_emitted() {
    let dir = project(
        "[package]\nname = \"reported\"\n\
         [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
    );
    std::fs::write(
        dir.path().join("build.rhai"),
        "build.warning(\"check the version features\");\nbuild.error(\"select at most one\");\n",
    )
    .unwrap();

    // No toolchain is involved: the script runs while the compile inputs are being prepared, well
    // before a backend is chosen, so this needs no `javac` real or fake.
    let output = jals()
        .args(["build", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    // Two diagnostics, each under its own severity and in the order the script emitted them. They
    // used to be flattened into one `error:` line, which read the warning as part of the failure.
    assert!(
        stderr.contains("warning[build-script]: check the version features"),
        "the warning emitted before the fatal diagnostic is context for it; stderr: {stderr}"
    );
    assert!(
        stderr.contains("error[build-script]: select at most one"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.find("warning[build-script]") < stderr.find("error[build-script]"),
        "emission order is preserved; stderr: {stderr}"
    );
}

/// The CLI used to report a broken `build.rhai` with no location at all: it held the Rhai position
/// and never resolved it against the script's text. It now points at the offending line.
#[test]
fn a_failing_build_script_is_reported_at_the_line_it_failed_on() {
    let dir = project(
        "[package]\nname = \"positioned\"\n\
         [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
    );
    std::fs::write(
        dir.path().join("build.rhai"),
        "let valid = 1;\nlet broken = ;\n",
    )
    .unwrap();

    let output = jals()
        .args(["build", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    // An `ariadne` report: the script's path, the failing line number, and the source line itself.
    assert!(stderr.contains("build.rhai:2:"), "stderr: {stderr}");
    assert!(stderr.contains("let broken = ;"), "stderr: {stderr}");
    assert!(stderr.contains("build-script"), "stderr: {stderr}");
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
            build.warning("generated BuildInfo.java");
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

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "stderr: {stderr}");
    // On the success path the diagnostics are warnings by construction, so the `warning:` lead is
    // the CLI's own severity channel and the diagnostic contributes only its message.
    assert!(
        stderr.contains("warning[build-script]: generated BuildInfo.java"),
        "stderr: {stderr}"
    );
    let generated = dir
        .path()
        .join("target/jals/build/rhai/out/com/example/Generated.java");
    assert!(generated.is_file());
    let args = read_arg_lines(&captured_args);
    // The build script's output is root project source, so it goes through the frontend like
    // any authored file; javac sees the staged copy, not the script's own output path.
    let staged_generated = dir
        .path()
        .join("target/jals/build/frontend/target/jals/build/rhai/out/com/example/Generated.java");
    assert!(
        args.iter().any(|arg| Path::new(arg) == staged_generated),
        "generated source should reach javac via the frontend staging tree; args: {args:?}"
    );
    assert!(
        !args.iter().any(|arg| Path::new(arg) == generated),
        "the pre-frontend generated file must not also be passed to javac"
    );
    assert!(args.iter().any(|arg| arg == "-Agenerated=true"));
    assert_eq!(std::fs::read_to_string(captured_env).unwrap(), "from-rhai");
    assert_eq!(
        std::fs::canonicalize(std::fs::read_to_string(captured_cwd).unwrap().trim()).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
}

/// The graph tests pin which channel of a `ProjectInputPlan` a publication lands in; this pins
/// that the host then does something different with the two, which is the whole claim.
#[cfg(unix)]
#[test]
fn a_dependency_publication_reaches_javac_only_when_it_declares_compile_intent() {
    let dir = project(
        "[package]\nname = \"consumer\"\n\
         [dependencies]\nlibrary = { path = \"library\" }\n",
    );
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(
        library.join("jals.toml"),
        "[package]\nname = \"library\"\n\
         [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
    )
    .unwrap();
    write_source_jar(
        &library.join("sources.jar"),
        &[(
            "net/example/Api.java",
            b"package net.example;\npublic class Api {}\n",
        )],
    );

    let build = |intent: &str| {
        std::fs::write(
            library.join("build.rhai"),
            format!(
                r#"
                    let jar = tasks.project_jar("sources.jar");
                    let sources = tasks.extract_java(jar, "net/example");
                    tasks.publish_tree(
                        "example-sources",
                        sources,
                        "src/main/java/net/example",
                        "replace-root",
                        "{intent}",
                    );
                "#
            ),
        )
        .unwrap();
        let captured_args = dir.path().join(format!("{intent}-javac.args"));
        let output = jals()
            .env("JAVAC", fake_javac(dir.path()))
            .env("JALS_CAPTURE_ARGS", &captured_args)
            .env("JALS_CAPTURE_ENV", dir.path().join("javac.env"))
            .env("JALS_CAPTURE_CWD", dir.path().join("javac.cwd"))
            .args(["build", "--manifest-path"])
            .arg(dir.path().join("jals.toml"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        (
            read_arg_lines(&captured_args),
            String::from_utf8(output.stderr).unwrap(),
        )
    };

    // A dependency's source-dependency artifacts materialize under a digest-named directory, so
    // match the file rather than a path this test would have to predict.
    let (compile_args, compile_stderr) = build("compile");
    assert!(
        compile_args.iter().any(|arg| arg.ends_with("Api.java")),
        "a compile publication is an ordinary source-dependency input; args: {compile_args:?}"
    );

    let (navigation_args, navigation_stderr) = build("navigation");
    assert!(
        !navigation_args.iter().any(|arg| arg.ends_with("Api.java")),
        "a navigation publication is a view for the editor, never a compile input; args: \
         {navigation_args:?}"
    );
    // Not vacuous: javac ran, it just was not handed the publication. `read_arg_lines` would have
    // panicked on a missing capture file, and the consumer's own source is what it did compile.
    assert!(
        navigation_args.iter().any(|arg| arg.ends_with("Main.java")),
        "args: {navigation_args:?}"
    );

    // Nothing on the library's classpath defines `net/example` under either intent, so the
    // consumer's build reports the publication. The only end-to-end check that the diagnosis
    // reaches a host at all, naming what the script wrote rather than where discovery put it —
    // under both intents, since the two are wrong in different ways and neither is silent.
    for (intent, stderr) in [
        ("compile", &compile_stderr),
        ("navigation", &navigation_stderr),
    ] {
        assert!(
            stderr.contains("example-sources") && stderr.contains("src/main/java/net/example"),
            "{intent} stderr: {stderr}"
        );
    }
}

#[cfg(unix)]
#[test]
fn transitive_graph_sources_and_classpath_reach_compile_and_run() {
    let dir = project(
        "[package]\nname = \"root\"\n\
         [run]\nmain-class = \"com.example.Main\"\n\
         [dependencies]\nchild = { path = \"child\" }\n",
    );
    std::fs::create_dir_all(dir.path().join("child/src")).unwrap();
    std::fs::write(dir.path().join("child/src/Child.java"), "class Child {}\n").unwrap();
    std::fs::write(
        dir.path().join("child/jals.toml"),
        "[build]\nsource-dirs = [\"src\"]\n\
         [dependencies]\nleaf = { path = \"../leaf\" }\n",
    )
    .unwrap();

    std::fs::create_dir_all(dir.path().join("leaf/src")).unwrap();
    std::fs::create_dir_all(dir.path().join("leaf/libs")).unwrap();
    std::fs::write(dir.path().join("leaf/src/Leaf.java"), "class Leaf {}\n").unwrap();
    std::fs::write(
        dir.path().join("leaf/libs/manifest.jar"),
        b"transitive manifest classpath",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("leaf/jals.toml"),
        "[package]\nname = \"leaf\"\n\
         [build]\nsource-dirs = [\"src\"]\nclasspath = [\"libs/manifest.jar\"]\n\
         script = { type = \"rhai\", file = \"build.rhai\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("leaf/build.rhai"),
        r#"
            if build.env("JALS_PACKAGE_NAME") != "leaf" {
                build.error("dependency did not receive its own package environment");
            }
            let source = output.write_text(
                "com/example/TransitiveGenerated.java",
                "package com.example; public class TransitiveGenerated {}\n",
            );
            let classpath = output.write("script.jar", [9, 8, 7]);
            build.add_source(source);
            build.add_classpath(classpath);
            build.add_javac_arg("-Adependency-only=true");
            build.add_jvm_arg("-Ddependency-only=true");
        "#,
    )
    .unwrap();

    let javac_args_path = dir.path().join("transitive-javac.args");
    let java_args_path = dir.path().join("transitive-java.args");
    let output = jals()
        .env("JAVAC", fake_javac(dir.path()))
        .env("JAVA", fake_java(dir.path()))
        .env("JALS_CAPTURE_ARGS", &javac_args_path)
        .env("JALS_CAPTURE_ENV", dir.path().join("transitive-javac.env"))
        .env("JALS_CAPTURE_CWD", dir.path().join("transitive-javac.cwd"))
        .env("JALS_CAPTURE_JAVA_ARGS", &java_args_path)
        .env(
            "JALS_CAPTURE_RUN_ENV",
            dir.path().join("transitive-java.env"),
        )
        .env(
            "JALS_CAPTURE_JAVA_CWD",
            dir.path().join("transitive-java.cwd"),
        )
        .args(["run", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let javac_args = read_arg_lines(&javac_args_path);
    assert!(
        javac_args
            .iter()
            .any(|arg| arg.ends_with("TransitiveGenerated.java")),
        "javac args: {javac_args:?}"
    );
    assert!(!javac_args.iter().any(|arg| arg == "-Adependency-only=true"));
    let javac_classpath = &javac_args[javac_args
        .iter()
        .position(|arg| arg == "-classpath")
        .unwrap()
        + 1];

    let java_args = read_arg_lines(&java_args_path);
    assert!(!java_args.iter().any(|arg| arg == "-Ddependency-only=true"));
    let java_classpath = &java_args[java_args.iter().position(|arg| arg == "-cp").unwrap() + 1];

    for classpath in [javac_classpath, java_classpath] {
        let entries: Vec<_> = classpath.split(':').map(Path::new).collect();
        assert!(entries.iter().any(|path| {
            std::fs::read(path).is_ok_and(|bytes| bytes == b"transitive manifest classpath")
        }));
        assert!(
            entries
                .iter()
                .any(|path| std::fs::read(path).is_ok_and(|bytes| bytes == [9, 8, 7]))
        );
    }
}

#[cfg(unix)]
#[test]
fn dependency_classpath_directory_is_passed_once_instead_of_member_classes() {
    let dir = project(
        "[package]\nname = \"directory-classpath\"\n\
         [dependencies]\nchild = { path = \"child\" }\n",
    );
    std::fs::create_dir_all(dir.path().join("child")).unwrap();
    std::fs::write(
        dir.path().join("child/jals.toml"),
        "[build]\nclasspath = [\"../classes\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("classes/pkg")).unwrap();
    std::fs::write(dir.path().join("classes/pkg/Api.class"), b"class bytes").unwrap();
    let captured_args = dir.path().join("directory-javac.args");
    let output = jals()
        .env("JAVAC", fake_javac(dir.path()))
        .env("JALS_CAPTURE_ARGS", &captured_args)
        .env("JALS_CAPTURE_ENV", dir.path().join("directory-javac.env"))
        .env("JALS_CAPTURE_CWD", dir.path().join("directory-javac.cwd"))
        .args(["build", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args = read_arg_lines(&captured_args);
    let classpath = &args[args.iter().position(|arg| arg == "-classpath").unwrap() + 1];
    let entries: Vec<_> = classpath.split(':').map(Path::new).collect();
    assert_eq!(entries.len(), 1, "classpath: {entries:?}");
    assert!(entries[0].is_dir(), "classpath: {entries:?}");
    assert_eq!(
        std::fs::read(entries[0].join("pkg/Api.class")).unwrap(),
        b"class bytes"
    );
    assert!(!entries.iter().any(|entry| {
        entry
            .extension()
            .is_some_and(|extension| extension == "class")
    }));
}

#[cfg(unix)]
#[test]
fn graph_failures_prevent_javac() {
    let malformed = project(
        "[package]\nname = \"malformed-root\"\n\
         [dependencies]\nchild = { path = \"child\" }\n",
    );
    std::fs::create_dir_all(malformed.path().join("child")).unwrap();
    std::fs::write(
        malformed.path().join("child/jals.toml"),
        "[build]\nsource-dirs = [\n",
    )
    .unwrap();

    let cycle = project(
        "[package]\nname = \"cycle-root\"\n\
         [dependencies]\na = { path = \"a\" }\n",
    );
    std::fs::create_dir_all(cycle.path().join("a")).unwrap();
    std::fs::create_dir_all(cycle.path().join("b")).unwrap();
    std::fs::write(
        cycle.path().join("a/jals.toml"),
        "[dependencies]\nb = { path = \"../b\" }\n",
    )
    .unwrap();
    std::fs::write(
        cycle.path().join("b/jals.toml"),
        "[dependencies]\na = { path = \"../a\" }\n",
    )
    .unwrap();

    let script = project(
        "[package]\nname = \"script-root\"\n\
         [dependencies]\nchild = { path = \"child\" }\n",
    );
    std::fs::create_dir_all(script.path().join("child/src")).unwrap();
    std::fs::write(
        script.path().join("child/src/Child.java"),
        "class Child {}\n",
    )
    .unwrap();
    std::fs::write(
        script.path().join("child/jals.toml"),
        "[build]\nsource-dirs = [\"src\"]\n\
         script = { type = \"rhai\", file = \"build.rhai\" }\n",
    )
    .unwrap();
    std::fs::write(
        script.path().join("child/build.rhai"),
        "build.error(\"dependency script failed\");\n",
    )
    .unwrap();

    // Two substrings for the script fixture rather than one sentence: the attribution names the
    // dependency and the body is the dependency's own diagnostic, and only the second used to be
    // missing. Splitting them also keeps the host path out of the assertion.
    let cases: [(&tempfile::TempDir, &[&str]); 3] = [
        (&malformed, &["malformed dependency manifest"]),
        (&cycle, &["dependency cycle"]),
        (
            &script,
            &[
                "dependency build script",
                "build script reported: error: dependency script failed",
            ],
        ),
    ];
    for (fixture, expected) in cases {
        let output = build_with_fake_javac(fixture.path());
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8(output.stderr).unwrap();
        for want in expected {
            assert!(stderr.contains(want), "want {want:?}; stderr: {stderr}");
        }
        assert!(!fixture.path().join("failed-javac.args").exists());
    }
}

#[test]
fn lint_warns_and_uses_default_context_when_the_dependency_graph_is_invalid() {
    let dir = project(
        "[package]\nname = \"lint-root\"\n\
         [dependencies]\nchild = { path = \"child\" }\n",
    );
    std::fs::create_dir_all(dir.path().join("child")).unwrap();
    std::fs::write(
        dir.path().join("child/jals.toml"),
        "[build]\nsource-dirs = [\n",
    )
    .unwrap();
    let source = dir.path().join("src/main/java/com/example/Main.java");
    std::fs::write(&source, "package com.example;\npublic class Main {}\n").unwrap();

    let output = jals().arg("lint").arg(source).output().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("warning: project analysis inputs unavailable"));
}

/// Requiring a portable project path for `classes-dir` / `source-dirs` broke projects that
/// predate build scripts: an absolute output directory and a source root outside the project both
/// worked before, and neither has anything to do with the build-script phase.
#[test]
fn host_paths_in_classes_dir_and_source_dirs_still_build() {
    let out = tempdir().unwrap();
    // A TOML *literal* string: a Windows path is full of backslashes, and a basic string would
    // read each one as the start of an escape sequence.
    let dir = project(&format!(
        "[package]\nname = \"hosty\"\n[build]\nclasses-dir = '{}'\n",
        out.path().display()
    ));
    let (stdout, code) = run(&[
        "build",
        "--dry-run",
        "--manifest-path",
        dir.path().join("jals.toml").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains(&format!("-d {}", out.path().display())));

    let shared = tempdir().unwrap();
    std::fs::create_dir_all(shared.path().join("src")).unwrap();
    let dir = project(
        "[package]\nname = \"external\"\n\
         [build]\nsource-dirs = [\"../shared-src\", \"src/main/java\"]\n",
    );
    std::fs::create_dir_all(dir.path().join("../shared-src")).ok();
    let (stdout, code) = run(&[
        "build",
        "--dry-run",
        "--manifest-path",
        dir.path().join("jals.toml").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stdout: {stdout}");
}

/// `--offline` promises to resolve from the verified cache only, but graph discovery ran its own
/// `git clone` regardless — so an offline build still blocked on the network until it timed out.
#[test]
fn offline_does_not_clone_git_dependencies() {
    let dir = project(
        "[package]\nname = \"offline\"\n\
         [dependencies]\ndep = { git = \"https://example.invalid/r.git\" }\n",
    );

    let output = jals()
        .args(["build", "--offline", "--dry-run", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("cannot be acquired offline"),
        "stderr: {stderr}"
    );
}

/// The sibling of the git case: `--offline` reached graph discovery and the build script, but not
/// the input resolution that follows, so an uncached `[dependencies]` jar was still downloaded.
///
/// The assertion is on the gate's own wording rather than merely on "it warned". `example.invalid`
/// never resolves, so this command warned and succeeded before the fix too — from a DNS failure.
/// Only a distinguishable message separates "refused" from "tried and failed".
#[test]
fn offline_does_not_fetch_remote_jar_dependencies() {
    let dir = project(
        "[package]\nname = \"offline-jar\"\n\
         [dependencies]\nlib = { jar = \"https://example.invalid/lib.jar\" }\n",
    );

    let output = jals()
        .args(["build", "--offline", "--dry-run", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("not fetched while offline"),
        "stderr: {stderr}"
    );
}

/// `Manifest::discover_path` returns a bare `jals.toml` when the manifest is in the current
/// directory, and `Path::new("jals.toml").parent()` is `Some("")` rather than `None`. The empty
/// root then failed to canonicalize, so the classpath and feature set were silently dropped and
/// lint ran with a weaker context than it should have.
#[test]
fn lint_keeps_project_context_for_a_manifest_in_the_current_directory() {
    let dir = project("[package]\nname = \"lint-cwd\"\n");
    std::fs::write(
        dir.path().join("src/main/java/com/example/Main.java"),
        "package com.example;\npublic class Main {}\n",
    )
    .unwrap();

    let output = jals()
        .current_dir(dir.path())
        .arg("lint")
        .arg("src/main/java/com/example/Main.java")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("project analysis inputs unavailable"),
        "project context must survive a cwd-relative manifest; stderr: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn dry_run_preprocesses_dependencies_without_mutating_their_tree() {
    let dir = project(
        "[package]\nname = \"dry-graph\"\n\
         [dependencies]\nchild = { path = \"child\" }\n",
    );
    std::fs::create_dir_all(dir.path().join("child/src")).unwrap();
    std::fs::write(dir.path().join("child/src/Child.java"), "class Child {}\n").unwrap();
    std::fs::write(
        dir.path().join("child/jals.toml"),
        "[build]\nsource-dirs = [\"src\"]\n\
         script = { type = \"rhai\", file = \"build.rhai\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("child/build.rhai"),
        r#"
            let source = output.write_text("DryGenerated.java", "class DryGenerated {}\n");
            build.add_source(source);
        "#,
    )
    .unwrap();
    let child = dir.path().join("child");
    let before = snapshot_tree(&child);

    let output = jals()
        .env("JAVAC", fake_javac(dir.path()))
        .args(["build", "--dry-run", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("DryGenerated.java"), "stdout: {stdout}");
    assert_eq!(snapshot_tree(&child), before);
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
    let args = read_arg_lines(&captured_args);
    let output_index = args.iter().position(|arg| arg == "-d").unwrap() + 1;
    // The whole point of the test is that a relative `--manifest-path` is resolved *once*, and
    // resolving it is what `canonicalize` does — so the expectations are canonical too. On macOS a
    // temporary directory is reached through a symlink (`/var` → `/private/var`), and the raw
    // `dir.path()` would disagree with every path the CLI emitted. It matters most for the negative
    // assertion below, which would otherwise hold for the wrong reason.
    let root = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(
        args[output_index],
        root.join("target/classes").to_string_lossy()
    );
    // `javac` is given the frontend's staged output, never the authored file. With the default
    // vanilla frontend the bytes are identical, so this asserts the *path* through the seam
    // rather than any change in what gets compiled.
    assert!(args.iter().any(|arg| {
        Path::new(arg)
            == root.join("target/jals/build/frontend/src/main/java/com/example/Main.java")
    }));
    assert!(
        !args
            .iter()
            .any(|arg| Path::new(arg) == root.join("src/main/java/com/example/Main.java")),
        "the authored source must not reach javac; it would bypass the frontend"
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
    let args = read_arg_lines(&java_args);
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

/// `jals lint --features` drives the `#[cfg(...)]` analysis: a false-`cfg` definition leaves the
/// analysis (its findings are suppressed and duplicates are tolerated), flipping the selection
/// flips which side is live, and a structural attribute error surfaces as a `cfg` diagnostic.
#[test]
fn lint_features_flag_selects_cfg_analysis() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("jals.toml"),
        "[package]\nname = \"hello\"\nfeatures = [\"attributes\"]\n[features]\nfancy = []\n",
    )
    .unwrap();
    let src = dir.path().join("src/main/java/com/example");
    std::fs::create_dir_all(&src).unwrap();
    let main = src.join("Main.java");
    // The disabled branch carries an `empty-catch` finding; the live branch is clean. With the
    // feature off the finding must not be reported, with it on it must be.
    std::fs::write(
        &main,
        "package com.example;\n\
         public class Main {\n\
         \x20\x20\x20\x20#[cfg(feature = \"fancy\")]\n\
         \x20\x20\x20\x20static void go() { try { p(); } catch (Exception e) {} }\n\
         \x20\x20\x20\x20#[cfg(not(feature = \"fancy\"))]\n\
         \x20\x20\x20\x20static void go() { p(); }\n\
         \x20\x20\x20\x20static void p() {}\n\
         }\n",
    )
    .unwrap();

    let (stdout, stderr, code) = run_full(&["lint", main.to_str().unwrap()]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !stdout.contains("empty-catch") && !stderr.contains("empty-catch"),
        "the disabled branch must not be linted: {stdout}{stderr}"
    );

    let (stdout, stderr, code) = run_full(&["lint", "--features", "fancy", main.to_str().unwrap()]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("empty-catch") || stderr.contains("empty-catch"),
        "the live branch is linted under --features fancy: {stdout}{stderr}"
    );

    // A structural attribute error is a `cfg` diagnostic at lint time — the same failure the
    // build would report.
    std::fs::write(
        &main,
        "package com.example;\n#[derive(Debug)]\npublic class Main {}\n",
    )
    .unwrap();
    let (stdout, stderr, code) = run_full(&["lint", main.to_str().unwrap()]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("unknown attribute `derive`")
            || stderr.contains("unknown attribute `derive`"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

// --- `jals lint` goes through the canonical diagnostics assembly --------------------------------
//
// `jals lint` used to sequence the lint engine itself, so it saw neither unresolved names, nor the
// `type-mismatch` suppression a broken parse needs, nor `cfg` hints. It now calls the same
// `jals_editor::FileDiagnostics` assembly the language server and the playground reach through
// `Editor`. Reporting unresolved names is the point of that move, and it is also what makes the
// *index* load-bearing: the run reports on the files it was given, but it has to resolve against
// the whole project, or every name declared elsewhere reads as unknown.

/// Write `text` to `<dir>/src/main/java/com/example/<name>.java` and return its path.
fn example_source(dir: &Path, name: &str, text: &str) -> PathBuf {
    let src = host_join(dir, "src/main/java/com/example");
    std::fs::create_dir_all(&src).unwrap();
    let path = src.join(format!("{name}.java"));
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn lint_resolves_sibling_types_from_the_project_index() {
    // The regression this whole change turns on: linting one file must not report every type the
    // rest of the project declares as unresolvable.
    let dir = project("[package]\nname = \"siblings\"\n");
    example_source(
        dir.path(),
        "Helper",
        "package com.example;\npublic class Helper {}\n",
    );
    let main = example_source(
        dir.path(),
        "Main",
        "package com.example;\npublic class Main { Helper h; }\n",
    );

    let (stdout, stderr, code) = run_full(&["lint", main.to_str().unwrap()]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !stderr.contains("cannot resolve"),
        "a sibling the caller did not name is still a project file: {stderr}"
    );
}

#[test]
fn lint_resolves_types_from_a_path_dependency() {
    // A `path` dependency's sources are a typing authority, not a navigation convenience. The
    // analysis inputs policy withheld them until this change, which made every reference into a
    // dependency an unresolved name the moment `jals lint` started reporting those.
    let dir = project("[package]\nname = \"root\"\n[dependencies]\nlib = { path = \"lib\" }\n");
    let lib = host_join(dir.path(), "lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("jals.toml"), "[package]\nname = \"lib\"\n").unwrap();
    example_source(&lib, "Lib", "package com.example;\npublic class Lib {}\n");
    let main = example_source(
        dir.path(),
        "Main",
        "package com.example;\npublic class Main { Lib l; }\n",
    );

    let (stdout, stderr, code) = run_full(&["lint", main.to_str().unwrap()]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !stderr.contains("cannot resolve"),
        "a `path` dependency's types resolve: {stderr}"
    );
}

#[test]
fn lint_reports_a_genuinely_unresolvable_type() {
    let dir = project("[package]\nname = \"unknowns\"\n");
    let main = example_source(
        dir.path(),
        "Main",
        "package com.example;\npublic class Main { Nope n; }\n",
    );

    let (stdout, stderr, code) = run_full(&["lint", main.to_str().unwrap()]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.contains("cannot resolve symbol `Nope`"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn lint_reports_only_the_named_files() {
    // The index spans the project; the report does not. A finding in a file the caller did not
    // name is not this run's business.
    let dir = project("[package]\nname = \"scoped\"\n");
    example_source(
        dir.path(),
        "Helper",
        "package com.example;\nimport java.util.*;\npublic class Helper {}\n",
    );
    let main = example_source(
        dir.path(),
        "Main",
        "package com.example;\npublic class Main { Helper h; }\n",
    );

    let (stdout, stderr, code) = run_full(&["lint", main.to_str().unwrap()]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !stdout.contains("wildcard-import") && !stderr.contains("wildcard-import"),
        "an unnamed file's findings are not reported: {stdout}{stderr}"
    );
}

#[test]
fn lint_accepts_a_source_root_and_reports_each_file_once() {
    // Naming the source root makes the reported set cover the project's own sources, which is the
    // case where the two sets overlap. The overlap is deduplicated by identity now — the workspace
    // answers by `FileKey`, so the walk and the named path are one file — rather than by the host
    // comparing canonicalized paths before indexing.
    let dir = project("[package]\nname = \"whole\"\n");
    example_source(
        dir.path(),
        "Helper",
        "package com.example;\npublic class Helper {}\n",
    );
    example_source(
        dir.path(),
        "Main",
        "package com.example;\nimport java.util.*;\npublic class Main { Helper h; }\n",
    );
    let root = host_join(dir.path(), "src/main/java");

    let (_, stderr, code) = run_full(&["lint", root.to_str().unwrap()]);
    assert_eq!(code, 1, "the wildcard import is the one finding: {stderr}");
    assert!(
        !stderr.contains("cannot resolve"),
        "a file indexed twice would not stop resolving, but a duplicate declaration is the \
         failure this guards: {stderr}"
    );
    assert_eq!(
        stderr.matches("wildcard-import").count(),
        1,
        "one finding, reported once: {stderr}"
    );
}

#[test]
fn lint_suppresses_resolution_passes_on_a_broken_parse() {
    // A broken tree yields spurious unknowns and type noise, so the assembly forces
    // `type-mismatch` off and skips the unresolved pass. `jals lint` used to run neither rule.
    let dir = project("[package]\nname = \"broken\"\n");
    let main = example_source(
        dir.path(),
        "Main",
        "package com.example;\npublic class Main { Nope n; int x = \"s\";\n",
    );

    let (stdout, stderr, code) = run_full(&["lint", main.to_str().unwrap()]);
    assert_eq!(code, 1, "the syntax error fails the run: {stderr}");
    assert!(
        !stderr.contains("cannot resolve") && !stderr.contains("type-mismatch"),
        "only the syntax errors survive a broken parse: {stdout}{stderr}"
    );
}

#[test]
fn lint_renders_cfg_disabled_regions_as_advice_without_failing() {
    // A `cfg`-disabled region is a hint: worth printing as advice, not worth failing a run over.
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("jals.toml"),
        "[package]\nname = \"hinted\"\nfeatures = [\"attributes\"]\n[features]\nfancy = []\n",
    )
    .unwrap();
    let main = example_source(
        dir.path(),
        "Main",
        "package com.example;\n\
         public class Main {\n\
         \x20\x20\x20\x20#[cfg(feature = \"fancy\")]\n\
         \x20\x20\x20\x20static void go() {}\n\
         }\n",
    );

    let (stdout, stderr, code) = run_full(&["lint", main.to_str().unwrap()]);
    assert_eq!(code, 0, "a hint is not a problem: {stdout}{stderr}");
    assert!(
        stderr.contains("disabled by `cfg`"),
        "the disabled region is still worth saying: {stdout}{stderr}"
    );
}

#[test]
fn lint_does_not_call_an_uncached_dependency_type_unresolved() {
    // `jals lint` never fetches, so a dependency this machine has not built is a jar the classpath
    // does not have. Reporting unresolved names could have made that one warning into one error per
    // reference; it does not, because an imported name is not one the resolver calls unknown. What
    // the run says about it is the refusal itself — rendered whole, so it names the locator.
    //
    // Hermetic: the fetch is refused by the offline policy before any request is made.
    let dir = project(
        "[package]\nname = \"uncached\"\n\
         [dependencies]\nlib = { jar = \"https://example.invalid/lib.jar\" }\n",
    );
    let main = example_source(
        dir.path(),
        "Main",
        "package com.example;\nimport com.example.lib.Thing;\n\
         public class Main { Thing t; Bare b; }\n",
    );

    let (stdout, stderr, code) = run_full(&["lint", main.to_str().unwrap()]);
    assert!(
        stderr.contains("not fetched while offline") && stderr.contains("https://example.invalid"),
        "the refusal names its locator: {stderr}"
    );
    assert!(
        !stderr.contains("cannot resolve symbol `Thing`"),
        "an imported type is not an unresolved name: {stdout}{stderr}"
    );
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.contains("cannot resolve symbol `Bare`"),
        "a genuinely unknown name still is: {stdout}{stderr}"
    );
    // The advisory states the condition; the sentence that clears it belongs to the code, and this
    // channel puts it on its own `note:` line under the diagnostic rather than inside the message.
    // Asserted as two lines because that separation is the whole of what this host adds.
    assert!(
        stderr
            .contains("note[dependency-cache]: some dependencies are not in the verified cache\n")
            && stderr.contains("note: run `jals build` to fetch them\n"),
        "the offline advisory carries its remedy on its own line: {stderr}"
    );
}

#[test]
fn lint_reads_stdin() {
    let mut child = jals()
        .arg("lint")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"import java.util.*;\nclass C {}\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("wildcard-import"), "stderr: {stderr}");
    assert!(stderr.contains("<stdin>"), "stderr: {stderr}");
}

/// Naming a source root *and* a file inside it is one file, reported once.
///
/// The observable half of what `FileKey` identity replaced: the host used to compare canonicalized
/// paths itself before indexing, because indexing one file twice declares its types twice and the
/// index answers an FQN clash by picking a winner. Now the two spellings produce one key, and a
/// key is one indexed file — so the duplicate cannot reach the index to be deduplicated.
#[test]
fn lint_reports_one_file_once_when_a_root_and_a_file_inside_it_are_both_named() {
    let dir = project("[package]\nname = \"overlap\"\n");
    let main = example_source(
        dir.path(),
        "Main",
        "package com.example;\nimport java.util.*;\npublic class Main {}\n",
    );
    let root = host_join(dir.path(), "src/main/java");

    let (stdout, stderr, code) = run_full(&[
        "lint",
        root.to_str().unwrap(),
        main.to_str().unwrap(),
        // The same path a third time, spelled with a redundant `.` and `..` round trip.
        main.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "the wildcard import is the one finding: {stderr}");
    assert_eq!(
        stderr.matches("wildcard-import").count(),
        1,
        "three spellings of one file report once: {stdout}{stderr}"
    );
    assert!(
        !stderr.contains("cannot resolve"),
        "a file indexed twice declares its types twice: {stderr}"
    );

    // The same promise for a file the project snapshot does *not* hold. That one is mounted, and a
    // mount takes a fresh key per reported position — so its identity has to be tracked by path,
    // or two spellings would become two files declaring the same type.
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let stray = scratch.join("Stray.java");
    std::fs::write(&stray, "import java.util.*;\npublic class Stray {}\n").unwrap();
    let spelled_again = host_join(dir.path(), "scratch/../scratch/Stray.java");

    let (stdout, stderr, code) = run_full(&[
        "lint",
        stray.to_str().unwrap(),
        spelled_again.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "the wildcard import is the one finding: {stderr}");
    assert_eq!(
        stderr.matches("wildcard-import").count(),
        1,
        "two spellings of one mounted file report once: {stdout}{stderr}"
    );
}

/// A `jalslint.toml` key this jals does not define is kept, and said out loud.
///
/// Both halves matter and they pull against each other. Rejecting the file would make one stale
/// name silence every *other* rule it configures; ignoring it silently would make a key the file
/// plainly writes do nothing with no way to find out. So the run loads, the good keys apply, and
/// the bad ones are named — against the file that wrote them, and without setting the exit code,
/// because the file being linted is not the file with the problem.
#[test]
fn lint_reports_an_unknown_config_key_without_failing_the_run() {
    let dir = project("[package]\nname = \"stale\"\n");
    std::fs::write(
        dir.path().join("jalslint.toml"),
        // The flat `[rules]` table the sectioned schema replaced, plus a rule that never existed.
        "[rules]\nwildcard-import = \"allow\"\n\n[style]\nno-such-rule = \"warn\"\n",
    )
    .unwrap();
    let source = example_source(
        dir.path(),
        "Stale",
        "package com.example;\npublic class Stale {}\n",
    );

    let (stdout, stderr, code) = run_full(&["lint", source.to_str().unwrap()]);
    assert_eq!(code, 0, "an unknown key is not a finding: {stdout}{stderr}");
    assert!(
        stderr.contains("unknown lint key `rules`"),
        "the stale table is named: {stdout}{stderr}"
    );
    assert!(
        stderr.contains("unknown lint key `style.no-such-rule`"),
        "the stale rule is named, section-qualified: {stdout}{stderr}"
    );
    assert!(
        stderr.contains("jalslint.toml"),
        "named against the file that wrote it: {stdout}{stderr}"
    );
}

/// One run spans directories with different `jalslint.toml` files.
///
/// The config is discovered per reported file, from that file's own directory upward, so a nested
/// config beats the project's. Nothing pinned this before, and it is the one property the move
/// onto `jals_editor::Workspace` could most easily have lost — resolving the config once per run
/// instead of once per file would still pass every other lint test.
#[test]
fn lint_discovers_a_jalslint_config_per_reported_file() {
    let dir = project("[package]\nname = \"perdir\"\n");
    // The project's own config switches the rule off …
    std::fs::write(
        dir.path().join("jalslint.toml"),
        "[style]\nwildcard-import = \"allow\"\n",
    )
    .unwrap();
    let quiet = example_source(
        dir.path(),
        "Quiet",
        "package com.example;\nimport java.util.*;\npublic class Quiet {}\n",
    );
    // … and a nested one switches it back on for its own directory only.
    let nested = host_join(dir.path(), "src/main/java/com/example/nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("jalslint.toml"),
        "[style]\nwildcard-import = \"warn\"\n",
    )
    .unwrap();
    let loud = nested.join("Loud.java");
    std::fs::write(
        &loud,
        "package com.example.nested;\nimport java.util.*;\npublic class Loud {}\n",
    )
    .unwrap();

    let (stdout, stderr, code) =
        run_full(&["lint", quiet.to_str().unwrap(), loud.to_str().unwrap()]);
    assert_eq!(code, 1, "the nested config re-enables the rule: {stderr}");
    assert_eq!(
        stderr.matches("wildcard-import").count(),
        1,
        "only the nested file's config warns: {stdout}{stderr}"
    );
    assert!(
        stderr.contains("Loud.java"),
        "the finding is the nested file's: {stdout}{stderr}"
    );
}

/// A named file under no `[build] source-dirs` root is still linted, and still resolves the
/// project around it.
///
/// The project snapshot does not capture it — no scope covers `scratch/` — so it is read by the
/// host and mounted into the aggregate. Mounting is what makes it a *project file* rather than a
/// detached one: the index it joins is the project's.
#[test]
fn lint_indexes_a_named_file_outside_every_source_root() {
    let dir = project("[package]\nname = \"outside\"\n");
    example_source(
        dir.path(),
        "Helper",
        "package com.example;\npublic class Helper {}\n",
    );
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let stray = scratch.join("Stray.java");
    std::fs::write(
        &stray,
        "import java.util.*;\nimport com.example.Helper;\npublic class Stray { Helper h; }\n",
    )
    .unwrap();

    let (stdout, stderr, code) = run_full(&["lint", stray.to_str().unwrap()]);
    assert_eq!(code, 1, "the wildcard import is reported: {stdout}{stderr}");
    assert!(
        stderr.contains("wildcard-import"),
        "the file outside the source roots is linted: {stdout}{stderr}"
    );
    assert!(
        !stderr.contains("cannot resolve"),
        "it resolves against the project it was mounted into: {stdout}{stderr}"
    );
}

/// A named file outside the project root is linted beside one inside it.
///
/// No key under the project addresses it, so it is mounted like stdin. What this pins is that the
/// two are reported in one run against one index, rather than the out-of-root path being dropped.
#[test]
fn lint_reports_a_named_file_outside_the_project_root() {
    let base = tempdir().unwrap();
    let root = base.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("jals.toml"), "[package]\nname = \"inner\"\n").unwrap();
    let inside = example_source(
        &root,
        "Main",
        "package com.example;\nimport java.util.*;\npublic class Main {}\n",
    );
    let outside_dir = base.path().join("elsewhere");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let outside = outside_dir.join("Stray.java");
    std::fs::write(&outside, "import java.io.*;\npublic class Stray {}\n").unwrap();

    // The first named file anchors project discovery, so this run has a real project *and* a path
    // that lies outside it.
    let (stdout, stderr, code) =
        run_full(&["lint", inside.to_str().unwrap(), outside.to_str().unwrap()]);
    assert_eq!(code, 1, "both wildcard imports report: {stdout}{stderr}");
    assert_eq!(
        stderr.matches("wildcard-import").count(),
        2,
        "the out-of-root file is not dropped: {stdout}{stderr}"
    );
    assert!(
        stderr.contains("Stray.java") && stderr.contains("Main.java"),
        "each is labelled by the path the caller typed: {stdout}{stderr}"
    );
}

/// A `[build] source-dirs` entry resolving outside the project root is a typing authority for
/// `jals lint`, not just for an editor.
///
/// It is the project's own code, so the analysis projection publishes it exactly as the editor
/// projection does. While it did not, every type such a root declared read as unresolved the
/// moment the lint index came from the project snapshot rather than a raw filesystem walk.
#[test]
fn lint_resolves_types_from_a_source_root_outside_the_project() {
    let base = tempdir().unwrap();
    let root = base.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("jals.toml"),
        "[package]\nname = \"external\"\n\
         [build]\nsource-dirs = [\"../shared-src\", \"src/main/java\"]\n",
    )
    .unwrap();
    let shared = base.path().join("shared-src/com/example");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(
        shared.join("Shared.java"),
        "package com.example;\npublic class Shared {}\n",
    )
    .unwrap();
    let main = example_source(
        &root,
        "Main",
        "package com.example;\npublic class Main { Shared s; }\n",
    );

    let (stdout, stderr, code) = run_full(&["lint", main.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "nothing is wrong with this project: {stdout}{stderr}"
    );
    assert!(
        !stderr.contains("cannot resolve"),
        "a source root outside the project still declares its types: {stdout}{stderr}"
    );
}

// --- `jalsfmt.toml` migration ----------------------------------------------------------------
//
// `jals fmt` / `jals init` detect a native Eclipse / IntelliJ / EditorConfig formatter config and
// migrate it into a `jalsfmt.toml` (`jals-fmt/DESIGN.md` §15). What these pin is the *contract*
// around that write: which invocations write, where the file lands, and what is never touched.

/// A project directory whose ancestor walk terminates: the migration refuses to write into a tree
/// with neither a `jals.toml` nor a `.git`, so every fixture below needs a marker.
fn marked_project() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/A.java"), "class A {}\n").unwrap();
    dir
}

/// `jals fmt <root>/src/A.java`, run from `cwd`.
fn fmt_in(cwd: &Path, root: &Path, extra: &[&str]) -> (String, String, i32) {
    let source = root.join("src/A.java");
    let mut args: Vec<&str> = vec!["fmt"];
    args.extend_from_slice(extra);
    let source = source.to_str().unwrap().to_owned();
    args.push(&source);
    let out = jals().args(&args).current_dir(cwd).output().unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
        out.status.code().unwrap(),
    )
}

#[test]
fn fmt_migrates_eclipse_prefs_into_jalsfmt_toml() {
    let dir = marked_project();
    std::fs::create_dir(dir.path().join(".settings")).unwrap();
    std::fs::write(
        dir.path().join(".settings/org.eclipse.jdt.core.prefs"),
        "eclipse.preferences.version=1\n\
         org.eclipse.jdt.core.formatter.lineSplit=120\n\
         org.eclipse.jdt.core.formatter.tabulation.size=2\n",
    )
    .unwrap();

    let (stdout, stderr, code) = fmt_in(dir.path(), dir.path(), &[]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let generated = std::fs::read_to_string(dir.path().join("jalsfmt.toml")).unwrap();
    assert!(
        generated.contains("# Generated by jals from .settings/org.eclipse.jdt.core.prefs"),
        "{generated}"
    );
    assert!(generated.contains("max-width = 120"), "{generated}");
    assert!(generated.contains("indent-width = 2"), "{generated}");
    // The generated file must be the config jals then discovers.
    assert!(stderr.contains("migrating formatter settings"), "{stderr}");
}

#[test]
fn fmt_migrates_an_eclipse_xml_profile() {
    // Also pins that `jals-cli` enables `jals-fmt/std`: without it the XML importers do not exist
    // and this row of the detection ladder is unreachable.
    let dir = marked_project();
    std::fs::write(
        // An arbitrary name — detection is by content, not by file name (DESIGN.md A.1).
        dir.path().join("team-formatter.xml"),
        r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<profiles version="23">
<profile kind="CodeFormatterProfile" name="Team" version="23">
<setting id="org.eclipse.jdt.core.formatter.lineSplit" value="140"/>
<setting id="org.eclipse.jdt.core.formatter.tabulation.size" value="8"/>
</profile>
</profiles>
"#,
    )
    .unwrap();

    let (stdout, stderr, code) = fmt_in(dir.path(), dir.path(), &[]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let generated = std::fs::read_to_string(dir.path().join("jalsfmt.toml")).unwrap();
    assert!(
        generated.contains("# Generated by jals from team-formatter.xml (eclipse 23)."),
        "{generated}"
    );
    assert!(generated.contains("max-width = 140"), "{generated}");
}

#[test]
fn fmt_check_and_no_migrate_write_nothing() {
    for flag in ["--check", "--diff", "--no-migrate"] {
        let dir = marked_project();
        std::fs::write(
            dir.path().join(".editorconfig"),
            "root = true\n[*.java]\nindent_size = 2\nmax_line_length = 120\n",
        )
        .unwrap();

        let (stdout, stderr, code) = fmt_in(dir.path(), dir.path(), &[flag]);

        // The contract pinned here is "no file is written", not the exit code: once the formatter
        // rewrite lands, `--check` against a migrated config will legitimately exit 1 when a file
        // would change.
        assert!(
            code == 0 || code == 1,
            "{flag}: stdout: {stdout}\nstderr: {stderr}"
        );
        assert!(
            !dir.path().join("jalsfmt.toml").exists(),
            "{flag} must not write a config"
        );
        // Detection still happens, so `--check` agrees with what a write-mode run would format.
        assert!(
            stderr.contains("migrating formatter settings"),
            "{flag}: {stderr}"
        );
    }
}

#[test]
fn fmt_never_overwrites_an_existing_jalsfmt_toml() {
    let dir = marked_project();
    std::fs::write(
        dir.path().join(".editorconfig"),
        "root = true\n[*.java]\nindent_size = 2\n",
    )
    .unwrap();
    let authored = "[layout]\nmax-width = 66\n";
    std::fs::write(dir.path().join("jalsfmt.toml"), authored).unwrap();

    let (stdout, stderr, code) = fmt_in(dir.path(), dir.path(), &[]);

    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("jalsfmt.toml")).unwrap(),
        authored
    );
    assert!(
        !stderr.contains("migrating formatter settings"),
        "an authored config ends the ladder: {stderr}"
    );
}

#[test]
fn fmt_finds_the_config_from_a_nested_working_directory() {
    // The answer must not depend on where `jals` was invoked from inside one project, and the
    // generated file belongs at the project root either way.
    let dir = marked_project();
    std::fs::write(
        dir.path().join(".editorconfig"),
        "root = true\n[*.java]\nindent_size = 2\nmax_line_length = 120\n",
    )
    .unwrap();

    let (stdout, stderr, code) = fmt_in(&dir.path().join("src"), dir.path(), &[]);

    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let generated = std::fs::read_to_string(dir.path().join("jalsfmt.toml")).unwrap();
    assert!(generated.contains("max-width = 120"), "{generated}");
    assert!(!dir.path().join("src/jalsfmt.toml").exists());
}

#[test]
fn fmt_writes_one_config_for_two_group_roots() {
    let dir = marked_project();
    std::fs::write(
        dir.path().join(".editorconfig"),
        "root = true\n[*.java]\nindent_size = 2\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("other")).unwrap();
    std::fs::write(dir.path().join("other/B.java"), "class B {}\n").unwrap();

    let a = dir.path().join("src/A.java");
    let b = dir.path().join("other/B.java");
    let (stdout, stderr, code) = run_full(&["fmt", a.to_str().unwrap(), b.to_str().unwrap()]);

    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout.matches("jalsfmt.toml").count(),
        1,
        "one project, one generated config: {stdout}"
    );
    assert!(!dir.path().join("src/jalsfmt.toml").exists());
    assert!(!dir.path().join("other/jalsfmt.toml").exists());
}

#[test]
fn fmt_without_a_project_marker_does_not_migrate() {
    // A tree with neither `jals.toml` nor `.git` is not a project. Walking on would let
    // `jals fmt /tmp/scratch/A.java` pick up `/tmp/.editorconfig` and write outside anything the
    // user considers theirs.
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/A.java"), "class A {}\n").unwrap();
    std::fs::write(
        dir.path().join(".editorconfig"),
        "root = true\n[*.java]\nindent_size = 2\n",
    )
    .unwrap();

    let (stdout, stderr, code) = fmt_in(dir.path(), dir.path(), &[]);

    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(!dir.path().join("jalsfmt.toml").exists());
    assert!(!stderr.contains("migrating formatter settings"), "{stderr}");
}

#[test]
fn fmt_without_a_native_config_writes_nothing() {
    let dir = marked_project();

    let (stdout, stderr, code) = fmt_in(dir.path(), dir.path(), &[]);

    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(!dir.path().join("jalsfmt.toml").exists());
}

#[test]
fn fmt_warns_and_continues_on_an_unreadable_native_config() {
    // A team's broken formatter export is not a reason `jals fmt` cannot format Java.
    let dir = marked_project();
    std::fs::write(
        dir.path().join("formatter.xml"),
        "<profile kind=\"CodeFormatterProfile\"><setting id=\"unclosed\n",
    )
    .unwrap();

    let (stdout, stderr, code) = fmt_in(dir.path(), dir.path(), &[]);

    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.contains("warning: ignoring formatter.xml"),
        "{stderr}"
    );
    assert!(!dir.path().join("jalsfmt.toml").exists());
}

#[test]
fn init_migrates_a_detected_editorconfig() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join(".editorconfig"),
        "root = true\n[*.java]\nindent_size = 2\nmax_line_length = 120\n",
    )
    .unwrap();

    let (stdout, stderr, code) = run_full(&["init", dir.path().to_str().unwrap()]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let generated = std::fs::read_to_string(dir.path().join("jalsfmt.toml")).unwrap();
    assert!(
        generated.contains("# Generated by jals from .editorconfig (intellij)."),
        "{generated}"
    );
    assert!(generated.contains("max-width = 120"), "{generated}");
    // The rest of the scaffold is unaffected.
    assert!(dir.path().join("jals.toml").exists());
    assert!(dir.path().join("src/main/java/Main.java").exists());
}

#[test]
fn init_in_an_empty_dir_still_writes_only_the_three_scaffold_files() {
    let dir = tempdir().unwrap();

    let (stdout, stderr, code) = run_full(&["init", dir.path().to_str().unwrap()]);

    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(dir.path().join("jals.toml").exists());
    assert!(dir.path().join(".gitignore").exists());
    assert!(dir.path().join("src/main/java/Main.java").exists());
    assert!(
        !dir.path().join("jalsfmt.toml").exists(),
        "nothing to migrate ⇒ no config is invented"
    );
}

/// `[build] backend = { type = "jals" }` compiles with the in-process compiler — no JDK involved —
/// and the class files it writes are the ones a real JVM then runs.
///
/// The `javac` path is exercised throughout the rest of this file; this is the other branch of the
/// same `jals build`, so the assertion is on the output landing where `javac -d` would have put it.
#[test]
fn the_jals_backend_compiles_without_a_jdk() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("jals.toml"),
        "[package]\nname = \"demo\"\n\n[build]\nbackend = { type = \"jals\" }\n",
    )
    .unwrap();
    let src = dir.path().join("src/main/java/com/example");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("Main.java"),
        "package com.example;\n\
         public class Main {\n\
         \x20   static int twice(int n) { return n + n; }\n\
         \x20   public static void main(String[] a) { System.out.println(twice(21)); }\n\
         }\n",
    )
    .unwrap();

    let output = jals()
        // Nothing on `PATH` should be consulted; if it were, an absent `javac` would be the error.
        .env("JAVAC", "/nonexistent/javac")
        .args(["build", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let classes = dir.path().join("target/classes");
    let emitted = classes.join("com/example/Main.class");
    assert!(emitted.is_file(), "no class file at {}", emitted.display());

    if !javac_available() {
        return;
    }
    let run = Command::new("java")
        .arg("-cp")
        .arg(&classes)
        .arg("com.example.Main")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "the JVM rejected the compiled class: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    // `println` writes the host's line separator, which is CRLF on Windows.
    assert_eq!(
        String::from_utf8(run.stdout).unwrap().replace("\r\n", "\n"),
        "42\n"
    );
}

/// `[build] backend = { type = "jals-wasm" }` compiles the whole project into one WebAssembly
/// module whose objects are managed by the host's garbage collector.
///
/// wasm has no dynamic loading and no classpath, so one module — not one artifact per type — is the
/// unit. The assertion is that a real engine runs it: `wasmtime` validating and executing the
/// module is the only statement about the encoding that cannot be argued with.
#[test]
fn the_wasm_backend_emits_one_module_for_the_project() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("jals.toml"),
        "[package]\nname = \"demo\"\n\n[build]\nbackend = { type = \"jals-wasm\" }\n",
    )
    .unwrap();
    let src = dir.path().join("src/main/java");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("Point.java"),
        "public class Point {\n\
         \x20   int x;\n\
         \x20   Point(int x) { this.x = x; }\n\
         \x20   int get() { return x; }\n\
         \x20   public static int roundTrip(int n) { Point p = new Point(n); return p.get(); }\n\
         }\n",
    )
    .unwrap();

    let output = jals()
        .args(["build", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let module = dir.path().join("target/classes/project.wasm");
    assert!(module.is_file(), "no module at {}", module.display());
    // The magic every WebAssembly module starts with.
    assert_eq!(&std::fs::read(&module).unwrap()[..4], b"\0asm");

    let available = Command::new("wasmtime")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !available {
        return;
    }
    let run = Command::new("wasmtime")
        .args(["run", "--invoke", "roundTrip"])
        .arg(&module)
        .arg("7")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "wasmtime rejected the module: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "7");
}

/// `jals run` hands a main class to `java`, which cannot be given a WebAssembly module. Saying so
/// beats the alternative: a `classes-dir` holding a `.wasm` and a "no main class" error that is
/// true and useless.
#[test]
fn running_a_wasm_backed_project_explains_itself() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("jals.toml"),
        "[package]\nname = \"demo\"\n\n[run]\nmain-class = \"Main\"\n\n\
         [build]\nbackend = { type = \"jals-wasm\" }\n",
    )
    .unwrap();
    let src = dir.path().join("src/main/java");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("Main.java"),
        "public class Main { public static int one() { return 1; } }\n",
    )
    .unwrap();

    let output = jals()
        .args(["run", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("WebAssembly module") && stderr.contains("wasmtime"),
        "expected an explanation of the wasm backend, got: {stderr}"
    );
}

/// A host compiler's exit code reaches the shell unchanged.
///
/// `javac` distinguishes a compile error (1) from bad arguments (2) and a system error (3), so a
/// build that reported only "nonzero" would leave a script unable to tell a broken invocation from
/// broken source. The compile backend seam carries the code verbatim for exactly this.
#[cfg(unix)]
#[test]
fn a_host_compilers_exit_code_reaches_the_shell() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = project("[package]\nname = \"exit-code\"\n");
    let program = dir.path().join("exiting-javac");
    // Exit 2, the code `javac` uses for a command line it cannot parse.
    std::fs::write(&program, "#!/bin/sh\nexit 2\n").unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&program, permissions).unwrap();

    let output = jals()
        .env("JAVAC", &program)
        .args(["build", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(2),
        "expected the compiler's own exit code, got {:?}",
        output.status.code()
    );
}

/// `jals run` honours `[build] backend`, rather than compiling with `javac` regardless.
///
/// The two steps are selected separately — the compile from `[build] backend`, the run from
/// `[toolchain] runtime` — so asking for the in-process compiler must not silently reach for a JDK
/// to compile with, while still running the classes it produced.
#[cfg(unix)]
#[test]
fn running_a_jals_backed_project_never_reaches_for_javac() {
    let dir = project(
        "[package]\nname = \"demo\"\n\n[run]\nmain-class = \"com.example.Main\"\n\n\
         [build]\nbackend = { type = \"jals\" }\n",
    );

    let javac_args = dir.path().join("run-javac.args");
    let java_args = dir.path().join("run-java.args");
    let output = jals()
        .env("JAVAC", fake_javac(dir.path()))
        .env("JAVA", fake_java(dir.path()))
        .env("JALS_CAPTURE_ARGS", &javac_args)
        .env("JALS_CAPTURE_ENV", dir.path().join("run-javac.env"))
        .env("JALS_CAPTURE_CWD", dir.path().join("run-javac.cwd"))
        .env("JALS_CAPTURE_JAVA_ARGS", &java_args)
        .env("JALS_CAPTURE_RUN_ENV", dir.path().join("run-java.env"))
        .env("JALS_CAPTURE_JAVA_CWD", dir.path().join("run-java.cwd"))
        .args(["run", "--manifest-path"])
        .arg(dir.path().join("jals.toml"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !javac_args.exists(),
        "the in-process backend compiled, so `javac` should never have been spawned"
    );
    // The compile still happened, and its output is on disk where the run step looks for it.
    assert!(
        dir.path()
            .join("target/classes/com/example/Main.class")
            .is_file()
    );
    // And the run step went ahead, with the compiled classes on its classpath.
    let args = read_arg_lines(&java_args);
    let classpath = args[args.iter().position(|arg| arg == "-cp").unwrap() + 1].clone();
    assert!(
        classpath.contains("target/classes"),
        "expected the classes dir on the run classpath, got {classpath}"
    );
    assert!(args.contains(&"com.example.Main".to_owned()));
}

/// A failed in-process compile stops `jals run` before the run step.
///
/// Both steps now share one path, so this is the join that has to hold: a compile that reported
/// source it cannot lower must not fall through to `java`, which would otherwise happily run
/// whatever class files an earlier successful build left in `classes-dir`.
#[cfg(unix)]
#[test]
fn a_failed_in_process_compile_does_not_reach_the_run_step() {
    let dir = project(
        "[package]\nname = \"demo\"\n\n[run]\nmain-class = \"com.example.Main\"\n\n\
         [build]\nbackend = { type = \"jals\" }\n",
    );
    let source = dir.path().join("src/main/java/com/example/Main.java");

    let java_args = dir.path().join("stale-java.args");
    let command = || {
        let mut jals = jals();
        jals.env("JAVAC", "/nonexistent/javac")
            .env("JAVA", fake_java(dir.path()))
            .env("JALS_CAPTURE_JAVA_ARGS", &java_args)
            .env("JALS_CAPTURE_RUN_ENV", dir.path().join("stale-java.env"))
            .env("JALS_CAPTURE_JAVA_CWD", dir.path().join("stale-java.cwd"))
            .args(["run", "--manifest-path"])
            .arg(dir.path().join("jals.toml"));
        jals
    };

    // A first run succeeds and leaves class files behind — the stale output this guards against.
    assert!(command().output().unwrap().status.success());
    assert!(
        dir.path()
            .join("target/classes/com/example/Main.class")
            .is_file()
    );
    std::fs::remove_file(&java_args).unwrap();

    // Now edit the source into something `jals-javac` has no lowering for. The class file from the
    // first run is still on disk and still runnable.
    std::fs::write(
        &source,
        "package com.example;\n\
         public class Main { public static void main(String[] a) { Runnable r = () -> {}; } }\n",
    )
    .unwrap();

    let output = command().output().unwrap();
    assert!(
        !output.status.success(),
        "a compile that reported unlowerable source must fail the run"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Main.java"),
        "expected the file named: {stderr}"
    );
    assert!(
        !java_args.exists(),
        "the run step must not execute the previous build's class files"
    );
}

/// Every member name of a stored jar, in archive order.
fn jar_members(path: &Path) -> Vec<String> {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    (0..archive.len())
        .map(|index| archive.by_index(index).unwrap().name().to_owned())
        .collect()
}

/// A project whose `[build] remap` names a mapping gated on a feature nothing selects, plus one
/// resource. The in-process backend keeps this off the host's JDK.
fn packaging_project() -> tempfile::TempDir {
    let dir = project(
        "[package]\nname = \"packaged\"\nversion = \"0.1.0\"\n\
         [features]\nobfuscated = []\n\
         [build]\nbackend = { type = \"jals\" }\nremap = { with = \"mojmap\" }\n\
         [mappings.mojmap]\nfile = \"maps/server.txt\"\nrequired-features = [\"obfuscated\"]\n",
    );
    std::fs::create_dir_all(dir.path().join("maps")).unwrap();
    std::fs::write(
        dir.path().join("maps/server.txt"),
        "com.example.Main -> a:\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src/main/resources")).unwrap();
    std::fs::write(dir.path().join("src/main/resources/mixins.json"), "{}").unwrap();
    dir
}

#[test]
fn a_declared_remap_with_no_active_mapping_still_writes_a_jar() {
    // "This selection ships no mappings" says *do not rewrite the names*, not *produce nothing*: a
    // release that ships deobfuscated needs the same distributable as one that does not.
    let dir = packaging_project();
    let (stdout, stderr, code) = run_full(&[
        "build",
        "--manifest-path",
        dir.path().join("jals.toml").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{stdout}{stderr}");

    let jar = dir.path().join("target/jals/remap/packaged-0.1.0.jar");
    let members = jar_members(&jar);
    assert_eq!(
        members.first().map(String::as_str),
        Some("META-INF/MANIFEST.MF")
    );
    assert!(
        members.iter().any(|name| name == "com/example/Main.class"),
        "the class keeps its own name when nothing remapped it: {members:?}"
    );
    assert!(
        !members.iter().any(|name| name == "a.class"),
        "the inactive mapping must not have been applied: {members:?}"
    );
}

#[test]
fn an_active_mapping_reobfuscates_the_packaged_jar() {
    // The other half of the same manifest: selecting the gate turns the very same step into a
    // reobfuscation, and the member is addressed by its new name.
    let dir = packaging_project();
    let (stdout, stderr, code) = run_full(&[
        "build",
        "--manifest-path",
        dir.path().join("jals.toml").to_str().unwrap(),
        "--features",
        "obfuscated",
    ]);
    assert_eq!(code, 0, "{stdout}{stderr}");

    let members = jar_members(&dir.path().join("target/jals/remap/packaged-0.1.0.jar"));
    assert!(
        members.iter().any(|name| name == "a.class"),
        "`com.example.Main -> a` should have renamed the member: {members:?}"
    );
}

#[test]
fn resources_are_packaged_into_the_jar() {
    // Resources are authored project files, so they come out of the snapshot — and a remap leaves
    // every non-class member alone, so the same file survives both branches byte for byte.
    let dir = packaging_project();
    let manifest = dir.path().join("jals.toml");
    let manifest = manifest.to_str().unwrap();
    for extra in [&[][..], &["--features", "obfuscated"][..]] {
        let mut args = vec!["build", "--manifest-path", manifest];
        args.extend_from_slice(extra);
        let (stdout, stderr, code) = run_full(&args);
        assert_eq!(code, 0, "{stdout}{stderr}");

        let members = jar_members(&dir.path().join("target/jals/remap/packaged-0.1.0.jar"));
        assert!(
            members.iter().any(|name| name == "mixins.json"),
            "{extra:?}: the resource is addressed below its declared root: {members:?}"
        );
    }
    // `classes-dir` is the compiler's, and resources are not compiler output.
    assert!(!dir.path().join("target/classes/mixins.json").exists());
}

/// The same packaging shape, with one resource declared as a template and two left alone.
///
/// A separate fixture on purpose: `packaging_project` and the tests over it are what assert that a
/// project declaring no `[build.resources]` still gets the byte-for-byte copy it always did, so
/// they are exactly the thing this feature must not disturb.
fn templated_project() -> tempfile::TempDir {
    let dir = project(
        "[package]\nname = \"templated\"\nversion = \"0.1.0\"\n\
         [features]\nserver = []\n\
         [build]\nbackend = { type = \"jals\" }\nremap = { with = \"mojmap\" }\n\
         [mappings.mojmap]\nfile = \"maps/server.txt\"\n\
         [build.resources]\ntemplate = [\"meta.json\"]\n",
    );
    std::fs::create_dir_all(dir.path().join("maps")).unwrap();
    std::fs::write(
        dir.path().join("maps/server.txt"),
        "com.example.Main -> a:\n",
    )
    .unwrap();
    let resources = dir.path().join("src/main/resources");
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::write(
        resources.join("meta.json"),
        "{\"version\":\"{{ package.version }}\",\
         \"env\":\"{% if features.server %}server{% else %}*{% endif %}\"}",
    )
    .unwrap();
    // Undeclared, and it spells a template on purpose.
    std::fs::write(resources.join("keep.txt"), "{{ package.version }}").unwrap();
    // Undeclared and not even text: nothing may decode it.
    std::fs::write(
        resources.join("icon.bin"),
        [0x89, 0x50, 0x4e, 0x47, 0xff, 0xfe],
    )
    .unwrap();
    dir
}

#[cfg(unix)]
#[test]
fn a_declared_resource_template_is_rendered_and_everything_else_keeps_its_bytes() {
    let dir = templated_project();
    let manifest = dir.path().join("jals.toml");
    let manifest = manifest.to_str().unwrap();
    let jar = dir.path().join("target/jals/remap/templated-0.1.0.jar");

    let (stdout, stderr, code) = run_full(&["build", "--manifest-path", manifest]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(
        jar_member(&jar, "meta.json"),
        b"{\"version\":\"0.1.0\",\"env\":\"*\"}".to_vec()
    );
    // Selection is by declaration and never by content, so an undeclared resource keeps its bytes
    // even when they spell a template — and a resource that is not text is never decoded at all.
    assert_eq!(
        jar_member(&jar, "keep.txt"),
        b"{{ package.version }}".to_vec()
    );
    assert_eq!(
        jar_member(&jar, "icon.bin"),
        vec![0x89, 0x50, 0x4e, 0x47, 0xff, 0xfe]
    );

    // The same project under a different selection renders a different member. Nothing else moves.
    let (stdout, stderr, code) =
        run_full(&["build", "--manifest-path", manifest, "--features", "server"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(
        jar_member(&jar, "meta.json"),
        b"{\"version\":\"0.1.0\",\"env\":\"server\"}".to_vec()
    );
    assert_eq!(
        jar_member(&jar, "keep.txt"),
        b"{{ package.version }}".to_vec()
    );
}

#[test]
fn a_resource_template_that_matches_nothing_fails_the_build() {
    // A `resource-dirs` entry that is not there is tolerated because the default lands on every
    // project. A pattern was written on purpose, so a typo fails rather than quietly shipping the
    // file unrendered.
    let dir = templated_project();
    let manifest_path = dir.path().join("jals.toml");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    std::fs::write(
        &manifest_path,
        manifest.replace("\"meta.json\"", "\"meta.jsonn\""),
    )
    .unwrap();

    let (stdout, stderr, code) =
        run_full(&["build", "--manifest-path", manifest_path.to_str().unwrap()]);
    assert_ne!(code, 0, "{stdout}{stderr}");
    assert!(
        stderr.contains("`meta.jsonn` matched no file"),
        "the message has to name the pattern: {stderr}"
    );
}

/// One member of a stored jar, as bytes.
#[cfg(unix)]
fn jar_member(path: &Path, name: &str) -> Vec<u8> {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut member = archive.by_name(name).unwrap();
    let mut bytes = Vec::new();
    std::io::copy(&mut member, &mut bytes).unwrap();
    bytes
}

/// Whether `haystack` contains `needle` as a raw byte run — how a `Utf8` constant is stored.
#[cfg(unix)]
fn contains_bytes(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// `HierarchyEvolution` implements `HierarchyLeft`, which extends `HierarchyRoot`, which is where
/// `rootValue` is declared. Only the client is compiled; the two supertypes on the path to that
/// declaration exist solely inside the jar a build task puts on the classpath.
///
/// Build the project and return the packaged client's bytes. `script` decides whether the task jar
/// is added at all, which is the whole variable: everything else is identical between the two runs.
///
/// Built with the fake `javac`, which compiles nothing — the class bytes are placed in `classes-dir`
/// directly and read back from there, exactly as a real process-based compile would leave them. That
/// keeps this off a host JDK, and `javac` is required regardless: the in-process backend is handed
/// an empty classpath by design and could not compile against a type that lives only in the jar.
#[cfg(unix)]
fn packaged_hierarchy_client(script: Option<&str>) -> Vec<u8> {
    const CLIENT: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../jals-classpath/tests/fixtures/hierarchy-evolution/v1/evolution/HierarchyEvolution.class"
    ));
    const LEFT: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../jals-classpath/tests/fixtures/hierarchy-evolution/v1/evolution/HierarchyLeft.class"
    ));
    const ROOT: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../jals-classpath/tests/fixtures/hierarchy-evolution/v1/evolution/HierarchyRoot.class"
    ));

    let manifest = format!(
        "[package]\nname = \"packaged\"\nversion = \"0.1.0\"\n[build]\n{}\
         remap = {{ with = \"mojmap\" }}\n\
         [mappings.mojmap]\nfile = \"maps/server.txt\"\n",
        if script.is_some() {
            "script = { type = \"rhai\", file = \"build.rhai\" }\n"
        } else {
            ""
        }
    );
    let dir = project(&manifest);
    if let Some(script) = script {
        std::fs::write(dir.path().join("build.rhai"), script).unwrap();
    }
    std::fs::create_dir_all(dir.path().join("vendor")).unwrap();
    write_source_jar(
        &dir.path().join("vendor/lib.jar"),
        &[
            ("evolution/HierarchyLeft.class", LEFT),
            ("evolution/HierarchyRoot.class", ROOT),
        ],
    );
    // Reobfuscation reads the left side as the name the jar already carries. The obfuscated member
    // name is deliberately distinctive: a one- or two-letter name matches somewhere in almost any
    // constant pool, and a raw-byte assertion on it would hold whether or not the walk ever ran.
    std::fs::create_dir_all(dir.path().join("maps")).unwrap();
    std::fs::write(
        dir.path().join("maps/server.txt"),
        "evolution.HierarchyRoot -> evolution.ObfRoot:\n    int rootValue(int) -> hierarchyProvedIt\n",
    )
    .unwrap();
    let classes = dir.path().join("target/classes/evolution");
    std::fs::create_dir_all(&classes).unwrap();
    std::fs::write(classes.join("HierarchyEvolution.class"), CLIENT).unwrap();

    let output = build_with_fake_javac(dir.path());
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    jar_member(
        &dir.path().join("target/jals/remap/packaged-0.1.0.jar"),
        "evolution/HierarchyEvolution.class",
    )
}

/// `[build] remap` closes its class hierarchy with the jars a build task put on the classpath, not
/// only with the ones `[dependencies]` resolved.
///
/// The failure this pins is *silent*: the remap succeeds either way, and the jar it writes without
/// the task jar differs only in that an inherited member kept its source name. So the test is a
/// pair — the same project built with and without the terminal — rather than a single assertion
/// about one output. `jals-classpath`'s `an_inherited_member_needs_the_jar_that_declares_it` is the
/// same mechanism one layer down; what is under test here is whether the host hands the key over.
///
/// Asserted on the presence of the obfuscated name, never on the absence of the original: a rename
/// leaves the superseded `Utf8` and `NameAndType` entries in the pool unreferenced rather than
/// rewriting them, so the source spelling survives in the bytes of a correctly remapped class.
#[cfg(unix)]
#[test]
fn an_inherited_member_from_a_task_jar_is_reobfuscated() {
    let without = packaged_hierarchy_client(None);
    assert!(
        !contains_bytes(&without, "hierarchyProvedIt"),
        "with no jar declaring `HierarchyRoot`, the walk cannot reach the declaration"
    );

    let with = packaged_hierarchy_client(Some(
        r#"tasks.add_classpath(tasks.project_jar("vendor/lib.jar"));"#,
    ));
    assert!(
        contains_bytes(&with, "hierarchyProvedIt"),
        "the inherited member resolves once the task jar closes the hierarchy"
    );
}

// ===== `jals test` =====

/// A project whose `#[test]` methods cover every shape the runner reports differently.
fn test_project() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("jals.toml"),
        "[package]\nname = \"demo\"\nfeatures = [\"attributes\"]\n",
    )
    .unwrap();
    let src = dir.path().join("src/main/java/com/example");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("Calc.java"),
        "package com.example;\n\
         public class Calc {\n\
         \x20   static int add(int a, int b) { return a + b; }\n\
         \x20   #[test]\n\
         \x20   static void adds() { assert add(2, 3) == 5; }\n\
         \x20   #[test]\n\
         \x20   static void addsWrongly() { assert add(2, 3) == 6; }\n\
         \x20   #[test]\n\
         \x20   #[should_fail]\n\
         \x20   static void divideByZero() { int n = 1 / 0; System.out.println(n); }\n\
         \x20   #[test]\n\
         \x20   #[ignore]\n\
         \x20   static void parked() {}\n\
         }\n",
    )
    .unwrap();
    dir
}

/// The manifest path argument for a project directory.
fn manifest_of(dir: &tempfile::TempDir) -> String {
    dir.path().join("jals.toml").to_str().unwrap().to_owned()
}

#[test]
fn test_reports_each_verdict_and_fails_the_run() {
    if !javac_available() {
        return;
    }
    let dir = test_project();
    let (_stdout, stderr, code) = run_full(&[
        "test",
        "--color",
        "never",
        "--manifest-path",
        &manifest_of(&dir),
    ]);
    // A failing test fails the command.
    assert_eq!(code, 1, "stderr: {stderr}");
    // `assert` is honoured, which only happens because the launcher passes `-ea`. Without it this
    // test would report a pass and the whole suite would be vacuous.
    assert!(
        stderr.contains("FAIL") && stderr.contains("Calc#addsWrongly"),
        "the failing assertion must be reported; stderr: {stderr}"
    );
    assert!(stderr.contains("PASS") && stderr.contains("Calc#adds"));
    // `#[should_fail]` passes *because* its body throws.
    assert!(stderr.contains("Calc#divideByZero"));
    // `#[ignore]` is not run, and the count says so.
    assert!(
        stderr.contains("3 tests run: 2 passed, 1 failed"),
        "unexpected summary; stderr: {stderr}"
    );
    assert!(!stderr.contains("PASS") || !stderr.contains("Calc#parked"));
}

#[test]
fn test_list_is_deterministic_and_goes_to_stdout() {
    if !javac_available() {
        return;
    }
    let dir = test_project();
    let args = [
        "test",
        "--list",
        "--color",
        "never",
        "--manifest-path",
        &manifest_of(&dir),
    ];
    let (first, _stderr, code) = run_full(&args);
    assert_eq!(code, 0);
    // Machine output on stdout, so a script can read it without the progress display in the way.
    assert_eq!(
        first.lines().collect::<Vec<_>>(),
        [
            "com.example.Calc#adds",
            "com.example.Calc#addsWrongly",
            "com.example.Calc#divideByZero",
        ]
    );
    // Sorted, and the same on every run: the *report* order is a promise even though the order
    // tests finish in is not.
    let (second, _stderr, _code) = run_full(&args);
    assert_eq!(first, second);
}

#[test]
fn an_ordinary_build_compiles_no_test_method() {
    if !javac_available() {
        return;
    }
    let dir = test_project();
    let (_stdout, stderr, code) = run_full(&["build", "--manifest-path", &manifest_of(&dir)]);
    assert_eq!(code, 0, "stderr: {stderr}");
    // The class exists...
    let classes = dir.path().join("target/classes/com/example");
    assert!(classes.join("Calc.class").is_file());
    // ...and no harness was generated beside it. The shim is named after the test class's whole
    // qualified name, so that this file names the class the generator would actually have written.
    assert!(!classes.join("JalsTest$com$example$Calc.class").exists());
    assert!(
        !dir.path()
            .join("target/classes/JalsTestHarness.class")
            .exists()
    );
    // The test methods are gone from the class file itself. Their names would appear in the
    // constant pool if the methods had been compiled.
    let bytes = std::fs::read(classes.join("Calc.class")).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("add"), "the real method must survive");
    assert!(
        !text.contains("addsWrongly"),
        "a `#[test]` method reached an ordinary build's output"
    );
}

#[test]
fn a_test_run_and_a_build_do_not_share_an_output_directory() {
    if !javac_available() {
        return;
    }
    let dir = test_project();
    let manifest = manifest_of(&dir);
    // Alternating the two commands must leave both outputs intact: they lower to different
    // staging roots and compile to different class directories.
    run_full(&["build", "--manifest-path", &manifest]);
    run_full(&["test", "--color", "never", "--manifest-path", &manifest]);
    run_full(&["build", "--manifest-path", &manifest]);
    assert!(
        dir.path()
            .join("target/classes/com/example/Calc.class")
            .is_file()
    );
    assert!(
        dir.path()
            .join("target/test-classes/JalsTestHarness.class")
            .is_file(),
        "the test run's own output must survive a later build"
    );
}

#[test]
fn a_malformed_test_attribute_is_reported_before_anything_is_compiled() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("jals.toml"),
        "[package]\nname = \"demo\"\nfeatures = [\"attributes\"]\n",
    )
    .unwrap();
    let src = dir.path().join("src/main/java/com/example");
    std::fs::create_dir_all(&src).unwrap();
    // Not `static`: a generated sibling class could not call it.
    std::fs::write(
        src.join("Bad.java"),
        "package com.example;\n\
         public class Bad {\n\
         \x20   #[test]\n\
         \x20   void notStatic() {}\n\
         }\n",
    )
    .unwrap();
    let (_stdout, stderr, code) = run_full(&[
        "test",
        "--color",
        "never",
        "--manifest-path",
        &manifest_of(&dir),
    ]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("must be `static`"),
        "the shape must be named; stderr: {stderr}"
    );
    // The same problem is an edit-time diagnostic, under the fixed `cfg` rule.
    let (_stdout, lint_stderr, lint_code) =
        run_full(&["lint", src.join("Bad.java").to_str().unwrap()]);
    assert_eq!(lint_code, 1);
    assert!(
        lint_stderr.contains("must be `static`"),
        "the linter must report it too; stderr: {lint_stderr}"
    );
}

#[test]
fn test_without_the_attributes_dialect_says_what_to_add() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("jals.toml"), "[package]\nname = \"demo\"\n").unwrap();
    let src = dir.path().join("src/main/java/com/example");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("Main.java"),
        "package com.example;\npublic class Main {}\n",
    )
    .unwrap();
    let (_stdout, stderr, code) = run_full(&[
        "test",
        "--color",
        "never",
        "--manifest-path",
        &manifest_of(&dir),
    ]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("features = [\"attributes\"]"),
        "the message must name the line to add; stderr: {stderr}"
    );
}

#[test]
fn test_filters_select_and_partition_covers_every_test() {
    if !javac_available() {
        return;
    }
    let dir = test_project();
    let manifest = manifest_of(&dir);
    // A substring filter narrows the run.
    let (stdout, _stderr, _code) = run_full(&[
        "test",
        "--list",
        "--color",
        "never",
        "divideByZero",
        "--manifest-path",
        &manifest,
    ]);
    assert_eq!(stdout.lines().count(), 1);

    // The two halves of a partition together cover exactly what the unpartitioned run does.
    let all: Vec<String> = run_full(&["test", "--list", "--manifest-path", &manifest])
        .0
        .lines()
        .map(str::to_owned)
        .collect();
    let mut sharded: Vec<String> = Vec::new();
    for shard in ["count:1/2", "count:2/2"] {
        sharded.extend(
            run_full(&[
                "test",
                "--list",
                "--partition",
                shard,
                "--manifest-path",
                &manifest,
            ])
            .0
            .lines()
            .map(str::to_owned),
        );
    }
    sharded.sort();
    let mut expected = all;
    expected.sort();
    assert_eq!(sharded, expected);
}
