use std::io::{Cursor, Write};

use futures::executor::block_on;
use jals_classpath::{
    DependencyLocation, DependencySpec, Fetcher, ProjectInputOptions, ProjectInputPlan,
    ProjectInputs,
};
use jals_config::{Feature, FeatureSet};
use jals_storage::{CodeTree, Entry, FileKey, MemoryStorage, Name};

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
    let inputs = block_on(ProjectInputs::assemble(
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
    let inputs = block_on(ProjectInputs::assemble(
        &NoFetch,
        &mut storage,
        &plan,
        ProjectInputOptions::Compile,
    ));
    assert_eq!(inputs.dependency_jars.len(), 1);
    assert!(inputs.classpath_classes.is_empty());
}
