#![cfg(feature = "native")]
//! Native manifest lowering: host path spellings, in-project `path` dependencies, and
//! out-of-project (sibling) `path` dependencies.

use core::future::{Future, ready};

use std::fs;
use std::str::FromStr;

use jals_classpath::{
    ClasspathEntry, Fetcher, NativeProjectPlan, ProjectInputOptions, ProjectInputs, SourceFile,
};
use jals_config::{DependencyScope, Manifest};
use jals_storage::{CacheNamespace, DirKey, NativeStorage};

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

    fn retry(&self) -> jals_classpath::RetrySchedule {
        jals_classpath::RetrySchedule::none()
    }

    fn delay(&self, _: u32) -> impl Future<Output = ()> {
        ready(())
    }

    fn fetch_admitted(
        &self,
        _: &str,
        _: &jals_progress::Task,
    ) -> impl Future<Output = Result<Vec<u8>, jals_classpath::FetchError>> {
        ready(Self::refuse())
    }
}

impl NoFetch {
    /// Diverges: being asked at all is the failure this fixture asserts against.
    fn refuse() -> Result<Vec<u8>, jals_classpath::FetchError> {
        panic!("unexpected fetch")
    }
}

fn manifest(toml: &str) -> Manifest {
    Manifest::from_str(&format!("[package]\nname = \"fixture\"\n{toml}")).unwrap()
}

#[test]
fn host_path_spellings_normalize_to_project_keys() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::create_dir_all(project.path().join("libs")).unwrap();
    fs::write(project.path().join("libs/dep.jar"), b"jar").unwrap();
    let manifest = manifest(
        r#"
[build]
source-dirs = [".", "./src", "src/"]
classpath = ["./libs/dep.jar"]
"#,
    );

    let plan = jals_exec::tokio_rt::run(|exec| async move {
        let storage = NativeStorage::native(
            project.path(),
            project.path().join("target/jals/cache"),
            exec,
        )
        .await
        .unwrap();
        NativeProjectPlan::from_manifest(
            &manifest,
            DependencyScope::Build,
            &features(&manifest),
            project.path(),
            &storage.view(),
        )
    })
    .expect("test runtime bootstraps");
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert_eq!(
        plan.source_roots,
        [DirKey::ROOT, DirKey::parse("src").unwrap()]
    );
    assert_eq!(plan.plan.classpath.len(), 1);
    assert!(matches!(
        &plan.plan.classpath[0],
        ClasspathEntry::ProjectFile(file) if file.to_string() == "libs/dep.jar"
    ));
}

/// `[test] source-dirs` is a source root of the project too, and under either scope.
///
/// Nothing on a compile path reads this list — a compiler is handed the sources `jals-cli`'s own
/// per-lowering `discover_sources` gathers — so what it decides is which files an *analysis* host
/// indexes. Leaving the test tree out put every file in it outside `Workspace::owns_path`, so the
/// language server answered one from a detached group with no `[package] features` and reported
/// each `#[test]` in it as an error, and `jals lint` saw a named test file's siblings not at all.
/// Unconditional for the reason `snapshot_scopes` captures the same tree unconditionally: the shape
/// of a project must not depend on which subcommand asked.
#[test]
fn the_test_source_dirs_are_source_roots_under_either_scope() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir_all(project.path().join("src/main/java")).unwrap();
    fs::create_dir_all(project.path().join("src/test/java")).unwrap();
    let manifest = manifest(
        r#"
[build]
source-dirs = ["src/main/java"]

[test]
source-dirs = ["src/test/java"]
"#,
    );

    let roots = jals_exec::tokio_rt::run(|exec| async move {
        let storage = NativeStorage::native(
            project.path(),
            project.path().join("target/jals/cache"),
            exec,
        )
        .await
        .unwrap();
        [DependencyScope::Build, DependencyScope::Test].map(|scope| {
            NativeProjectPlan::from_manifest(
                &manifest,
                scope,
                &features(&manifest),
                project.path(),
                &storage.view(),
            )
            .source_roots
        })
    })
    .expect("test runtime bootstraps");
    for (scope, source_roots) in [DependencyScope::Build, DependencyScope::Test]
        .iter()
        .zip(&roots)
    {
        assert_eq!(
            source_roots,
            &[
                DirKey::parse("src/main/java").unwrap(),
                DirKey::parse("src/test/java").unwrap()
            ],
            "source roots under {scope:?}"
        );
    }
}

