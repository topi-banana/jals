//! Capabilities which cannot be represented by project storage.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ExternalLocator;

/// Whether a fetch capability may reach the network.
///
/// Part of the capability rather than a value beside it. A host that must not fetch hands over a
/// [`Fetcher`] that says so, and every fetch this crate performs goes through `Fetch`, which
/// asks. Holding the capability while being unaware of the policy is what the previous
/// arrangement allowed, and it is what let `jals lint` and the language server pass `Offline` to
/// some layers while handing a freshly built online fetcher to others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    Online,
    Offline,
}

impl NetworkPolicy {
    /// How a refusal by this policy reads.
    ///
    /// Public so a host that wants to add remediation advice — the language server tells the user
    /// `jals build` populates the cache — can recognize one without copying the sentence. One
    /// owner, so the wording and the thing that matches it cannot drift apart.
    pub const OFFLINE_REFUSAL: &'static str = "not fetched while offline";

    /// Whether `message` reports a refusal by [`Offline`](Self::Offline).
    ///
    /// The other half of [`OFFLINE_REFUSAL`](Self::OFFLINE_REFUSAL)'s promise: naming the sentence
    /// only prevents drift if what recognizes it lives beside it, and a caller writing the
    /// substring test itself is the copy that constant exists to avoid.
    ///
    /// A producer wraps the refusal in its own sentence, so this is a substring test rather than an
    /// equality one. It takes the text rather than a [`Warning`] because a caller scanning a
    /// project's diagnostics has more than one kind of message in front of it, and a refusal reads
    /// the same in all of them.
    pub fn refused_offline(message: &str) -> bool {
        message.contains(Self::OFFLINE_REFUSAL)
    }

    /// The policy a host's `--offline`-style flag selects. Named so the conversion is spelled once
    /// rather than as an `if` at each construction site.
    pub const fn when_offline(offline: bool) -> Self {
        if offline { Self::Offline } else { Self::Online }
    }

    /// Whether this policy permits obtaining `locator`'s bytes.
    ///
    /// Only *network* locators are refused. This seam also carries `file://` and plain host paths
    /// — `NativeProjectPlan::classify` lowers a `jar = "../libs/x.jar"` resolving outside the
    /// project root to exactly such an external locator — and refusing those offline would break a
    /// build that never wanted the network at all. That is why the discriminator is
    /// [`ExternalLocator::is_remote`] and never `is_url`, which also matches `file://`.
    ///
    /// `pub(crate)`: the gate belongs to this crate. A host chooses the policy and answers
    /// [`Fetcher::network`] with it; deciding what that policy admits is not its business, and a
    /// second caller of this would be a second gate.
    pub(crate) fn admits(self, locator: &ExternalLocator) -> bool {
        match self {
            Self::Online => true,
            Self::Offline => !ExternalLocator::is_remote(locator.as_str()),
        }
    }
}

/// Why a fetch failed, and whether trying again could plausibly succeed.
///
/// The one place a fetch failure is still *typed*. An implementor is where the distinction is
/// knowable — the native adapter is holding a `reqwest::Error` with its status and its
/// timeout/connect flags — and it is gone one line later if that error becomes a `String` there.
/// So the classification is the implementor's answer and the decision built on it is [`Fetch`]'s:
/// the adapter says what happened, the door says what to do about it.
///
/// It stops at [`Fetch`]. Everything above still receives a `String`, which is why a `Warning`, a
/// `BuildTaskRunError`, and [`NetworkPolicy::refused_offline`] all read exactly as they did.
#[derive(Debug, Clone)]
pub struct FetchError {
    message: String,
    transient: bool,
}

impl FetchError {
    /// A failure another attempt could plausibly fix: a timeout, a refused connection, a 5xx.
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
        }
    }

    /// A failure that will read the same however many times it is asked: a 404, a body over the
    /// caller's ceiling, an unreadable file.
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
        }
    }

    const fn is_transient(&self) -> bool {
        self.transient
    }

    fn into_message(self) -> String {
        self.message
    }
}

