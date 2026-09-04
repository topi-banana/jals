//! The fetch retry: which failures are attempted again, how many times, and what never is.
//!
//! Everything here goes through the two public resolvers, because `Fetch` — the door that owns the
//! loop — is `pub(crate)`. That is the point rather than an inconvenience: the claims are about
//! what a *resolution* does, and a test that reached past the resolvers would be asserting against
//! a seam no caller uses.

use core::cell::{Cell, RefCell};
use core::future::{Future, ready};

use jals_classpath::{
    DependencyLocation, DependencyResolver, DependencySpec, ExpectedDigest,
    ExternalArtifactResolver, ExternalArtifactSpec, ExternalLocator, FetchError, Fetcher,
    NetworkPolicy, RetrySchedule,
};
use jals_exec::block_on_inline;
use jals_storage::{CacheNamespace, CodeTree, ContentDigest, MemoryStorage, Name};

const LOCATOR: &str = "https://example.invalid/artifact.jar";
const BYTES: &[u8] = b"the artifact";

/// A fetcher whose first `failures` attempts fail, and which records every wait it was asked for.
///
/// The recorded waits are what makes the loop observable: a test that only counted attempts could
/// not tell a backoff from a busy retry, which is the one thing a delay-less implementation would
/// get wrong.
struct FlakyFetcher {
    failures: usize,
    transient: bool,
    network: NetworkPolicy,
    retry: RetrySchedule,
    calls: Cell<usize>,
    waits: RefCell<Vec<u32>>,
}

impl FlakyFetcher {
    /// `failures` transient failures, then the bytes, under `retries` further attempts.
    const fn transient(failures: usize, retries: u32) -> Self {
        Self {
            failures,
            transient: true,
            network: NetworkPolicy::Online,
            retry: RetrySchedule::new(retries),
            calls: Cell::new(0),
            waits: RefCell::new(Vec::new()),
        }
    }

    /// A failure no number of attempts changes.
    fn permanent(retries: u32) -> Self {
        Self {
            transient: false,
            ..Self::transient(usize::MAX, retries)
        }
    }

    /// A capability that refuses the network, and would fail transiently if it did not.
    fn offline(retries: u32) -> Self {
        Self {
            network: NetworkPolicy::Offline,
            ..Self::transient(usize::MAX, retries)
        }
    }

    const fn calls(&self) -> usize {
        self.calls.get()
    }

    fn waits(&self) -> Vec<u32> {
        self.waits.borrow().clone()
    }

    /// What attempt number `attempt` is answered with.
    fn answer(&self, attempt: usize, locator: &str) -> Result<Vec<u8>, FetchError> {
        if attempt > self.failures {
            return Ok(BYTES.to_vec());
        }
        let message = format!("HTTP status server error (522 <none>) for url ({locator})");
        if self.transient {
            Err(FetchError::transient(message))
        } else {
            Err(FetchError::permanent(message))
        }
    }
}

impl Fetcher for FlakyFetcher {
    fn network(&self) -> NetworkPolicy {
        self.network
    }

    fn retry(&self) -> RetrySchedule {
        self.retry
    }

    fn delay(&self, millis: u32) -> impl Future<Output = ()> {
        self.waits.borrow_mut().push(millis);
        ready(())
    }

    fn fetch_admitted(
        &self,
        locator: &str,
        _: &jals_progress::Task,
    ) -> impl Future<Output = Result<Vec<u8>, FetchError>> {
        let attempt = self.calls.get() + 1;
        self.calls.set(attempt);
        ready(self.answer(attempt, locator))
    }
}

fn spec() -> ExternalArtifactSpec {
    ExternalArtifactSpec {
        locator: ExternalLocator::new(LOCATOR),
        expected: ExpectedDigest::Sha256(ContentDigest::of(BYTES)),
        max_bytes: 1024,
        namespace: CacheNamespace::BuildTaskArtifact,
    }
}

