use core::future::{Future, ready};

use std::collections::BTreeSet;
use std::str::FromStr as _;
use std::sync::atomic::{AtomicUsize, Ordering};

use jals_classpath::{
    DependencyLocation, DependencyResolver, DependencySpec, ExpectedDigest,
    ExternalArtifactResolver, ExternalArtifactSpec, ExternalLocator, Fetcher, MappingResolver,
    MappingSpec, NetworkPolicy,
};
use jals_exec::block_on_inline;
use jals_storage::{
    CacheKey, CacheNamespace, CodeTree, ContentDigest, Entry, FileKey, MemoryStorage, Name,
};
use sha1::{Digest as _, Sha1};

struct MockFetcher {
    bytes: Vec<u8>,
    calls: AtomicUsize,
    network: NetworkPolicy,
}

impl MockFetcher {
    fn online(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            calls: AtomicUsize::new(0),
            network: NetworkPolicy::Online,
        }
    }

    fn offline(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            calls: AtomicUsize::new(0),
            network: NetworkPolicy::Offline,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Fetcher for MockFetcher {
    fn network(&self) -> NetworkPolicy {
        self.network
    }

    fn fetch_admitted(&self, _locator: &str) -> impl Future<Output = Result<Vec<u8>, String>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        ready(Ok(self.bytes.clone()))
    }
}

#[test]
fn external_artifacts_verify_sha1_and_reuse_the_sha256_cache_offline() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let bytes = b"verified artifact";
        let expected = ExpectedDigest::Sha1(Sha1::digest(bytes).into());
        let spec = ExternalArtifactSpec {
            locator: ExternalLocator::new("https://example.invalid/artifact.jar"),
            expected,
            max_bytes: 1024,
            namespace: CacheNamespace::BuildTaskArtifact,
        };
        let online = MockFetcher::online(bytes);
        let key = ExternalArtifactResolver::resolve(&online, storage.artifacts_mut(), &spec)
            .await
            .unwrap();
        assert_eq!(online.calls(), 1);
        assert_eq!(key.content(), ContentDigest::of(bytes));

        let offline = MockFetcher::offline(b"wrong");
        let cached = ExternalArtifactResolver::resolve(&offline, storage.artifacts_mut(), &spec)
            .await
            .unwrap();
        assert_eq!(cached, key);
        assert_eq!(offline.calls(), 0);
    });
}

#[test]
fn external_artifacts_reject_oversize_and_digest_mismatch_without_indexing() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let locator = ExternalLocator::new("https://example.invalid/artifact.jar");
        let spec = ExternalArtifactSpec {
            locator: locator.clone(),
            expected: ExpectedDigest::Sha256(ContentDigest::of(b"expected")),
            max_bytes: 4,
            namespace: CacheNamespace::BuildTaskArtifact,
        };
        let fetcher = MockFetcher::online(b"oversized");
        let error = ExternalArtifactResolver::resolve(&fetcher, storage.artifacts_mut(), &spec)
            .await
            .unwrap_err();
        assert!(error.contains("exceeding the limit"), "{error}");
        assert_eq!(fetcher.calls(), 1);

        let mismatch = ExternalArtifactSpec {
            max_bytes: 1024,
            ..spec
        };
        let error = ExternalArtifactResolver::resolve(&fetcher, storage.artifacts_mut(), &mismatch)
            .await
            .unwrap_err();
        assert!(error.contains("digest mismatch"), "{error}");

        // The whole message, not a substring: this is the one diagnostic that has to say the cache
        // was already tried, and it reaches a destination (`BuildTaskRunError::Node`) that renders
        // it with no origin beside it, so it names its own locator.
        let offline = MockFetcher::offline(b"oversized");
        let error = ExternalArtifactResolver::resolve(&offline, storage.artifacts_mut(), &mismatch)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            "external artifact `https://example.invalid/artifact.jar` is not available in the \
             verified cache while offline"
        );
        assert_eq!(offline.calls(), 0);
    });
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
        let fetcher = MockFetcher::online(b"");
        let spec = DependencySpec {
            name: Name::new("dep").unwrap(),
            location: DependencyLocation::Project(FileKey::parse("lib/dep.jar").unwrap()),
            recursive: false,
            remap: None,
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
        let fetcher = MockFetcher::online(b"wrong");
        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[DependencySpec {
                name: Name::new("cached").unwrap(),
                location: DependencyLocation::Artifact(key.clone()),
                recursive: false,
                remap: None,
            }],
        )
        .await;

        assert_eq!(fetcher.calls(), 0);
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
        assert_eq!(resolved.jars[0].key, key);
    });
}

#[test]
fn expected_digest_enables_verified_external_cache_hits() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = MockFetcher::online(b"remote jar");
        let spec = DependencySpec {
            name: Name::new("remote").unwrap(),
            location: DependencyLocation::External {
                locator: ExternalLocator::new("https://example.invalid/dep.jar"),
                expected: Some(ContentDigest::of(b"remote jar")),
            },
            recursive: false,
            remap: None,
        };
        let first = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            std::slice::from_ref(&spec),
        )
        .await;
        assert_eq!(fetcher.calls(), 1);
        let second = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[spec],
        )
        .await;
        assert_eq!(fetcher.calls(), 1, "second resolution must hit cache");
        assert_eq!(first.jars[0].key, second.jars[0].key);
    });
}

/// A capability that must never be asked for bytes.
///
/// It panics rather than returning an error: the point of the test below is that locator-index
/// recovery happens *before* anything reaches the fetch seam. An `Err` would prove only that a
/// failed fetch is survivable, which is a much weaker claim.
struct OfflineFetcher;

impl Fetcher for OfflineFetcher {
    fn network(&self) -> NetworkPolicy {
        NetworkPolicy::Offline
    }