#[test]
fn in_project_path_dependency_auto_detects_conventional_source_root() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir_all(project.path().join("lib/src/main/java")).unwrap();
    fs::write(
        project.path().join("lib/src/main/java/Lib.java"),
        b"class Lib {}",
    )
    .unwrap();
    // A stray file outside the conventional root must not become an analysis input.
    fs::write(project.path().join("lib/Scratch.java"), b"class Scratch {}").unwrap();
    let manifest = manifest("[dependencies]\nlib = { path = \"./lib\" }\n");

    let plan = jals_exec::tokio_rt::run(|exec| async move {
        let storage = NativeStorage::native(
            project.path(),
            project.path().join("target/jals/cache"),
            exec,
        )
        .await
        .unwrap();
        NativeProjectPlan::from_manifest(
            &manifest,
            DependencyScope::Build,
            &features(&manifest),
            project.path(),
            &storage.view(),
        )
    })
    .expect("test runtime bootstraps");
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert_eq!(
        plan.plan.source_dependency_roots,
        [DirKey::parse("lib/src/main/java").unwrap()]
    );
}

#[test]
fn sibling_path_dependency_is_scanned_and_published() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(base.path().join("sibling/src/main/java/pkg")).unwrap();
    fs::write(
        base.path().join("sibling/src/main/java/pkg/Lib.java"),
        b"package pkg; class Lib {}",
    )
    .unwrap();
    let manifest = manifest("[dependencies]\nsibling = { path = \"../sibling\" }\n");

    jals_exec::tokio_rt::run(|exec| async move {
        let mut storage = NativeStorage::native(&project, project.join("target/jals/cache"), exec)
            .await
            .unwrap();
        let mut plan = NativeProjectPlan::from_manifest(
            &manifest,
            DependencyScope::Build,
            &features(&manifest),
            &project,
            &storage.view(),
        );
        assert!(plan.plan.source_dependency_roots.is_empty());
        plan.materialize_path_sources(&project, &mut storage).await;
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

        let inputs = ProjectInputs::assemble(
            &NoFetch,
            &mut storage,
            &plan.plan,
            ProjectInputOptions::Compile,
            &jals_progress::Progress::SILENT,
        )
        .await;
        let [SourceFile::Artifact(source)] = inputs.source_dep_sources.as_slice() else {
            panic!(
                "expected one cache-backed path source: {:?}",
                inputs.source_dep_sources
            );
        };
        assert_eq!(source.key.namespace(), CacheNamespace::PathSource);
        assert_eq!(source.path.to_string(), "sibling/pkg/Lib.java");
        assert_eq!(
            storage
                .artifacts()
                .lookup(&source.key)
                .await
                .unwrap()
                .unwrap(),
            b"package pkg; class Lib {}"
        );
    })
    .expect("test runtime bootstraps");
}

#[test]
fn missing_path_dependency_is_a_warning_not_a_panic() {
    let project = tempfile::tempdir().unwrap();
    let manifest = manifest("[dependencies]\ngone = { path = \"../does-not-exist\" }\n");
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
            DependencyScope::Build,
            &features(&manifest),
            project.path(),
            &storage.view(),
        );
        plan.materialize_path_sources(project.path(), &mut storage)
            .await;
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.plan.source_dependency_artifacts.is_empty());
    })
    .expect("test runtime bootstraps");
}

#[test]
fn external_dependency_subdirectories_accept_normal_host_spellings() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    let sibling = base.path().join("sibling");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(sibling.join("src")).unwrap();
    fs::write(sibling.join("src/Lib.java"), b"class Lib {}").unwrap();
    let manifest = manifest(
        r#"
[dependencies]
dot = { path = "../sibling", dir = "." }
cur = { path = "../sibling", dir = "./src" }
trailing = { path = "../sibling", dir = "src/" }
"#,
    );
    jals_exec::tokio_rt::run(|exec| async move {
        let mut storage = NativeStorage::native(&project, project.join("target/jals/cache"), exec)
            .await
            .unwrap();
        let mut plan = NativeProjectPlan::from_manifest(
            &manifest,
            DependencyScope::Build,
            &features(&manifest),
            &project,
            &storage.view(),
        );
        plan.materialize_path_sources(&project, &mut storage).await;

        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        assert_eq!(plan.plan.source_dependency_artifacts.len(), 3);
    })
    .expect("test runtime bootstraps");
}

