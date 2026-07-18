#![cfg(feature = "native")]
//! Native manifest lowering: host path spellings, in-project `path` dependencies, and
//! out-of-project (sibling) `path` dependencies.

use std::fs;
use std::str::FromStr;

use futures::executor::block_on;
use jals_classpath::{
    ClasspathEntry, Fetcher, NativeProjectPlan, ProjectInputOptions, ProjectInputs, SourceFile,
};
use jals_config::Manifest;
use jals_storage::{CacheNamespace, DirKey, NativeStorage};

struct NoFetch;

impl Fetcher for NoFetch {
    async fn fetch(&self, _: &str) -> Result<Vec<u8>, String> {
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
    let storage =
        NativeStorage::native(project.path(), project.path().join("target/jals/cache")).unwrap();

    let manifest = manifest(
        r#"
[build]
source-dirs = [".", "./src", "src/"]
classpath = ["./libs/dep.jar"]
"#,
    );
    let plan = NativeProjectPlan::from_manifest(&manifest, project.path(), &storage.view());
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
    let storage =
        NativeStorage::native(project.path(), project.path().join("target/jals/cache")).unwrap();

    let manifest = manifest("[dependencies]\nlib = { path = \"./lib\" }\n");
    let plan = NativeProjectPlan::from_manifest(&manifest, project.path(), &storage.view());
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

    let mut storage = NativeStorage::native(&project, project.join("target/jals/cache")).unwrap();
    let manifest = manifest("[dependencies]\nsibling = { path = \"../sibling\" }\n");
    let mut plan = NativeProjectPlan::from_manifest(&manifest, &project, &storage.view());
    assert!(plan.plan.source_dependency_roots.is_empty());
    plan.materialize_path_sources(&project, &mut storage, ProjectInputOptions::Compile);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

    let inputs = block_on(ProjectInputs::assemble(
        &NoFetch,
        &mut storage,
        &plan.plan,
        ProjectInputOptions::Compile,
    ));
    let [SourceFile::Artifact(source)] = inputs.source_dep_sources.as_slice() else {
        panic!(
            "expected one cache-backed path source: {:?}",
            inputs.source_dep_sources
        );
    };
    assert_eq!(source.key.namespace(), CacheNamespace::PathSource);
    assert_eq!(source.path.to_string(), "sibling/pkg/Lib.java");
    assert_eq!(
        storage.artifacts().lookup(&source.key).unwrap().unwrap(),
        b"package pkg; class Lib {}"
    );
}

#[test]
fn missing_path_dependency_is_a_warning_not_a_panic() {
    let project = tempfile::tempdir().unwrap();
    let mut storage =
        NativeStorage::native(project.path(), project.path().join("target/jals/cache")).unwrap();
    let manifest = manifest("[dependencies]\ngone = { path = \"../does-not-exist\" }\n");
    let mut plan = NativeProjectPlan::from_manifest(&manifest, project.path(), &storage.view());
    plan.materialize_path_sources(project.path(), &mut storage, ProjectInputOptions::Compile);
    assert_eq!(plan.warnings.len(), 1);
    assert!(plan.plan.source_dependency_artifacts.is_empty());
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
    let mut storage = NativeStorage::native(&project, project.join("target/jals/cache")).unwrap();
    let mut plan = NativeProjectPlan::from_manifest(&manifest, &project, &storage.view());
    plan.materialize_path_sources(&project, &mut storage, ProjectInputOptions::Compile);

    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert_eq!(plan.plan.source_dependency_artifacts.len(), 3);
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
    let manifest = manifest(&format!(
        r#"
[build]
source-dirs = ["../sibling-source", "{absolute_source}"]
classpath = ["../sibling-classes", "{absolute_class}"]
"#
    ));
    let scopes = NativeProjectPlan::snapshot_scopes(&manifest, &project);
    let mut storage = NativeStorage::for_project_scoped(&project, scopes).unwrap();
    let (inputs, source_roots) = NativeProjectPlan::assemble_blocking(
        &manifest,
        &project,
        &mut storage,
        ProjectInputOptions::Editor,
    );

    assert!(source_roots.is_empty());
    assert!(inputs.warnings.is_empty(), "{:?}", inputs.warnings);
    assert_eq!(inputs.classpath_classes.len(), 2);
    assert_eq!(inputs.source_dep_sources.len(), 2);
}
