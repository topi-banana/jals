//! Resolving the reference screenshots a `[[test-target]]` judges its own against.
//!
//! A golden set is a fetched artifact and is treated as one: pinned by digest, verified before it
//! is opened, cached under its own namespace. That is the whole reason it is *not* a directory of
//! committed files — reference images are binary, they are large, and they are regenerated whenever
//! the renderer that produced them moves, which is three arguments against a repository and none
//! against the machinery `jals.toml` already has for pinning bytes.
//!
//! Here rather than in `jals-build` for the same reason `MappingSpec` is here: this crate already
//! owns the fetch gate, the digest verification and the archive reader, and a golden set needs all
//! three. What it is *for* — comparing pictures — is `jals-build`'s and `jals-image`'s, and neither
//! of those is named in this file.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;

use jals_config::Manifest;
use jals_config::testing::GoldenSource;
use jals_exec::Exec;
use jals_storage::{ArtifactCache, CacheBackend, CacheNamespace, RelativePath};

use crate::io::Fetcher;
use crate::load::{FileTreeExtraction, SourceTree, SourceTreeLimits};
use crate::resolve::{
    ExpectedDigest, ExternalArtifactResolver, ExternalArtifactSpec, ExternalLocator,
};
use crate::{Warning, WarningOrigin};

/// The reference images one `[[golden.<name>]]` key resolves to.
pub struct GoldenSet;

impl GoldenSet {
    /// The most members a golden archive may hold.
    ///
    /// A screenshot suite is a handful of pictures per release, not a corpus: a cap in the low
    /// thousands is far above anything anyone writes and far below "unpack whatever arrives".
    const MAX_MEMBERS: usize = 4_096;

    /// How much larger than the archive its contents may unpack to.
    ///
    /// PNG is already compressed, so a stored or barely-deflated archive of them unpacks to about
    /// its own size. Four times that is headroom for a differently-packed archive without being an
    /// invitation: the bound exists so a hostile archive cannot expand into memory unbounded, and
    /// the digest has already established the bytes are the ones the manifest named.
    const EXPANSION_HEADROOM: usize = 4;

    /// Resolve the alternative of `manifest`'s `[[golden.<name>]]` entry `reference` that `enabled`
    /// activates, fetching and unpacking it.
    ///
    /// `Ok(None)` is the ordinary answer in two cases, and neither is a failure: the key is not
    /// declared, or no alternative is active under this selection. The second is what a project
    /// looks like before its first golden archive exists — every screenshot then has nothing to be
    /// compared against, which the caller reports as such.
    ///
    /// # Errors
    /// A [`Warning`] when the active alternative is malformed, cannot be fetched or verified, or
    /// does not unpack.
    pub async fn resolve<F: Fetcher, C: CacheBackend>(
        fetcher: &F,
        cache: &mut ArtifactCache<C>,
        exec: &Exec,
        manifest: &Manifest,
        reference: &str,
        enabled: &BTreeSet<String>,
    ) -> Result<Option<SourceTree>, Warning> {
        let Some(entry) = manifest.golden.get(reference) else {
            return Ok(None);
        };
        // An ambiguous entry is a manifest error a validated manifest cannot reach, and
        // `WarningOrigin` has no manifest variant — the origins it names are all *artifacts*. The
        // first alternative's URL is the closest true subject: it is one of the two the selection
        // could not choose between.
        let source = entry.active(reference, enabled).map_err(|error| {
            let subject = entry
                .alternatives()
                .first()
                .map_or_else(|| reference.to_owned(), |source| source.url.clone());
            Warning::new(
                WarningOrigin::External(ExternalLocator::new(&subject)),
                format!("golden set `{reference}`: {error}"),
            )
        })?;
        let Some(source) = source else {
            return Ok(None);
        };
        Self::fetch(fetcher, cache, exec, reference, source)
            .await
            .map(Some)
    }

