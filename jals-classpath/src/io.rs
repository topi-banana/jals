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
    pub const OFFLINE_REFUSAL: &str = "not fetched while offline";

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

/// Fetch bytes from an external locator.
///
/// Project-relative files are never passed through this seam. They are read from a
/// [`jals_storage::ProjectView`]; only genuinely external content (normally HTTP) is fetched.
///
/// An implementor answers [`network`](Self::network) and supplies the two `_admitted` methods. It
/// never sees a locator its own policy refused: this crate's own `Fetch` is the only caller, and
/// it asks first.
#[allow(async_fn_in_trait)]
pub trait Fetcher {
    /// Whether this capability may reach the network.
    ///
    /// Deliberately without a default. A host constructs the fetcher, so a host is what answers;
    /// a default would let one be built without the question ever being put.
    fn network(&self) -> NetworkPolicy;

    /// Fetch `locator`, returning a diagnostic-ready error message on failure.
    ///
    /// **Precondition:** the network gate has already admitted `locator`. This crate reaches it
    /// through `api::bytes` and nothing else may call it directly.
    async fn fetch_admitted(&self, locator: &str) -> Result<Vec<u8>, String>;

    /// Fetch at most `max_bytes`, rejecting an oversized result. Carries the same precondition as
    /// [`fetch_admitted`](Self::fetch_admitted).
    ///
    /// The default buffers the whole body and checks afterwards. An implementor that can refuse
    /// before reading should override it, and both real ones do.
    async fn fetch_bounded_admitted(
        &self,
        locator: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, String> {
        let bytes = self.fetch_admitted(locator).await?;
        if bytes.len() > max_bytes {
            return Err(format!(
                "response has {} bytes, exceeding the limit of {max_bytes}",
                bytes.len()
            ));
        }
        Ok(bytes)
    }
}

pub(crate) use api::{bounded, bytes};
// `native.rs` is its only caller, so the re-export carries the same gate the module does.
#[cfg(feature = "native")]
pub(crate) use api::admit;

/// The crate's only door onto a [`Fetcher`]: apply the capability's own [`NetworkPolicy`], then
/// call it.
///
/// Not a provided method on the trait. Rust has no final trait method, so a gate written as one is
/// advisory — an implementor overrides it and the policy is gone. Nothing outside this crate calls
/// a `Fetcher` at all, so a `pub(crate)` namespace really is the only path a fetch can take.
mod api {
    use super::{ExternalLocator, Fetcher, NetworkPolicy, String, ToOwned, Vec};

    /// Fetch `locator` in full.
    pub(crate) async fn bytes<F: Fetcher>(
        fetcher: &F,
        locator: &ExternalLocator,
    ) -> Result<Vec<u8>, String> {
        admit(fetcher, locator)?;
        fetcher.fetch_admitted(locator.as_str()).await
    }

    /// Fetch `locator` under a byte ceiling.
    pub(crate) async fn bounded<F: Fetcher>(
        fetcher: &F,
        locator: &ExternalLocator,
        max_bytes: usize,
    ) -> Result<Vec<u8>, String> {
        admit(fetcher, locator)?;
        fetcher
            .fetch_bounded_admitted(locator.as_str(), max_bytes)
            .await
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
}