impl core::fmt::Display for FetchError {
    /// The message verbatim. Every boundary above [`Fetch`] flattens a failure to text, and one
    /// that rendered a severity or a classification beside it would be one more string for
    /// [`NetworkPolicy::refused_offline`]'s substring test to have to survive.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}

/// How many further attempts a transient fetch failure is given, and how long to wait between
/// them.
///
/// Part of the [`Fetcher`] for the same reason [`NetworkPolicy`] is: a host constructs the
/// capability, so a host is what states the policy, and every step handed that capability
/// inherits it. A value travelling beside it is how two layers end up disagreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrySchedule {
    retries: u32,
}

impl RetrySchedule {
    /// What a host picks with no reason to pick otherwise: three further attempts, four in total.
    pub const DEFAULT_RETRIES: u32 = 3;

    /// The first wait. Doubling is too slow to outlast an origin that is shedding load and too
    /// eager to be polite to one that is not; tripling from half a second gives 0.5s / 1.5s /
    /// 4.5s, so the default schedule costs at most ~6.5s of waiting per locator.
    const BASE_MILLIS: u32 = 500;
    const MULTIPLIER: u32 = 3;
    /// No single wait exceeds this, however many retries a host asked for.
    const CEILING_MILLIS: u32 = 10_000;
    /// Jitter is added on top of a wait and never subtracted from it, so a jittered wait is never
    /// shorter than the unjittered one. It is smaller than the gap between two consecutive waits,
    /// so the schedule still grows attempt over attempt until the ceiling flattens it.
    const JITTER_MILLIS: u32 = 1_000;

    /// `retries` further attempts after the first one fails.
    pub const fn new(retries: u32) -> Self {
        Self { retries }
    }

    /// One attempt and no more.
    ///
    /// What a host that cannot fetch at all states — `jals lint` and the language server are
    /// [`NetworkPolicy::Offline`], so their refusal comes before any attempt — and what a test
    /// double states to assert that a call happened exactly once.
    pub const fn none() -> Self {
        Self { retries: 0 }
    }

    const fn retries(self) -> u32 {
        self.retries
    }

    /// How long to wait before the attempt after the one indexed `attempt` (0-based).
    ///
    /// An associated function rather than a method: the wait depends on which attempt just failed
    /// and on nothing a host configured. `retries` says *how many* attempts there are, and the two
    /// questions stay separate.
    ///
    /// The jitter is derived from `locator` and `attempt` rather than drawn from a generator:
    /// portable code here has no clock and the workspace has no RNG dependency, and — the reason
    /// that matters — `DependencyResolver::resolve` fetches every locator concurrently, so a
    /// shared schedule would send the whole fan-out back at the origin in one wave. Deriving the
    /// jitter per locator spreads them, and being deterministic is what lets the schedule be
    /// asserted in a test instead of sampled.
    fn backoff_millis(attempt: u32, locator: &str) -> u32 {
        let growth = Self::MULTIPLIER.saturating_pow(attempt);
        let wait = Self::BASE_MILLIS
            .saturating_mul(growth)
            .min(Self::CEILING_MILLIS);
        wait.saturating_add(Self::jitter_millis(attempt, locator))
    }

    /// FNV-1a over the locator and the attempt, folded into `0..JITTER_MILLIS`.
    fn jitter_millis(attempt: u32, locator: &str) -> u32 {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = OFFSET;
        for byte in locator.as_bytes().iter().chain(&attempt.to_le_bytes()) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        u32::try_from(hash % u64::from(Self::JITTER_MILLIS)).unwrap_or(0)
    }
}

/// Fetch bytes from an external locator.
///
/// Project-relative files are never passed through this seam. They are read from a
/// [`jals_storage::ProjectView`]; only genuinely external content (normally HTTP) is fetched.
///
/// An implementor answers [`network`](Self::network) and [`retry`](Self::retry), supplies a
/// [`delay`](Self::delay), and supplies the two `_admitted` methods. It never sees a locator its
/// own policy refused: this crate's own `Fetch` is the only caller, and it asks first.
#[allow(async_fn_in_trait)]
pub trait Fetcher {
    /// Whether this capability may reach the network.
    ///
    /// Deliberately without a default. A host constructs the fetcher, so a host is what answers;
    /// a default would let one be built without the question ever being put.
    fn network(&self) -> NetworkPolicy;

    /// How many further attempts a transient failure is given.
    ///
    /// Without a default for the same reason as [`network`](Self::network), and paired with
    /// [`delay`](Self::delay) because a schedule an implementor cannot wait on is a busy loop.
    fn retry(&self) -> RetrySchedule;