    fn fetch_admitted(&self, locator: &str) -> impl Future<Output = Result<Vec<u8>, String>> {
        ready(Self::refuse(locator))
    }
}

impl OfflineFetcher {
    /// Diverges: locator-index recovery must answer before anything reaches the fetch seam.
    fn refuse(locator: &str) -> Result<Vec<u8>, String> {
        panic!("resolution must not fetch, but asked for `{locator}`")
    }
}

#[test]
fn digest_less_external_dependency_resolves_from_cache_offline() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = MockFetcher::online(b"remote jar");
        let spec = DependencySpec {
            name: Name::new("remote").unwrap(),
            location: DependencyLocation::External {
                locator: ExternalLocator::new("https://example.invalid/dep.jar"),
                expected: None,
            },
            recursive: false,
            remap: None,
        };
        let first = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            std::slice::from_ref(&spec),
        )
        .await;
        assert_eq!(fetcher.calls(), 1);
        assert!(first.warnings.is_empty());

        // The second resolution has no network at all; the locator index recovers the cached jar.
        let second = DependencyResolver::resolve(
            &OfflineFetcher,
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
        let fetcher = MockFetcher::online(b"wrong");
        let spec = DependencySpec {
            name: Name::new("remote").unwrap(),
            location: DependencyLocation::External {
                locator: ExternalLocator::new("https://example.invalid/dep.jar"),
                expected: Some(ContentDigest::of(b"expected")),
            },
            recursive: false,
            remap: None,
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
        let fetcher = MockFetcher::online(b"shared jar");
        let spec = |name: &str| DependencySpec {
            name: Name::new(name).unwrap(),
            location: DependencyLocation::External {
                locator: ExternalLocator::new("https://example.invalid/shared.jar"),
                expected: None,
            },
            recursive: false,
            remap: None,
        };
        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[spec("first"), spec("second")],
        )
        .await;
        assert_eq!(fetcher.calls(), 1);
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
        let names: Vec<_> = resolved.jars.iter().map(|jar| jar.name.as_str()).collect();
        assert_eq!(names, ["first", "second"]);
        assert_eq!(resolved.jars[0].key, resolved.jars[1].key);
    });
}

/// The regression this whole seam exists for.
///
/// `DependencyResolver` never took a `NetworkPolicy`, so an uncached `[dependencies]` jar was
/// fetched under `jals build --offline`, under `jals lint`, and when the language server opened a
/// folder. The policy rides the capability now, so refusing is the capability's answer and no
/// caller has to remember to ask.
#[test]
fn offline_does_not_fetch_an_uncached_external_dependency_jar() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let locator = ExternalLocator::new("https://example.invalid/dep.jar");
        let fetcher = MockFetcher::offline(b"must not be served");
        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[DependencySpec {
                name: Name::new("remote").unwrap(),
                location: DependencyLocation::External {
                    locator: locator.clone(),
                    expected: None,
                },
                recursive: false,
                remap: None,
            }],
        )
        .await;

        assert_eq!(fetcher.calls(), 0);
        assert!(resolved.jars.is_empty(), "{:?}", resolved.jars);
        assert_eq!(resolved.warnings.len(), 1, "{:?}", resolved.warnings);
        // Rendered whole: the subject is the origin, and the refusal is distinguishable from a
        // fetch that was attempted and failed — `jals-cli`'s end-to-end test rests on that.
        assert_eq!(
            resolved.warnings[0].to_string(),
            "`https://example.invalid/dep.jar`: dependency `remote` fetch failed: \
             not fetched while offline"
        );
    });
}

/// The twin nobody thinks of, and the only thing pinning `is_remote` over `is_url`.
///
/// `NativeProjectPlan::classify` lowers a `jar = "../libs/x.jar"` that resolves outside the project
/// root to an *external* locator carrying a host path, and `file://` reaches this seam too. Neither
/// is the network, so an offline capability still has to serve both — gating on `is_url` would
/// break a build that never wanted the network at all.
#[test]
fn offline_still_reads_a_non_network_external_locator() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = MockFetcher::offline(b"local jar");
        let spec = |name: &str, locator: &str| DependencySpec {
            name: Name::new(name).unwrap(),
            location: DependencyLocation::External {
                locator: ExternalLocator::new(locator),
                expected: None,
            },
            recursive: false,
            remap: None,
        };
        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[
                spec("relative", "../libs/local.jar"),
                spec("file-url", "file:///opt/local.jar"),
            ],
        )
        .await;

        assert_eq!(fetcher.calls(), 2);
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
        assert_eq!(resolved.jars.len(), 2);
    });
}

/// `MappingResolver::text` hardcoded `NetworkPolicy::Online`, which was the one place its own doc
/// comment — "a host with no network hands over one that refuses" — was untrue.
#[test]
fn offline_does_not_fetch_an_external_mapping_text() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let manifest = jals_config::Manifest::from_str(
            "[package]\nname = \"fixture\"\n\n\
             [mappings.names]\nurl = \"https://example.invalid/names.txt\"\n\
             sha1 = \"da39a3ee5e6b4b0d3255bfef95601890afd80709\"\nmax-bytes = 16777216\n",
        )
        .unwrap();
        let mut warnings = Vec::new();
        let spec = MappingSpec::lower_active(&manifest, "names", &BTreeSet::new(), &mut warnings)
            .unwrap()
            .expect("the entry lowers");
        assert!(warnings.is_empty(), "{warnings:?}");

        let fetcher = MockFetcher::offline(b"must not be served");
        let warning =
            MappingResolver::text(&fetcher, &storage.view(), storage.artifacts_mut(), &spec)
                .await
                .unwrap_err();

        assert_eq!(fetcher.calls(), 0);
        assert!(warning.message.contains("not available"), "{warning}");
    });
}
