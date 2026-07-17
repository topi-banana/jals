#![cfg(feature = "native")]

use std::fs;
use std::process::Command;
use std::str::FromStr;

use futures::executor::block_on;
use jals_classpath::{Fetcher, NativeProjectPlan, ProjectInputOptions, ProjectInputs, SourceFile};
use jals_config::Manifest;
use jals_storage::{CacheNamespace, NativeStorage};

struct NoFetch;

impl Fetcher for NoFetch {
    async fn fetch(&self, _: &str) -> Result<Vec<u8>, String> {
        panic!("unexpected fetch")
    }
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .expect("git is required by the native adapter contract test");
    assert!(status.success(), "git {args:?} failed");
}

fn git_output(repo: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("git is required by the native adapter contract test");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn fixture_repository(source_text: &[u8]) -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    let source = repository.path().join("src/main/java/example/Hello.java");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, source_text).unwrap();
    git(repository.path(), &["init", "--quiet"]);
    git(
        repository.path(),
        &["config", "user.email", "jals@example.invalid"],
    );
    git(repository.path(), &["config", "user.name", "JALS Test"]);
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "fixture"]);
    repository
}

#[test]
fn git_sources_are_verified_artifacts_and_materialize_with_java_names() {
    let repository = fixture_repository(b"package example; public class Hello {}");

    let locator = repository.path().to_string_lossy().replace('\\', "\\\\");
    let manifest = Manifest::from_str(&format!(
        r#"
[package]
name = "git-fixture"

[dependencies]
fixture = {{ git = "{locator}" }}
"#
    ))
    .unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut storage =
        NativeStorage::native(project.path(), project.path().join("target/jals/cache")).unwrap();
    let mut plan = NativeProjectPlan::from_manifest(&manifest, &storage.view());
    plan.materialize_git_sources(project.path(), &mut storage, ProjectInputOptions::Compile);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

    let inputs = block_on(ProjectInputs::assemble(
        &NoFetch,
        &mut storage,
        &plan.plan,
        ProjectInputOptions::Compile,
    ));
    let [SourceFile::Artifact(source)] = inputs.source_dep_sources.as_slice() else {
        panic!("expected one cache-backed Git source");
    };
    assert_eq!(source.key.namespace(), CacheNamespace::GitCheckout);
    assert_eq!(source.path.to_string(), "fixture/example/Hello.java");
    assert_eq!(
        storage.artifacts().lookup(&source.key).unwrap().unwrap(),
        b"package example; public class Hello {}"
    );

    let materialized = storage
        .artifacts()
        .materialize_source(&source.key, &source.path)
        .unwrap();
    assert_eq!(
        materialized.extension().and_then(|value| value.to_str()),
        Some("java")
    );
    assert_eq!(
        fs::read(materialized).unwrap(),
        b"package example; public class Hello {}"
    );
}

/// A dependency pinned to a `rev` is immutable: once its checkout has been published, a later
/// assembly rebuilds the same artifacts from the cache without cloning — even after the
/// repository itself has disappeared.
#[test]
fn pinned_git_dependency_reuses_the_cached_checkout_without_cloning() {
    let repository = fixture_repository(b"package example; public class Hello {}");
    let rev = git_output(repository.path(), &["rev-parse", "HEAD"]);

    let locator = repository.path().to_string_lossy().replace('\\', "\\\\");
    let manifest = Manifest::from_str(&format!(
        r#"
[package]
name = "git-fixture"

[dependencies]
fixture = {{ git = "{locator}", rev = "{rev}" }}
"#
    ))
    .unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut storage =
        NativeStorage::native(project.path(), project.path().join("target/jals/cache")).unwrap();

    let mut first = NativeProjectPlan::from_manifest(&manifest, &storage.view());
    first.materialize_git_sources(project.path(), &mut storage, ProjectInputOptions::Compile);
    assert!(first.warnings.is_empty(), "{:?}", first.warnings);
    assert_eq!(first.plan.source_dependency_artifacts.len(), 1);

    // The repository is gone; only the published cache can satisfy the second assembly.
    drop(repository);

    let mut second = NativeProjectPlan::from_manifest(&manifest, &storage.view());
    second.materialize_git_sources(project.path(), &mut storage, ProjectInputOptions::Compile);
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
    assert_eq!(
        first.plan.source_dependency_artifacts,
        second.plan.source_dependency_artifacts
    );
}
