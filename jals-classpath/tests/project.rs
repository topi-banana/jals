use std::io::{Cursor, Write};

use jals_classpath::{
    DependencyLocation, DependencySpec, Fetcher, LibrarySource, ProjectInputOptions,
    ProjectInputPlan, ProjectInputs,
};
use jals_config::{Feature, FeatureSet};
use jals_exec::block_on_inline;
use jals_storage::{
    CacheKey, CacheNamespace, CodeTree, ContentDigest, Entry, FileKey, MemoryStorage, Name,
    RelativePath,
};

const BOX_CLASS: &[u8] = include_bytes!("fixtures/Box.class");

struct NoFetch;
impl Fetcher for NoFetch {
    async fn fetch(&self, _: &str) -> Result<Vec<u8>, String> {
        panic!("unexpected fetch")
    }
}

fn jar() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut bytes);
    zip.start_file("Box.class", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(BOX_CLASS).unwrap();
    zip.finish().unwrap();
    bytes.into_inner()
}

fn setup() -> (MemoryStorage, ProjectInputPlan) {
    let tree = CodeTree::new([Entry::File(FileKey::parse("lib/box.jar").unwrap(), jar())]).unwrap();
    let storage = MemoryStorage::memory(tree);
    let plan = ProjectInputPlan {
        dependencies: vec![DependencySpec {
            name: Name::new("box").unwrap(),
            location: DependencyLocation::Project(FileKey::parse("lib/box.jar").unwrap()),
            recursive: false,
        }],
        feature_set: FeatureSet::resolve(&[Feature::Java25]),
        ..ProjectInputPlan::default()
    };
    (storage, plan)
}

#[test]
fn analysis_resolves_and_loads_from_one_storage_revision() {
    let (mut storage, plan) = setup();
    let inputs = block_on_inline(ProjectInputs::assemble(
        &NoFetch,
        &mut storage,
        &plan,
        ProjectInputOptions::Analysis,
    ));
    assert_eq!(inputs.dependency_jars.len(), 1);
    assert_eq!(inputs.classpath_classes.len(), 1);
    assert_eq!(inputs.feature_set, FeatureSet::resolve(&[Feature::Java25]));
    assert!(inputs.warnings.is_empty(), "{:?}", inputs.warnings);
}

#[test]
fn compile_resolves_without_parsing_classfiles() {
    let (mut storage, plan) = setup();
    let inputs = block_on_inline(ProjectInputs::assemble(
        &NoFetch,
        &mut storage,
        &plan,
        ProjectInputOptions::Compile,
    ));
    assert_eq!(inputs.dependency_jars.len(), 1);
    assert!(inputs.classpath_classes.is_empty());
}

#[test]
fn a_published_navigation_source_wins_over_the_skeleton_for_the_same_type() {
    // `Box.class` is on the classpath, so skeleton synthesis renders `Box.java` for it. A build
    // task that published its own `Box.java` is the better answer — it is real source, not a
    // rendering — and both address the type by the same package-relative path. Whichever the host
    // mounts last would otherwise decide, silently.
    let (mut storage, mut plan) = setup();
    let published = b"public final class Box { /* the real thing */ }";
    let key = CacheKey::new(
        CacheNamespace::BuildTaskSource,
        ContentDigest::of(b"published"),
        ContentDigest::of(published),
    );
    block_on_inline(storage.artifacts_mut().publish(&key, published)).unwrap();
    plan.library_source_artifacts = vec![LibrarySource {
        path: RelativePath::parse("Box.java").unwrap(),
        key: key.clone(),
    }];

    let inputs = block_on_inline(ProjectInputs::assemble(
        &NoFetch,
        &mut storage,
        &plan,
        ProjectInputOptions::Editor,
    ));
    let boxes: Vec<_> = inputs
        .library_sources
        .iter()
        .filter(|source| source.path.to_string() == "Box.java")
        .collect();
    assert_eq!(boxes.len(), 1, "{:?}", inputs.library_sources);
    assert_eq!(boxes[0].key, key);
}

#[test]
fn navigation_sources_never_reach_a_compile() {
    // They exist for a reader. Handing them to `javac` alongside the classpath that already defines
    // the same types is a duplicate-class error, not extra coverage.
    let (mut storage, mut plan) = setup();
    let published = b"public final class Box {}";
    let key = CacheKey::new(
        CacheNamespace::BuildTaskSource,
        ContentDigest::of(b"published"),
        ContentDigest::of(published),
    );
    block_on_inline(storage.artifacts_mut().publish(&key, published)).unwrap();
    plan.library_source_artifacts = vec![LibrarySource {
        path: RelativePath::parse("Box.java").unwrap(),
        key,
    }];

    let inputs = block_on_inline(ProjectInputs::assemble(
        &NoFetch,
        &mut storage,
        &plan,
        ProjectInputOptions::Compile,
    ));
    assert!(inputs.library_sources.is_empty());
    assert!(inputs.source_dep_sources.is_empty());
}
