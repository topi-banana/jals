use std::sync::atomic::{AtomicUsize, Ordering};

use jals_classpath::{
    DependencyLocation, DependencyResolver, DependencySpec, ExternalLocator, Fetcher,
};
use jals_exec::block_on_inline;
use jals_storage::{
    CacheKey, CacheNamespace, CodeTree, ContentDigest, Entry, FileKey, MemoryStorage, Name,
};

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
    block_on_inline(async {
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
        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[spec],
        )
        .await;
        assert_eq!(resolved.jars.len(), 1);
        assert!(resolved.warnings.is_empty());
        assert_eq!(
            storage
                .artifacts()
                .lookup(&resolved.jars[0].key)
                .await
                .unwrap()
                .unwrap(),
            b"jar"
        );
    });
}

#[test]
fn artifact_dependency_is_verified_without_fetching_or_republishing() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let key = CacheKey::new(
            CacheNamespace::DependencyJar,
            ContentDigest::of(b"project-graph"),
            ContentDigest::of(b"jar"),
        );
        storage.artifacts_mut().publish(&key, b"jar").await.unwrap();
        let fetcher = MockFetcher {
            bytes: b"wrong".to_vec(),
            calls: AtomicUsize::new(0),
        };
        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[DependencySpec {
                name: Name::new("cached").unwrap(),
                location: DependencyLocation::Artifact(key.clone()),
                recursive: false,
            }],
        )
        .await;

        assert_eq!(fetcher.calls.load(Ordering::Relaxed), 0);
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
        assert_eq!(resolved.jars[0].key, key);
    });
}

#[test]
fn expected_digest_enables_verified_external_cache_hits() {
    block_on_inline(async {
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
        let first = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            std::slice::from_ref(&spec),
        )
        .await;
        assert_eq!(fetcher.calls.load(Ordering::Relaxed), 1);
        let second = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[spec],
        )
        .await;
        assert_eq!(
            fetcher.calls.load(Ordering::Relaxed),
            1,
            "second resolution must hit cache"
        );
        assert_eq!(first.jars[0].key, second.jars[0].key);
    });
}

struct FailingFetcher;

impl Fetcher for FailingFetcher {
    async fn fetch(&self, _locator: &str) -> Result<Vec<u8>, String> {
        Err("network unavailable".to_owned())
    }
}

#[test]
fn digest_less_external_dependency_resolves_from_cache_offline() {
    block_on_inline(async {
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
        let first = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            std::slice::from_ref(&spec),
        )
        .await;
        assert_eq!(fetcher.calls.load(Ordering::Relaxed), 1);
        assert!(first.warnings.is_empty());

        // The second resolution has no network at all; the locator index recovers the cached jar.
        let second = DependencyResolver::resolve(
            &FailingFetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[spec],
        )
        .await;
        assert!(second.warnings.is_empty(), "{:?}", second.warnings);
        assert_eq!(first.jars[0].key, second.jars[0].key);
        assert_eq!(
            storage
                .artifacts()
                .lookup(&second.jars[0].key)
                .await
                .unwrap()
                .unwrap(),
            b"remote jar"
        );
    });
}

#[test]
fn digest_mismatch_is_a_warning_and_is_not_published() {
    block_on_inline(async {
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
        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[spec],
        )
        .await;
        assert!(resolved.jars.is_empty());
        assert_eq!(resolved.warnings.len(), 1);
        assert!(resolved.warnings[0].message.contains("digest mismatch"));
    });
}

/// Two specs sharing one locator must trigger exactly one fetch (deduplicated concurrent pass)
/// and still resolve both, in spec order.
#[test]
fn duplicate_locators_fetch_once_and_resolve_in_spec_order() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = MockFetcher {
            bytes: b"shared jar".to_vec(),
            calls: AtomicUsize::new(0),
        };
        let spec = |name: &str| DependencySpec {
            name: Name::new(name).unwrap(),
            location: DependencyLocation::External {
                locator: ExternalLocator::new("https://example.invalid/shared.jar"),
                expected: None,
            },
            recursive: false,
        };
        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[spec("first"), spec("second")],
        )
        .await;
        assert_eq!(fetcher.calls.load(Ordering::Relaxed), 1);
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
        let names: Vec<_> = resolved.jars.iter().map(|jar| jar.name.as_str()).collect();
        assert_eq!(names, ["first", "second"]);
        assert_eq!(resolved.jars[0].key, resolved.jars[1].key);
    });
}
