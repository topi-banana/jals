//! Resolving source-form `[dependencies]` (`git` / `path`) to `.java` files: a `path` dependency is
//! read in place (with auto-detected or explicit source root), a missing path is a warning, and a
//! `git` dependency is cloned and the requested ref checked out before its sources are collected.

use std::path::Path;
use std::process::Command;

use jals_build::ManifestExt;
use jals_classpath::DepsCache;
use jals_config::Manifest;

/// Write `jals.toml` with `body` under `root` and load it (parsing + validating), so the test drives
/// the real classification path.
fn manifest(root: &Path, body: &str) -> Manifest {
    std::fs::write(root.join("jals.toml"), body).unwrap();
    Manifest::from_file(&root.join("jals.toml")).unwrap()
}

/// Create `path`'s parents and write `content` to it.
fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Collect the resolved `.java`, asserting no warnings were produced.
fn resolve_ok(manifest: &Manifest, root: &Path) -> Vec<std::path::PathBuf> {
    let mut warnings = Vec::new();
    let files = DepsCache::resolve_project_source_deps(manifest, root, |m| warnings.push(m));
    assert!(warnings.is_empty(), "{warnings:?}");
    files
}

#[test]
fn path_dependency_auto_detects_maven_source_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A `src/main/java` layout under the dependency directory — the host should find it without a
    // `dir`. A stray `.java` outside the source root is *not* collected.
    write(
        &root.join("dep/src/main/java/com/example/Foo.java"),
        "package com.example; public class Foo {}",
    );
    write(&root.join("dep/build.gradle"), "// not java");
    write(&root.join("dep/Ignored.java"), "class Ignored {}");

    let m = manifest(root, "[dependencies]\ndep = { path = \"dep\" }\n");
    let files = resolve_ok(&m, root);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].ends_with("com/example/Foo.java"));
    assert!(
        std::fs::read_to_string(&files[0])
            .unwrap()
            .contains("class Foo")
    );
}

#[test]
fn path_dependency_honors_explicit_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("dep/core/java/A.java"), "class A {}");
    // `src/main/java` exists too, but the explicit `dir` overrides the auto-detection.
    write(&root.join("dep/src/main/java/B.java"), "class B {}");

    let m = manifest(
        root,
        "[dependencies]\ndep = { path = \"dep\", dir = \"core/java\" }\n",
    );
    let files = resolve_ok(&m, root);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].ends_with("A.java"));
}

#[test]
fn path_dependency_falls_back_to_dependency_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // No `src` layout: the dependency root itself is the source root.
    write(&root.join("dep/Flat.java"), "class Flat {}");

    let m = manifest(root, "[dependencies]\ndep = { path = \"dep\" }\n");
    let files = resolve_ok(&m, root);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].ends_with("Flat.java"));
}

#[test]
fn missing_path_dependency_is_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let m = manifest(root, "[dependencies]\ndep = { path = \"absent\" }\n");

    let mut warnings = Vec::new();
    let files = DepsCache::resolve_project_source_deps(&m, root, |m| warnings.push(m));
    assert!(files.is_empty());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("does not exist"), "{warnings:?}");
}

/// Run a `git` subcommand in `dir`, asserting success. Pins identity inline so the test does not
/// depend on the machine's global git config.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.email=test@example.com", "-c", "user.name=test"])
        .args(args)
        .status()
        .expect("git is available");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn git_dependency_clones_checks_out_ref_and_collects_sources() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // A throwaway upstream repo: commit `Foo.java` with "v1" content, tag it `v1`, then change it on
    // the default branch. Pinning `tag = "v1"` must check out the original content, not the tip.
    let upstream = root.join("upstream");
    let foo = upstream.join("src/main/java/Foo.java");
    write(&foo, "class Foo { /* v1 */ }");
    git(&upstream, &["init", "-q", "-b", "main"]);
    git(&upstream, &["add", "."]);
    git(&upstream, &["commit", "-q", "-m", "v1"]);
    git(&upstream, &["tag", "v1"]);
    std::fs::write(&foo, "class Foo { /* v2 */ }").unwrap();
    git(&upstream, &["commit", "-q", "-am", "v2"]);

    let url = upstream.to_string_lossy().into_owned();
    let m = manifest(
        root,
        &format!("[dependencies]\ndep = {{ git = \"{url}\", tag = \"v1\" }}\n"),
    );
    let files = resolve_ok(&m, root);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].ends_with("Foo.java"));
    // The checked-out content is the tagged `v1`, proving the ref checkout, not just the clone.
    let content = std::fs::read_to_string(&files[0]).unwrap();
    assert!(content.contains("v1"), "{content}");
    assert!(!content.contains("v2"), "{content}");
    // The clone landed under the project's `target/jals/deps/git` cache.
    assert!(files[0].starts_with(root.join("target/jals/deps/git")));

    // A second resolution reuses the existing clone (cache hit) and yields the same path.
    let again = resolve_ok(&m, root);
    assert_eq!(files, again);
}