    /// Wait `millis` milliseconds before the next attempt.
    ///
    /// The host's, because waiting is a runtime capability and this trait is portable: the native
    /// adapter reaches `jals_exec::tokio_rt::sleep_millis`, the browser one a `TimeoutFuture`, and
    /// a test double records the number and returns immediately.
    async fn delay(&self, millis: u32);

    /// Fetch `locator`, returning a diagnostic-ready failure.
    ///
    /// **Precondition:** the network gate has already admitted `locator`. This crate reaches it
    /// through `Fetch::bytes` and nothing else may call it directly.
    ///
    /// Whether the returned [`FetchError`] is transient is the implementor's judgement and is
    /// where a retry decision is made possible — see [`FetchError`].
    async fn fetch_admitted(&self, locator: &str) -> Result<Vec<u8>, FetchError>;

    /// Fetch at most `max_bytes`, rejecting an oversized result. Carries the same precondition as
    /// [`fetch_admitted`](Self::fetch_admitted).
    ///
    /// The default buffers the whole body and checks afterwards. An implementor that can refuse
    /// before reading should override it, and both real ones do.
    async fn fetch_bounded_admitted(
        &self,
        locator: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, FetchError> {
        let bytes = self.fetch_admitted(locator).await?;
        if bytes.len() > max_bytes {
            return Err(FetchError::permanent(format!(
                "response has {} bytes, exceeding the limit of {max_bytes}",
                bytes.len()
            )));
        }
        Ok(bytes)
    }
}

/// Which of the two `_admitted` methods one attempt calls.
///
/// The retry loop is written once and this is what lets it be: the two entry points differ only
/// in which call they make, and a second copy of the loop is a second place a classification or a
/// wait can be got wrong.
#[derive(Clone, Copy)]
enum Request {
    Full,
    Bounded(usize),
}

/// The crate's only door onto a [`Fetcher`]: apply the capability's own [`NetworkPolicy`], then
/// call it, retrying while the capability's own [`RetrySchedule`] still allows one and the
/// failure says another attempt could succeed.
///
/// Not a provided method on the trait. Rust has no final trait method, so a gate written as one is
/// advisory — an implementor overrides it and the policy is gone. Nothing outside this crate calls
/// a `Fetcher` at all, so a `pub(crate)` namespace really is the only path a fetch can take.
pub(crate) struct Fetch;

impl Fetch {
    /// Fetch `locator` in full.
    pub(crate) async fn bytes<F: Fetcher>(
        fetcher: &F,
        locator: &ExternalLocator,
    ) -> Result<Vec<u8>, String> {
        Self::run(fetcher, locator, Request::Full).await
    }

    /// Fetch `locator` under a byte ceiling.
    pub(crate) async fn bounded<F: Fetcher>(
        fetcher: &F,
        locator: &ExternalLocator,
        max_bytes: usize,
    ) -> Result<Vec<u8>, String> {
        Self::run(fetcher, locator, Request::Bounded(max_bytes)).await
    }

    /// Apply the capability's policy to one locator.
    ///
    /// The refusal names no locator: every caller attributes it to a `WarningOrigin::External` or
    /// restates the locator itself, and a warning carries its subject in its origin, never twice.
    ///
    /// `pub(crate)` because not every acquisition this crate performs is a byte fetch through the
    /// [`Fetcher`]: a `git` dependency is cloned by a subprocess. The bytes do not come through the
    /// capability, but **the network access does**, so the same gate has to answer — and it answers
    /// here rather than at that call site, because a second acquisition path deciding for itself is
    /// how a second policy grows.
    pub(crate) fn admit<F: Fetcher>(fetcher: &F, locator: &ExternalLocator) -> Result<(), String> {
        if fetcher.network().admits(locator) {
            return Ok(());
        }
        Err(NetworkPolicy::OFFLINE_REFUSAL.to_owned())
    }

