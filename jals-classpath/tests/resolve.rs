use std::sync::atomic::{AtomicUsize, Ordering};

use futures::executor::block_on;
use jals_classpath::{
    DependencyLocation, DependencyResolver, DependencySpec, ExternalLocator, Fetcher,
};
use jals_storage::{CodeTree, ContentDigest, Entry, FileKey, MemoryStorage, Name};

struct MockFetcher {
    bytes: Vec<u8>,
    calls: AtomicUsize,
}

impl Fetcher for MockFetcher {
    async fn fetch(&self, _locator: &str) -> Result<Vec<u8>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(self.bytes.clone())
    }
}

#[test]
fn project_dependency_is_read_from_the_captured_revision() {
    let tree = CodeTree::new([Entry::File(
        FileKey::parse("lib/dep.jar").unwrap(),
        b"jar".to_vec(),
    )])
    .unwrap();
    let mut storage = MemoryStorage::memory(tree);
    let fetcher = MockFetcher {
        bytes: Vec::new(),
        calls: AtomicUsize::new(0),
    };
    let spec = DependencySpec {
        name: Name::new("dep").unwrap(),
        location: DependencyLocation::Project(FileKey::parse("lib/dep.jar").unwrap()),
        recursive: false,
    };
    let resolved = block_on(DependencyResolver::resolve(
        &fetcher,
        &storage.view(),
        storage.artifacts_mut(),
        &[spec],
    ));
    assert_eq!(resolved.jars.len(), 1);
    assert!(resolved.warnings.is_empty());
    assert_eq!(
        storage
            .artifacts()
            .lookup(&resolved.jars[0].key)
            .unwrap()
            .unwrap(),
        b"jar"
    );
}

#[test]
fn expected_digest_enables_verified_external_cache_hits() {
    let mut storage = MemoryStorage::memory(CodeTree::default());
    let fetcher = MockFetcher {
        bytes: b"remote jar".to_vec(),
        calls: AtomicUsize::new(0),
    };
    let spec = DependencySpec {
        name: Name::new("remote").unwrap(),
        location: DependencyLocation::External {
            locator: ExternalLocator::new("https://example.invalid/dep.jar"),
            expected: Some(ContentDigest::of(b"remote jar")),
        },
        recursive: false,
    };
    let first = block_on(DependencyResolver::resolve(
        &fetcher,
        &storage.view(),
        storage.artifacts_mut(),
        std::slice::from_ref(&spec),
    ));
    assert_eq!(fetcher.calls.load(Ordering::Relaxed), 1);
    let second = block_on(DependencyResolver::resolve(
        &fetcher,
        &storage.view(),
        storage.artifacts_mut(),
        &[spec],
    ));
    assert_eq!(
        fetcher.calls.load(Ordering::Relaxed),
        1,
        "second resolution must hit cache"
    );
    assert_eq!(first.jars[0].key, second.jars[0].key);
}

struct FailingFetcher;

impl Fetcher for FailingFetcher {
    async fn fetch(&self, _locator: &str) -> Result<Vec<u8>, String> {
        Err("network unavailable".to_owned())
    }
}

#[test]
fn digest_less_external_dependency_resolves_from_cache_offline() {
    let mut storage = MemoryStorage::memory(CodeTree::default());
    let fetcher = MockFetcher {
        bytes: b"remote jar".to_vec(),
        calls: AtomicUsize::new(0),
    };
    let spec = DependencySpec {
        name: Name::new("remote").unwrap(),
        location: DependencyLocation::External {
            locator: ExternalLocator::new("https://example.invalid/dep.jar"),
            expected: None,
        },
        recursive: false,
    };
    let first = block_on(DependencyResolver::resolve(
        &fetcher,
        &storage.view(),
        storage.artifacts_mut(),
        std::slice::from_ref(&spec),
    ));
    assert_eq!(fetcher.calls.load(Ordering::Relaxed), 1);
    assert!(first.warnings.is_empty());

    // The second resolution has no network at all; the locator index recovers the cached jar.
    let second = block_on(DependencyResolver::resolve(
        &FailingFetcher,
        &storage.view(),
        storage.artifacts_mut(),
        &[spec],
    ));
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
    assert_eq!(first.jars[0].key, second.jars[0].key);
    assert_eq!(
        storage
            .artifacts()
            .lookup(&second.jars[0].key)
            .unwrap()
            .unwrap(),
        b"remote jar"
    );
}

#[test]
fn digest_mismatch_is_a_warning_and_is_not_published() {
    let mut storage = MemoryStorage::memory(CodeTree::default());
    let fetcher = MockFetcher {
        bytes: b"wrong".to_vec(),
        calls: AtomicUsize::new(0),
    };
    let spec = DependencySpec {
        name: Name::new("remote").unwrap(),
        location: DependencyLocation::External {
            locator: ExternalLocator::new("https://example.invalid/dep.jar"),
            expected: Some(ContentDigest::of(b"expected")),
        },
        recursive: false,
    };
    let resolved = block_on(DependencyResolver::resolve(
        &fetcher,
        &storage.view(),
        storage.artifacts_mut(),
        &[spec],
    ));
    assert!(resolved.jars.is_empty());
    assert_eq!(resolved.warnings.len(), 1);
    assert!(resolved.warnings[0].message.contains("digest mismatch"));
}