#[test]
fn sibling_and_absolute_build_inputs_are_adapted_without_being_dropped() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    let sibling_source = base.path().join("sibling-source");
    let absolute_source = base.path().join("absolute-source");
    let sibling_classes = base.path().join("sibling-classes");
    let absolute_class = base.path().join("absolute/Box.class");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&sibling_source).unwrap();
    fs::create_dir_all(&absolute_source).unwrap();
    fs::create_dir_all(&sibling_classes).unwrap();
    fs::create_dir_all(absolute_class.parent().unwrap()).unwrap();
    fs::write(sibling_source.join("Sibling.java"), b"class Sibling {}").unwrap();
    fs::write(absolute_source.join("Absolute.java"), b"class Absolute {}").unwrap();
    let box_class = include_bytes!("fixtures/Box.class");
    fs::write(sibling_classes.join("Box.class"), box_class).unwrap();
    fs::write(&absolute_class, box_class).unwrap();

    let absolute_source = absolute_source.to_string_lossy().replace('\\', "\\\\");
    let absolute_class = absolute_class.to_string_lossy().replace('\\', "\\\\");
    let source = format!(
        r#"
[build]
source-dirs = ["../sibling-source", "{absolute_source}"]
classpath = ["../sibling-classes", "{absolute_class}"]
"#
    );

    // Both modes that resolve names, not just the one that also navigates. A source root outside
    // the project is the project's own code — a typing authority — so `jals lint` has to see
    // exactly what an editor sees; before this held, an out-of-root `source-dirs` entry silently
    // stopped widening the lint index and every type it declared read as unresolved.
    for options in [ProjectInputOptions::Editor, ProjectInputOptions::Analysis] {
        let project = project.clone();
        let manifest = manifest(&source);
        let (inputs, source_roots) = jals_exec::tokio_rt::run(|exec| async move {
            let scopes = NativeProjectPlan::snapshot_scopes(&manifest, &project);
            let mut storage = NativeStorage::for_project_scoped(&project, scopes, exec)
                .await
                .unwrap();
            NativeProjectPlan::assemble_native(
                &manifest,
                DependencyScope::Build,
                &features(&manifest),
                &project,
                &mut storage,
                &NoFetch,
                options,
                &jals_progress::Progress::SILENT,
            )
            .await
        })
        .expect("test runtime bootstraps");

        assert!(source_roots.is_empty(), "{options:?}");
        assert!(
            inputs.warnings.is_empty(),
            "{options:?}: {:?}",
            inputs.warnings
        );
        assert_eq!(inputs.classpath_classes.len(), 2, "{options:?}");
        assert_eq!(inputs.source_dep_sources.len(), 2, "{options:?}");
    }
}

#[test]
fn the_snapshot_captures_every_mapping_alternative_and_every_resource() {
    // Both halves are silent when they are missing: a mapping file outside the snapshot makes the
    // captured tree depend on `--features`, and a resource dir outside it produces a jar with no
    // resources in it. Neither reports anything, so the scopes are what has to be pinned.
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    fs::create_dir_all(root.join("maps")).unwrap();
    fs::create_dir_all(root.join("src/main/resources/nested")).unwrap();
    fs::write(root.join("maps/a.txt"), b"pkg.A -> a:\n").unwrap();
    fs::write(root.join("maps/b.txt"), b"pkg.A -> b:\n").unwrap();
    fs::write(root.join("src/main/resources/mixins.json"), b"{}").unwrap();
    fs::write(root.join("src/main/resources/nested/data.bin"), b"\x00\x01").unwrap();

    let manifest = manifest(
        r#"
[features]
"1.20.1" = []
"1.19.4" = []

[build]
remap = { with = "mojmap" }

[[mappings.mojmap]]
file = "maps/a.txt"
required-features = ["1.20.1"]

[[mappings.mojmap]]
file = "maps/b.txt"
required-features = ["1.19.4"]
"#,
    );

    let captured = jals_exec::tokio_rt::run(|exec| async move {
        let scopes = NativeProjectPlan::snapshot_scopes(&manifest, root);
        let storage = NativeStorage::for_project_scoped(root, scopes, exec)
            .await
            .unwrap();
        let view = storage.view();
        view.tree()
            .files_under(&DirKey::ROOT)
            .map(|file| file.key().path().to_string())
            .collect::<Vec<_>>()
    })
    .expect("test runtime bootstraps");

    for expected in [
        // Every alternative, not just the one some selection activates.
        "maps/a.txt",
        "maps/b.txt",
        // Every file below a resource root, whatever its extension.
        "src/main/resources/mixins.json",
        "src/main/resources/nested/data.bin",
    ] {
        assert!(
            captured.iter().any(|path| path == expected),
            "`{expected}` is missing from the captured tree: {captured:?}"
        );
    }
}
