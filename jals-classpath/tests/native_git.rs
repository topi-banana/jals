#![cfg(feature = "native")]

use std::fs;
use std::process::Command;
use std::str::FromStr;

use jals_classpath::{Fetcher, NativeProjectPlan, ProjectInputOptions, ProjectInputs, SourceFile};
use jals_config::Manifest;
use jals_storage::{CacheNamespace, NativeStorage};

/// The build features a test project resolves with nothing selected — its own `[features] default`
/// closure. Every one of these fixtures declares no features, so this is the empty set; it exists so
/// the call sites read as "the default selection" rather than as a magic empty value.
fn features(manifest: &jals_config::Manifest) -> jals_config::ResolvedBuildFeatures {
    manifest
        .resolve_build_features(&[], false, false)
        .expect("fixtures declare no features")
}

struct NoFetch;

impl Fetcher for NoFetch {
    // `Online`: the panic is the assertion — `Offline` would refuse first and pass blind.
    fn network(&self) -> jals_classpath::NetworkPolicy {
        jals_classpath::NetworkPolicy::Online
    }

    async fn fetch_admitted(&self, _: &str) -> Result<Vec<u8>, String> {
        panic!("unexpected fetch")
    }
}

/// The capability an analysis host hands over: it may not reach the network, and a `git clone` is
/// reaching the network even though the bytes never pass through `fetch_admitted`.
struct OfflineFetch;

impl Fetcher for OfflineFetch {
    fn network(&self) -> jals_classpath::NetworkPolicy {
        jals_classpath::NetworkPolicy::Offline
    }

    async fn fetch_admitted(&self, _: &str) -> Result<Vec<u8>, String> {
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

/// An offline capability refuses a remote `git` dependency *before* the clone runs.
///
/// The clone is a subprocess, not a fetch, so it does not pass through `fetch_admitted` and the
/// `no-ungated-fetch` rule cannot see it. What kept an analysis host out of it used to be the
/// `ProjectInputOptions::Analysis` skip — an accident of that option also declining to read the
/// result, and one that disappeared the moment analysis started needing a dependency's types.
///
/// Hermetic and fast by construction: the refusal is the whole point, so `git` is never spawned and
/// `example.invalid` is never resolved. The two failure modes read differently — this asserts the
/// policy's wording, not `git clone failed`.
#[test]
fn an_offline_capability_refuses_a_remote_git_dependency_without_cloning() {
    let project = tempfile::tempdir().unwrap();
    let manifest = Manifest::from_str(
        r#"
[package]
name = "offline-git"

[dependencies]
fixture = { git = "https://example.invalid/fixture.git" }
"#,
    )
    .unwrap();

    jals_exec::tokio_rt::run(|exec| async move {
        let mut storage = NativeStorage::native(
            project.path(),
            project.path().join("target/jals/cache"),
            exec,
        )
        .await
        .unwrap();
        let mut plan = NativeProjectPlan::from_manifest(
            &manifest,
            &features(&manifest),
            project.path(),
            &storage.view(),
        );
        plan.materialize_git_sources(project.path(), &mut storage, &OfflineFetch)
            .await;

        let [warning] = plan.warnings.as_slice() else {
            panic!("expected exactly one refusal: {:?}", plan.warnings);
        };
        let rendered = warning.to_string();
        assert!(
            rendered.contains(jals_classpath::NetworkPolicy::OFFLINE_REFUSAL),
            "the policy refused, rather than `git` failing: {rendered}"
        );
        assert!(
            rendered.contains("example.invalid"),
            "the warning names the locator through its origin: {rendered}"
        );
        assert!(plan.plan.source_dependency_artifacts.is_empty());
    })
    .unwrap();
}

/// A `git` dependency whose locator is a host path is not the network, so an offline capability
/// admits it — the same rule that keeps `jar = "../lib/x.jar"` working under `--offline`.
#[test]
fn an_offline_capability_still_clones_a_local_git_dependency() {
    let repository = fixture_repository(b"package example; public class Hello {}");
    let locator = repository.path().to_string_lossy().replace('\\', "\\\\");
    let project = tempfile::tempdir().unwrap();
    let manifest = Manifest::from_str(&format!(
        r#"
[package]
name = "local-git"

[dependencies]
fixture = {{ git = "{locator}" }}
"#
    ))
    .unwrap();

    jals_exec::tokio_rt::run(|exec| async move {
        let mut storage = NativeStorage::native(
            project.path(),
            project.path().join("target/jals/cache"),
            exec,
        )
        .await
        .unwrap();
        let mut plan = NativeProjectPlan::from_manifest(
            &manifest,
            &features(&manifest),
            project.path(),
            &storage.view(),
        );
        plan.materialize_git_sources(project.path(), &mut storage, &OfflineFetch)
            .await;

        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        assert_eq!(plan.plan.source_dependency_artifacts.len(), 1);
    })
    .unwrap();
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
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        let mut storage = NativeStorage::native(
            project.path(),
            project.path().join("target/jals/cache"),
            exec,
        )
        .await
        .unwrap();
        let mut plan = NativeProjectPlan::from_manifest(
            &manifest,
            &features(&manifest),
            project.path(),
            &storage.view(),
        );
        plan.materialize_git_sources(project.path(), &mut storage, &NoFetch)
            .await;
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

        let inputs = ProjectInputs::assemble(
            &NoFetch,
            &mut storage,
            &plan.plan,
            ProjectInputOptions::Compile,
        )
        .await;
        let [SourceFile::Artifact(source)] = inputs.source_dep_sources.as_slice() else {
            panic!("expected one cache-backed Git source");
        };
        assert_eq!(source.key.namespace(), CacheNamespace::GitCheckout);
        assert_eq!(source.path.to_string(), "fixture/example/Hello.java");
        assert_eq!(
            storage
                .artifacts()
                .lookup(&source.key)
                .await
                .unwrap()
                .unwrap(),
            b"package example; public class Hello {}"
        );

        let materialized = storage
            .artifacts()
            .materialize_source(&source.key, &source.path)
            .await
            .unwrap();
        assert_eq!(
            materialized.extension().and_then(|value| value.to_str()),
            Some("java")
        );
        assert_eq!(
            fs::read(materialized).unwrap(),
            b"package example; public class Hello {}"
        );
    })
    .expect("test runtime bootstraps");
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
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        let mut storage = NativeStorage::native(
            project.path(),
            project.path().join("target/jals/cache"),
            exec,
        )
        .await
        .unwrap();

        let mut first = NativeProjectPlan::from_manifest(
            &manifest,
            &features(&manifest),
            project.path(),
            &storage.view(),
        );
        first
            .materialize_git_sources(project.path(), &mut storage, &NoFetch)
            .await;
        assert!(first.warnings.is_empty(), "{:?}", first.warnings);
        assert_eq!(first.plan.source_dependency_artifacts.len(), 1);

        // The repository is gone; only the published cache can satisfy the second assembly.
        drop(repository);

        let mut second = NativeProjectPlan::from_manifest(
            &manifest,
            &features(&manifest),
            project.path(),
            &storage.view(),
        );
        second
            .materialize_git_sources(project.path(), &mut storage, &NoFetch)
            .await;
        assert!(second.warnings.is_empty(), "{:?}", second.warnings);
        assert_eq!(
            first.plan.source_dependency_artifacts,
            second.plan.source_dependency_artifacts
        );
    })
    .expect("test runtime bootstraps");
}