    /// Gate once, then attempt until the schedule or the failure says stop.
    ///
    /// [`admit`](Self::admit) is called **outside** the loop, and that placement is the whole
    /// guarantee that an offline refusal is never retried: it is not a failure the loop can see,
    /// so no classification of it has to be got right.
    async fn run<F: Fetcher>(
        fetcher: &F,
        locator: &ExternalLocator,
        request: Request,
    ) -> Result<Vec<u8>, String> {
        Self::admit(fetcher, locator)?;
        let schedule = fetcher.retry();
        let mut attempt = 0;
        loop {
            match Self::attempt(fetcher, locator.as_str(), request).await {
                Ok(bytes) => return Ok(bytes),
                Err(error) if error.is_transient() && attempt < schedule.retries() => {
                    fetcher
                        .delay(RetrySchedule::backoff_millis(attempt, locator.as_str()))
                        .await;
                    attempt += 1;
                }
                Err(error) => return Err(Self::render(error, attempt + 1)),
            }
        }
    }

    /// One attempt, in whichever of the two shapes the caller asked for.
    async fn attempt<F: Fetcher>(
        fetcher: &F,
        locator: &str,
        request: Request,
    ) -> Result<Vec<u8>, FetchError> {
        match request {
            Request::Full => fetcher.fetch_admitted(locator).await,
            Request::Bounded(max_bytes) => fetcher.fetch_bounded_admitted(locator, max_bytes).await,
        }
    }

    /// The message a caller above this door receives.
    ///
    /// A single attempt renders byte-identically to what it rendered before there was a loop —
    /// every permanent failure, the oversize refusals and the offline one included, reaches here
    /// with `attempts == 1`. Only a failure that actually cost waiting says so, because that is
    /// the case where the elapsed time is otherwise unexplained.
    fn render(error: FetchError, attempts: u32) -> String {
        let message = error.into_message();
        if attempts <= 1 {
            return message;
        }
        format!("{message} (after {attempts} attempts)")
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{ExternalLocator, NetworkPolicy, RetrySchedule};

    const LOCATOR: &str = "https://example.invalid/a.jar";

    #[test]
    fn the_backoff_grows_and_stays_under_the_ceiling() {
        let waits: Vec<u32> = (0..8)
            .map(|attempt| RetrySchedule::backoff_millis(attempt, LOCATOR))
            .collect();
        // Growth holds until the ceiling flattens it; past that the waits differ only by jitter,
        // which is the point of having a ceiling at all.
        let growing = &waits[..4];
        assert!(
            growing.windows(2).all(|pair| pair[0] < pair[1]),
            "waits must grow while under the ceiling: {waits:?}"
        );
        let ceiling = RetrySchedule::CEILING_MILLIS + RetrySchedule::JITTER_MILLIS;
        assert!(
            waits.iter().all(|wait| *wait <= ceiling),
            "no wait may exceed {ceiling}ms: {waits:?}"
        );
        // The documented default schedule, jitter aside.
        for (attempt, floor) in [(0, 500), (1, 1_500), (2, 4_500)] {
            let wait = RetrySchedule::backoff_millis(attempt, LOCATOR);
            assert!(
                (floor..floor + RetrySchedule::JITTER_MILLIS).contains(&wait),
                "attempt {attempt} waited {wait}ms, expected {floor}ms plus jitter"
            );
        }
    }

    #[test]
    fn the_jitter_is_deterministic_and_locator_specific() {
        assert_eq!(
            RetrySchedule::backoff_millis(1, LOCATOR),
            RetrySchedule::backoff_millis(1, LOCATOR),
            "the same locator and attempt must wait the same"
        );
        // Two locators fetched concurrently must not go back at the origin in one wave.
        let waits: Vec<u32> = ["https://a.invalid/x.jar", "https://b.invalid/y.jar"]
            .iter()
            .map(|locator| RetrySchedule::backoff_millis(0, locator))
            .collect();
        assert_ne!(waits[0], waits[1], "jitter must separate two locators");
    }

    #[test]
    fn a_schedule_without_retries_allows_no_second_attempt() {
        assert_eq!(RetrySchedule::none().retries(), 0);
        assert_eq!(
            RetrySchedule::new(RetrySchedule::DEFAULT_RETRIES).retries(),
            3
        );
    }

    #[test]
    fn offline_still_admits_a_host_path_and_refuses_the_network() {
        assert!(NetworkPolicy::Offline.admits(&ExternalLocator::new("file:///tmp/a.jar")));
        assert!(!NetworkPolicy::Offline.admits(&ExternalLocator::new("https://a.invalid/a.jar")));
    }
}