#[test]
fn a_transient_failure_is_retried_until_it_succeeds() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = FlakyFetcher::transient(2, RetrySchedule::DEFAULT_RETRIES);
        let key = ExternalArtifactResolver::resolve(
            &fetcher,
            storage.artifacts_mut(),
            &spec(),
            &jals_progress::Progress::SILENT,
        )
        .await
        .expect("the third attempt serves the bytes");

        assert_eq!(key.content(), ContentDigest::of(BYTES));
        assert_eq!(fetcher.calls(), 3, "two failures then one success");
        let waits = fetcher.waits();
        assert_eq!(waits.len(), 2, "one wait per retry: {waits:?}");
        assert!(
            waits[0] < waits[1],
            "the backoff must grow between attempts: {waits:?}"
        );
        assert!(waits.iter().all(|wait| *wait > 0), "{waits:?}");
    });
}

#[test]
fn a_permanent_failure_is_attempted_once_and_reads_unchanged() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = FlakyFetcher::permanent(RetrySchedule::DEFAULT_RETRIES);
        let error = ExternalArtifactResolver::resolve(
            &fetcher,
            storage.artifacts_mut(),
            &spec(),
            &jals_progress::Progress::SILENT,
        )
        .await
        .unwrap_err();

        assert_eq!(fetcher.calls(), 1, "a permanent failure is not retried");
        assert!(fetcher.waits().is_empty(), "nothing to wait for");
        // A single attempt renders exactly as it did before there was a loop.
        assert_eq!(
            error,
            format!("HTTP status server error (522 <none>) for url ({LOCATOR})")
        );
    });
}

#[test]
fn an_offline_refusal_is_never_attempted() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = FlakyFetcher::offline(RetrySchedule::DEFAULT_RETRIES);
        let error = ExternalArtifactResolver::resolve(
            &fetcher,
            storage.artifacts_mut(),
            &spec(),
            &jals_progress::Progress::SILENT,
        )
        .await
        .unwrap_err();

        // The gate is outside the loop, so the refusal is not a failure the loop can classify.
        assert_eq!(fetcher.calls(), 0);
        assert!(fetcher.waits().is_empty());
        assert!(
            NetworkPolicy::refused_offline(&error) || error.contains("while offline"),
            "{error}"
        );
    });
}

#[test]
fn an_exhausted_schedule_says_how_many_attempts_it_cost() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = FlakyFetcher::transient(usize::MAX, 2);
        let error = ExternalArtifactResolver::resolve(
            &fetcher,
            storage.artifacts_mut(),
            &spec(),
            &jals_progress::Progress::SILENT,
        )
        .await
        .unwrap_err();

        assert_eq!(fetcher.calls(), 3, "the first attempt plus two retries");
        assert_eq!(fetcher.waits().len(), 2);
        assert!(
            error.ends_with("(after 3 attempts)"),
            "a failure that cost waiting has to say so: {error}"
        );
    });
}

#[test]
fn a_schedule_without_retries_attempts_once_however_transient_the_failure() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = FlakyFetcher::transient(usize::MAX, 0);
        let error = ExternalArtifactResolver::resolve(
            &fetcher,
            storage.artifacts_mut(),
            &spec(),
            &jals_progress::Progress::SILENT,
        )
        .await
        .unwrap_err();

        assert_eq!(fetcher.calls(), 1);
        assert!(fetcher.waits().is_empty());
        assert!(!error.contains("attempts"), "{error}");
    });
}

/// The other door: `DependencyResolver` fetches its locators concurrently, so the retry has to
/// live inside the per-locator future rather than around the fan-out.
#[test]
fn a_dependency_jar_is_retried_inside_the_concurrent_fan_out() {
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = FlakyFetcher::transient(1, RetrySchedule::DEFAULT_RETRIES);
        let resolved = DependencyResolver::resolve(
            &fetcher,
            &storage.view(),
            storage.artifacts_mut(),
            &[DependencySpec {
                name: Name::new("remote").unwrap(),
                location: DependencyLocation::External {
                    locator: ExternalLocator::new(LOCATOR),
                    expected: Some(ContentDigest::of(BYTES)),
                },
                recursive: false,
                remap: None,
            }],
            &jals_progress::Progress::SILENT,
        )
        .await;

        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
        assert_eq!(resolved.jars.len(), 1);
        assert_eq!(fetcher.calls(), 2, "one failure then one success");
        assert_eq!(fetcher.waits().len(), 1);
    });
}