    /// Fetch one alternative and unpack it into a tree of reference images.
    async fn fetch<F: Fetcher, C: CacheBackend>(
        fetcher: &F,
        cache: &mut ArtifactCache<C>,
        exec: &Exec,
        reference: &str,
        source: &GoldenSource,
    ) -> Result<SourceTree, Warning> {
        let locator = ExternalLocator::new(&source.url);
        let expected = ExpectedDigest::from_hex("sha256", &source.sha256).ok_or_else(|| {
            Warning::new(
                WarningOrigin::External(locator.clone()),
                format!("golden set `{reference}` has a malformed sha256"),
            )
        })?;
        // The manifest's cap is a `u64` so it can name a size the way a fetch does; the resolver's
        // is a `usize` because it bounds an allocation.
        let max_bytes = usize::try_from(source.max_bytes).unwrap_or(usize::MAX);
        let key = ExternalArtifactResolver::resolve(
            fetcher,
            cache,
            &ExternalArtifactSpec {
                locator: locator.clone(),
                expected,
                max_bytes,
                namespace: CacheNamespace::GoldenScreenshots,
            },
        )
        .await
        .map_err(|error| {
            Warning::new(
                WarningOrigin::External(locator.clone()),
                format!("golden set `{reference}` could not be resolved: {error}"),
            )
        })?;

        FileTreeExtraction::all(
            exec,
            cache,
            &key,
            &RelativePath::ROOT,
            SourceTreeLimits {
                max_files: Self::MAX_MEMBERS,
                max_file_bytes: max_bytes,
                max_total_bytes: max_bytes.saturating_mul(Self::EXPANSION_HEADROOM),
            },
        )
        .await
        .map_err(|error| {
            Warning::new(
                WarningOrigin::Artifact(key.clone()),
                format!("golden set `{reference}` could not be unpacked: {error}"),
            )
        })
    }

    /// Render the `[[golden.<name>]]` block an author pastes after publishing an archive.
    ///
    /// Rendered here rather than by the CLI because the shape it has to match is this module's
    /// contract: a reader of the block below and the parser above have to agree, and one of them
    /// changing without the other is the failure this places next to each other to avoid.
    #[must_use]
    pub fn declaration(
        reference: &str,
        required_features: &BTreeSet<String>,
        url: &str,
        sha256: &str,
        bytes: usize,
    ) -> String {
        let features = required_features
            .iter()
            .map(|feature| format!("\"{feature}\""))
            .collect::<alloc::vec::Vec<_>>()
            .join(", ");
        format!(
            "[[golden.{reference}]]\n\
             required-features = [{features}]\n\
             url = \"{url}\"\n\
             sha256 = \"{sha256}\"\n\
             max-bytes = {}\n",
            // Rounded up to the next KiB, exactly as the mapping tables round to the next MiB: the
            // digest is what guarantees the content, so pinning an exact byte count would only ever
            // break on a re-serve of identical bytes with different framing.
            bytes.div_ceil(1024).saturating_mul(1024).max(1024)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::GoldenSet;
    use alloc::borrow::ToOwned;
    use alloc::collections::BTreeSet;
    use alloc::string::String;

    #[test]
    fn a_declaration_is_pasteable_toml_with_a_rounded_cap() {
        let features: BTreeSet<String> = core::iter::once("1.21.11".to_owned()).collect();
        let block = GoldenSet::declaration(
            "client-e2e",
            &features,
            "https://example.invalid/g.zip",
            "abc",
            1500,
        );
        assert_eq!(
            block,
            "[[golden.client-e2e]]\n\
             required-features = [\"1.21.11\"]\n\
             url = \"https://example.invalid/g.zip\"\n\
             sha256 = \"abc\"\n\
             max-bytes = 2048\n"
        );
    }

    #[test]
    fn a_selection_with_no_features_still_renders_the_key() {
        let block = GoldenSet::declaration("g", &BTreeSet::new(), "https://x/", "d", 10);
        assert!(block.contains("required-features = []"), "{block}");
        // Never zero: a cap of zero admits nothing, which the schema refuses.
        assert!(block.contains("max-bytes = 1024"), "{block}");
    }
}
