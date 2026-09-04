use core::future::{Future, ready};

use jals_classpath::{Fetcher, ProjectInputOptions, ProjectInputPlan, ProjectInputs};
use jals_exec::block_on_inline;
use jals_storage::{CodeTree, Entry, FileKey, MemoryStorage};

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

#[test]
fn typed_source_dependency_roots_collect_only_java_in_stable_order() {
    let tree = CodeTree::new([
        Entry::File(
            FileKey::parse("dep/src/Z.java").unwrap(),
            b"class Z {}".to_vec(),
        ),
        Entry::File(
            FileKey::parse("dep/src/A.java").unwrap(),
            b"class A {}".to_vec(),
        ),
        Entry::File(
            FileKey::parse("dep/src/readme.txt").unwrap(),
            b"text".to_vec(),
        ),
    ])
    .unwrap();
    let mut storage = MemoryStorage::memory(tree);
    let plan = ProjectInputPlan {
        source_dependency_roots: vec![jals_storage::DirKey::parse("dep/src").unwrap()],
        ..ProjectInputPlan::default()
    };
    let inputs = block_on_inline(ProjectInputs::assemble(
        &NoFetch,
        &mut storage,
        &plan,
        ProjectInputOptions::Compile,
        &jals_progress::Progress::SILENT,
    ));
    let files: Vec<_> = inputs
        .source_dep_sources
        .iter()
        .map(|source| match source {
            jals_classpath::SourceFile::Project(key) => key.to_string(),
            jals_classpath::SourceFile::Artifact(_) => panic!("unexpected artifact source"),
        })
        .collect();
    assert_eq!(files, ["dep/src/A.java", "dep/src/Z.java"]);
    assert!(inputs.warnings.is_empty());
}

#[test]
fn missing_source_root_is_diagnostic_not_missing_data() {
    let mut storage = MemoryStorage::memory(CodeTree::default());
    let plan = ProjectInputPlan {
        source_dependency_roots: vec![jals_storage::DirKey::parse("missing").unwrap()],
        ..ProjectInputPlan::default()
    };
    let inputs = block_on_inline(ProjectInputs::assemble(
        &NoFetch,
        &mut storage,
        &plan,
        ProjectInputOptions::Compile,
        &jals_progress::Progress::SILENT,
    ));
    assert!(inputs.source_dep_sources.is_empty());
    assert_eq!(inputs.warnings.len(), 1);
}
