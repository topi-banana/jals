use core::future::{Future, ready};

use std::collections::BTreeSet;
use std::str::FromStr as _;
use std::sync::atomic::{AtomicUsize, Ordering};

use jals_classpath::{
    DependencyLocation, DependencyResolver, DependencySpec, ExpectedDigest,
    ExternalArtifactResolver, ExternalArtifactSpec, ExternalLocator, FetchError, Fetcher,
    MappingResolver, MappingSpec, NetworkPolicy, RetrySchedule,
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

    fn retry(&self) -> RetrySchedule {
        RetrySchedule::none()
    }

    fn delay(&self, _: u32) -> impl Future<Output = ()> {
        ready(())
    }

    fn fetch_admitted(
        &self,
        _locator: &str,
        _: &jals_progress::Task,
    ) -> impl Future<Output = Result<Vec<u8>, FetchError>> {
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
        let key = ExternalArtifactResolver::resolve(
            &online,
            storage.artifacts_mut(),
            &spec,
            &jals_progress::Progress::SILENT,
        )
        .await
        .unwrap();
        assert_eq!(online.calls(), 1);
        assert_eq!(key.content(), ContentDigest::of(bytes));

        let offline = MockFetcher::offline(b"wrong");
        let cached = ExternalArtifactResolver::resolve(
            &offline,
            storage.artifacts_mut(),
            &spec,
            &jals_progress::Progress::SILENT,
        )
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
        let error = ExternalArtifactResolver::resolve(
            &fetcher,
            storage.artifacts_mut(),
            &spec,
            &jals_progress::Progress::SILENT,
        )
        .await
        .unwrap_err();
        assert!(error.contains("exceeding the limit"), "{error}");
        assert_eq!(fetcher.calls(), 1);

        let mismatch = ExternalArtifactSpec {
            max_bytes: 1024,
            ..spec
        };
        let error = ExternalArtifactResolver::resolve(
            &fetcher,
            storage.artifacts_mut(),
            &mismatch,
            &jals_progress::Progress::SILENT,
        )
        .await
        .unwrap_err();
        assert!(error.contains("digest mismatch"), "{error}");

        // The whole message, not a substring: this is the one diagnostic that has to say the cache
        // was already tried, and it reaches a destination (`BuildTaskRunError::Node`) that renders
        // it with no origin beside it, so it names its own locator.
        let offline = MockFetcher::offline(b"oversized");
        let error = ExternalArtifactResolver::resolve(
            &offline,
            storage.artifacts_mut(),
            &mismatch,
            &jals_progress::Progress::SILENT,
        )
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
            &jals_progress::Progress::SILENT,
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
            &jals_progress::Progress::SILENT,
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
            &jals_progress::Progress::SILENT,
        )
        .await;
        assert_eq!(fetcher.calls(), 1);
        let second = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[spec],
            &jals_progress::Progress::SILENT,
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

    fn retry(&self) -> RetrySchedule {
        RetrySchedule::none()
    }

    fn delay(&self, _: u32) -> impl Future<Output = ()> {
        ready(())
    }

    fn fetch_admitted(
        &self,
        locator: &str,
        _: &jals_progress::Task,
    ) -> impl Future<Output = Result<Vec<u8>, FetchError>> {
        ready(Self::refuse(locator))
    }
}

impl OfflineFetcher {
    /// Diverges: locator-index recovery must answer before anything reaches the fetch seam.
    fn refuse(locator: &str) -> Result<Vec<u8>, FetchError> {
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
            &jals_progress::Progress::SILENT,
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
            &jals_progress::Progress::SILENT,
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
            &jals_progress::Progress::SILENT,
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
            &jals_progress::Progress::SILENT,
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
            &jals_progress::Progress::SILENT,
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
            &jals_progress::Progress::SILENT,
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
        let warning = MappingResolver::text(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &spec,
            &jals_progress::Progress::SILENT,
        )
        .await
        .unwrap_err();

        assert_eq!(fetcher.calls(), 0);
        assert!(warning.message.contains("not available"), "{warning}");
    });
}

/// A recording sink: what a resolution said it was doing, in the order it said it.
///
/// `Sink` is `Send + Sync` because a fan-out worker may emit through one; a `Mutex` is what that
/// costs here and the whole implementation.
#[derive(Default)]
struct Recorder {
    events: std::sync::Mutex<Vec<String>>,
}

impl Recorder {
    /// Every `Started` unit, rendered as `<activity> <subject>`.
    fn started(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

impl jals_progress::Sink for Recorder {
    fn emit(&self, event: &jals_progress::Event) {
        if let jals_progress::Event::Started { unit, .. } = event {
            self.events
                .lock()
                .unwrap()
                .push(format!("{:?} {}", unit.activity, unit.describe()));
        }
    }
}

/// A resolution reports itself, and the downloads it owns report inside it.
///
/// The pass is one unit rather than none because a run that says nothing until the first byte
/// arrives looks hung on a slow link; and it is one unit rather than one *per spec* because the
/// per-locator `Fetch` units already say that, deduplicated the way the fetch itself is.
#[test]
fn a_resolution_reports_the_pass_and_the_downloads_inside_it() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = MockFetcher::online(b"remote jar");
        let recorder = std::sync::Arc::new(Recorder::default());
        let progress = jals_progress::Progress::to(std::sync::Arc::clone(&recorder) as _);
        let spec = DependencySpec {
            name: Name::new("remote").unwrap(),
            location: DependencyLocation::External {
                locator: ExternalLocator::new("https://example.invalid/dep.jar"),
                expected: Some(ContentDigest::of(b"remote jar")),
            },
            recursive: false,
            remap: None,
        };

        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            core::slice::from_ref(&spec),
            &progress,
        )
        .await;
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);

        assert_eq!(
            recorder.started(),
            vec![
                String::from("Resolve 1 dependency"),
                // The subject is what the caller wrote in `[dependencies]`, not the URL: a locator
                // is how the bytes are reached and the name is what a person asked for.
                String::from("Fetch remote"),
            ]
        );
    });
}

/// Nothing declared is nothing to report — a `Resolving` line above a project with no
/// `[dependencies]` would be a line about work that did not happen.
#[test]
fn a_resolution_with_nothing_to_resolve_reports_nothing() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = MockFetcher::online(b"");
        let recorder = std::sync::Arc::new(Recorder::default());
        let progress = jals_progress::Progress::to(std::sync::Arc::clone(&recorder) as _);

        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[],
            &progress,
        )
        .await;

        assert!(resolved.jars.is_empty() && resolved.warnings.is_empty());
        assert!(recorder.started().is_empty(), "{:?}", recorder.started());
    });
}
