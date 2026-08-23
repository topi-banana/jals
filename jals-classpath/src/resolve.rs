//! Deterministic dependency resolution into the project artifact cache.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, FileKey, Name,
    ProjectView, ProvenanceFold,
};
use sha1::{Digest as _, Sha1};

use crate::io::Fetch;
use crate::{Fetcher, MappingFormat, Warning, WarningOrigin};
use jals_config::{AmbiguousMapping, Manifest, MappingDigest, MappingFormatKind, MappingSource};

/// A non-project locator used by a host fetch adapter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExternalLocator(String);

impl ExternalLocator {
    /// Construct an external locator. It is deliberately not interpreted as a filesystem path.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `value` is a URL-shaped locator rather than a plain path — the one scheme set the
    /// host adapters share when deciding how a locator's bytes are obtained. Gated like the
    /// `native` module that is its only caller: without the gate, a `--no-default-features` build
    /// compiles a function nothing in that configuration can reach, which the wasm clippy gate
    /// reports as `dead_code` rather than something to suppress.
    #[cfg(feature = "native")]
    pub(crate) fn is_url(value: &str) -> bool {
        ["http://", "https://", "file://"]
            .iter()
            .any(|scheme| value.starts_with(scheme))
    }

    /// Whether `value` is fetched over the network — the locators worth recovering from the
    /// cache's locator index instead of refetching. Local `file://` and plain-path locators are
    /// deliberately read fresh so edits to a local jar are always picked up.
    ///
    /// It is also what [`NetworkPolicy`](crate::NetworkPolicy) asks when it decides whether to
    /// admit a locator, and it is deliberately *not* the crate-private `is_url`: that one also
    /// matches `file://`, and refusing a `file://` or plain-path locator offline would break a
    /// dependency that never wanted the network. The two predicates differ by exactly that scheme;
    /// do not merge them.
    ///
    /// Public because `jals-project` classifies a declared locator with the same rule while it
    /// builds the graph — the gate itself is this crate's own and stays here.
    pub fn is_remote(value: &str) -> bool {
        ["http://", "https://"]
            .iter()
            .any(|scheme| value.starts_with(scheme))
    }
}

/// The locator verbatim, which is what a warning attributed to it has to name: the user wrote this
/// string, so it is the one form they can find in their own manifest.
impl fmt::Display for ExternalLocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a dependency jar's bytes originate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyLocation {
    /// A file in the immutable project revision.
    Project(FileKey),
    /// An already-published artifact. Resolution verifies the existing bytes and reuses this key
    /// without fetching or publishing them again.
    Artifact(CacheKey),
    /// External content. Supplying a digest permits a verified cache hit without fetching.
    External {
        locator: ExternalLocator,
        expected: Option<ContentDigest>,
    },
}

/// Where a mapping text's bytes originate.
///
/// Deliberately not a [`DependencyLocation`]: that type's external arm pins a SHA-256 because a
/// dependency jar has no other digest to offer, while a published mapping set is as likely to be
/// pinned by SHA-1 (which is what Mojang's version metadata carries). Sharing the type would have
/// meant either widening it for a consumer that is not a dependency, or refusing the digest half the
/// real inputs come with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingLocation {
    /// A mapping text in the immutable project revision.
    Project(FileKey),
    /// A mapping text fetched and verified against `expected`.
    External {
        locator: ExternalLocator,
        expected: ExpectedDigest,
        max_bytes: usize,
    },
}

/// One already-classified mapping set, resolved from the manifest that declared it.
///
/// Carries the *resolution*, not the `[mappings]` key: a name is only meaningful inside the manifest
/// that wrote it, and a dependency's table is a different namespace from its consumer's. Resolving
/// at lowering time is what keeps that question from reaching anything downstream.
///
/// A caller outside this module holds one and hands it back — to [`MappingResolver::text`], or to a
/// remap request through `format`. The other two fields are how *this* module resolves it, so they
/// stay in it: anything that read `location` would be deciding where the bytes come from, which is
/// the decision this type exists to have already made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingSpec {
    /// The `[mappings]` key, for diagnostics only.
    name: Name,
    location: MappingLocation,
    /// Which grammar the text is written in, read by whoever builds the remap request.
    pub format: MappingFormat,
}

impl MappingSpec {
    /// Lower one alternative of the `[mappings]` entry `reference` to a spec.
    ///
    /// Says nothing about whether the alternative is *active*: `required-features` is evaluated
    /// against a feature selection, and which selection that is depends on the caller — the
    /// declaring project's own for a direct lowering, a graph node's for an edge. Keeping the gate
    /// out of here is what lets both callers share one lowering instead of growing a second, and it
    /// is why an edge can lower every alternative up front and pick between them later.
    ///
    /// `None` when the value is malformed, diagnosed into `warnings` first.
    pub fn lower(
        reference: &str,
        source: &MappingSource,
        warnings: &mut Vec<Warning>,
    ) -> Option<Self> {
        // Every rejection here points at the value that was written rather than at the key, since
        // the key is what the reader already knows.
        let mut reject = |locator: &str, message: String| {
            warnings.push(Warning::new(
                WarningOrigin::External(ExternalLocator::new(locator)),
                message,
            ));
        };
        let name = match Name::new(reference) {
            Ok(name) => name,
            Err(error) => {
                reject(
                    reference,
                    format!("mapping name is not a portable name: {error:?}"),
                );
                return None;
            }
        };
        let format = match source.format() {
            MappingFormatKind::Proguard {} => MappingFormat::Proguard,
            MappingFormatKind::TinyV2 { from, to } => MappingFormat::TinyV2 {
                from: from.clone(),
                to: to.clone(),
            },
        };
        let location = match source {
            MappingSource::File(file) => match FileKey::parse(&file.file) {
                Ok(key) => MappingLocation::Project(key),
                Err(error) => {
                    reject(
                        &file.file,
                        format!("mapping `{name}` has an invalid `file`: {error:?}"),
                    );
                    return None;
                }
            },
            MappingSource::Url(url) => {
                let expected = match url.digest(reference) {
                    Ok(MappingDigest::Sha1(hex)) => ExpectedDigest::from_hex("sha1", &hex),
                    Ok(MappingDigest::Sha256(hex)) => ExpectedDigest::from_hex("sha256", &hex),
                    Err(error) => {
                        reject(&url.url, error.to_string());
                        return None;
                    }
                };
                let Some(expected) = expected else {
                    reject(&url.url, format!("mapping `{name}` has a malformed digest"));
                    return None;
                };
                MappingLocation::External {
                    locator: ExternalLocator::new(&url.url),
                    expected,
                    // The manifest's cap is a `u64` so it can name a size the way a fetch does; the
                    // resolver's is a `usize` because it bounds an allocation. On a 32-bit host a
                    // cap above `usize::MAX` is unsatisfiable anyway, so saturating is the same
                    // answer the fetch would give.
                    max_bytes: usize::try_from(url.max_bytes).unwrap_or(usize::MAX),
                }
            }
        };
        Some(Self {
            name,
            location,
            format,
        })
    }

    /// Lower the alternative of `manifest`'s `[mappings]` entry `reference` that `enabled` activates.
    ///
    /// The single spelling of gate-then-lower, so the two callers that hold a resolved selection —
    /// the classpath plan's `[dependencies] remap` and the post-compile `[build] remap` — cannot
    /// answer "which alternative" differently.
    ///
    /// `Ok(None)` when the key is undeclared (a manifest that reached this layer unvalidated), when
    /// no alternative is active, or when the active one is malformed.
    ///
    /// # Errors
    /// [`AmbiguousMapping`] when more than one alternative is active — unreachable for a validated
    /// manifest, and passed through rather than resolved here because the two callers report it
    /// differently.
    pub fn lower_active(
        manifest: &Manifest,
        reference: &str,
        enabled: &BTreeSet<String>,
        warnings: &mut Vec<Warning>,
    ) -> Result<Option<Self>, AmbiguousMapping> {
        let Some(entry) = manifest.mappings.get(reference) else {
            return Ok(None);
        };
        Ok(entry
            .active(reference, enabled)?
            .and_then(|source| Self::lower(reference, source, warnings)))
    }
}

/// One already-classified dependency request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencySpec {
    pub name: Name,
    pub location: DependencyLocation,
    /// The mapping set that deobfuscates this jar before anything reads it, when one was declared.
    ///
    /// Applied after resolution and before nested expansion, so the classpath, the analysis index,
    /// and the skeletons an editor synthesizes all see one set of names.
    pub remap: Option<MappingSpec>,
    /// Whether nested jars should be expanded by the archive adapter.
    pub recursive: bool,
}

/// A dependency jar published in an [`ArtifactCache`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedJar {
    pub name: Name,
    pub key: CacheKey,
    pub(crate) recursive: bool,
}

/// Resolution continues after individual failures, collecting diagnostics in stable request order.
#[derive(Debug, Default)]
pub struct ResolvedDependencies {
    pub jars: Vec<ResolvedJar>,
    pub warnings: Vec<Warning>,
}

/// One spec's state after the serial classification pass: decided from the project or cache, or
/// waiting on the deduplicated fetch at `locator`.
enum Classified {
    Done(Result<CacheKey, Warning>),
    NeedsFetch { locator: usize },
}

/// Stateless mapping-text resolver.
pub struct MappingResolver;

impl MappingResolver {
    /// Read one mapping set's text, fetching and verifying it when it is external.
    ///
    /// The fetch capability is the `Fetcher`, exactly as it is for [`DependencyResolver`] beside it:
    /// a host with no network hands over one that refuses, rather than this layer carrying a policy
    /// its neighbour does not. The refusal is applied by `Fetch`, which every fetch in this crate
    /// goes through — this function used to pass `NetworkPolicy::Online` here, which was the one
    /// place the sentence above was untrue.
    ///
    /// # Errors
    /// A [`Warning`] attributed to the mapping's own location — a missing project file, a failed or
    /// oversized fetch, a digest mismatch, or bytes that are not UTF-8. Every one of them is a
    /// reason the *jar* cannot be produced, so a caller drops that input rather than using it under
    /// the names it was trying to replace.
    pub async fn text<F: Fetcher, C: CacheBackend>(
        fetcher: &F,
        view: &ProjectView,
        cache: &mut ArtifactCache<C>,
        spec: &MappingSpec,
    ) -> Result<String, Warning> {
        let bytes = match &spec.location {
            MappingLocation::Project(key) => view
                .file(key)
                .map(|file| file.bytes().to_vec())
                .map_err(|error| {
                    Warning::new(
                        WarningOrigin::ProjectFile(key.clone()),
                        format!("mapping `{}` cannot be read: {error}", spec.name),
                    )
                })?,
            MappingLocation::External {
                locator,
                expected,
                max_bytes,
            } => {
                let key = ExternalArtifactResolver::resolve(
                    fetcher,
                    cache,
                    &ExternalArtifactSpec {
                        locator: locator.clone(),
                        expected: *expected,
                        max_bytes: *max_bytes,
                        namespace: CacheNamespace::Mappings,
                    },
                )
                .await
                .map_err(|error| {
                    Warning::new(
                        WarningOrigin::External(locator.clone()),
                        format!("mapping `{}` could not be resolved: {error}", spec.name),
                    )
                })?;
                cache
                    .lookup_bounded(&key, *max_bytes)
                    .await
                    .map_err(|error| {
                        Warning::new(
                            WarningOrigin::Artifact(key.clone()),
                            format!("mapping `{}` cache read failed: {error:?}", spec.name),
                        )
                    })?
                    .ok_or_else(|| {
                        Warning::new(
                            WarningOrigin::Artifact(key.clone()),
                            format!("mapping `{}` is not cached", spec.name),
                        )
                    })?
            }
        };
        String::from_utf8(bytes).map_err(|error| {
            Warning::new(
                Self::origin(spec),
                format!("mapping `{}` is not UTF-8: {error}", spec.name),
            )
        })
    }

    /// What a diagnostic about this mapping set points at.
    fn origin(spec: &MappingSpec) -> WarningOrigin {
        match &spec.location {
            MappingLocation::Project(key) => WarningOrigin::ProjectFile(key.clone()),
            MappingLocation::External { locator, .. } => WarningOrigin::External(locator.clone()),
        }
    }
}

/// Stateless dependency resolver. Persistence belongs to [`ArtifactCache`].
pub struct DependencyResolver;

/// Expected digest supplied by an external artifact's metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedDigest {
    Sha1([u8; 20]),
    Sha256(ContentDigest),
}

impl ExpectedDigest {
    pub fn from_hex(algorithm: &str, value: &str) -> Option<Self> {
        match algorithm {
            "sha1" => {
                let bytes = Self::decode_hex::<20>(value)?;
                Some(Self::Sha1(bytes))
            }
            "sha256" => ContentDigest::from_hex(value).map(Self::Sha256),
            _ => None,
        }
    }

    fn decode_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
        if value.len() != N * 2 {
            return None;
        }
        let mut out = [0u8; N];
        for (index, chunk) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let high = char::from(chunk[0]).to_digit(16)?;
            let low = char::from(chunk[1]).to_digit(16)?;
            out[index] = u8::try_from((high << 4) | low).ok()?;
        }
        Some(out)
    }

    fn framed_bytes(self) -> Vec<u8> {
        match self {
            Self::Sha1(digest) => {
                let mut bytes = Vec::with_capacity(21);
                bytes.push(1);
                bytes.extend_from_slice(&digest);
                bytes
            }
            Self::Sha256(digest) => {
                let mut bytes = Vec::with_capacity(33);
                bytes.push(2);
                bytes.extend_from_slice(digest.as_bytes());
                bytes
            }
        }
    }

    fn matches(self, bytes: &[u8]) -> bool {
        match self {
            Self::Sha1(expected) => Sha1::digest(bytes).as_slice() == expected,
            Self::Sha256(expected) => ContentDigest::of(bytes) == expected,
        }
    }
}

/// One verified, bounded external artifact request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalArtifactSpec {
    pub locator: ExternalLocator,
    pub expected: ExpectedDigest,
    pub max_bytes: usize,
    pub namespace: CacheNamespace,
}

/// Cache-first resolver shared by build tasks and dependency lowering.
pub struct ExternalArtifactResolver;

impl ExternalArtifactResolver {
    pub async fn resolve<F: Fetcher, C: CacheBackend>(
        fetcher: &F,
        cache: &mut ArtifactCache<C>,
        spec: &ExternalArtifactSpec,
    ) -> Result<CacheKey, String> {
        if spec.max_bytes == 0 {
            return Err("external artifact has a zero byte limit".to_owned());
        }
        let provenance = Self::provenance(spec);
        let cached = match spec.expected {
            ExpectedDigest::Sha256(content) => {
                Some(CacheKey::new(spec.namespace, provenance, content))
            }
            ExpectedDigest::Sha1(_) => cache
                .indexed_key(spec.namespace, provenance)
                .await
                .ok()
                .flatten(),
        };
        if let Some(key) = cached
            && let Ok(Some(bytes)) = cache.lookup_bounded(&key, spec.max_bytes).await
            && spec.expected.matches(&bytes)
        {
            return Ok(key);
        }
        // Not redundant with the gate inside `Fetch`: this is the *message*. Only here is it known
        // that the verified cache was already tried and missed, and this string reaches
        // `BuildTaskRunError::Node`, which renders it with no origin beside it — so it names its
        // own locator. Both ask `NetworkPolicy::admits`, so they cannot disagree about which
        // locators are refused, only about how the refusal reads.
        if !fetcher.network().admits(&spec.locator) {
            return Err(format!(
                "external artifact `{}` is not available in the verified cache while offline",
                spec.locator.as_str()
            ));
        }
        let bytes = Fetch::bounded(fetcher, &spec.locator, spec.max_bytes).await?;
        if !spec.expected.matches(&bytes) {
            return Err(format!(
                "external artifact `{}` digest mismatch",
                spec.locator.as_str()
            ));
        }
        let key = CacheKey::new(spec.namespace, provenance, ContentDigest::of(&bytes));
        cache
            .publish(&key, &bytes)
            .await
            .map_err(|error| format!("external artifact cache publish failed: {error:?}"))?;
        cache
            .record_index(&key)
            .await
            .map_err(|error| format!("external artifact index update failed: {error:?}"))?;
        Ok(key)
    }

    /// The provenance shared by a fetched artifact's published key and its locator-index
    /// recovery for SHA-1-pinned specs. Publish and recovery must fold identically or every
    /// recovery misses and the artifact is refetched — never inline one side.
    fn provenance(spec: &ExternalArtifactSpec) -> ContentDigest {
        let mut fold = ProvenanceFold::new(b"external-artifact\0");
        fold.bytes(spec.locator.as_str().as_bytes())
            .bytes(&spec.expected.framed_bytes());
        fold.finish()
    }
}

impl DependencyResolver {
    /// Resolve project and external jars into the cache.
    ///
    /// Three passes keep the output byte-identical to a sequential walk while overlapping the
    /// network waits: (1) serial, in spec order — everything up to a fetch (project publication,
    /// verified lookups, locator-index recovery); (2) the remaining locators, deduplicated,
    /// fetched concurrently on the current task; (3) serial, in spec order — digest verification,
    /// write-once publication, index recording, and emission of jars and warnings.
    pub async fn resolve<F: Fetcher, C: CacheBackend>(
        fetcher: &F,
        view: &ProjectView,
        cache: &mut ArtifactCache<C>,
        specs: &[DependencySpec],
    ) -> ResolvedDependencies {
        // Pass 1: classify serially, collecting the deduplicated locators still needing bytes.
        let mut classified = Vec::with_capacity(specs.len());
        let mut locators: Vec<&ExternalLocator> = Vec::new();
        for spec in specs {
            let state = Self::classify(view, cache, spec).await.map_or_else(
                || {
                    let DependencyLocation::External { locator, .. } = &spec.location else {
                        unreachable!("only external specs need a fetch");
                    };
                    let index = locators
                        .iter()
                        .position(|known| *known == locator)
                        .unwrap_or_else(|| {
                            locators.push(locator);
                            locators.len() - 1
                        });
                    Classified::NeedsFetch { locator: index }
                },
                Classified::Done,
            );
            classified.push(state);
        }

        // Pass 2: overlap the network waits. Single-thread concurrency is the right shape here —
        // the work is waiting, not CPU.
        let fetched = jals_exec::join_ordered(
            locators
                .iter()
                .map(|locator| Fetch::bytes(fetcher, locator)),
        )
        .await;

        // Pass 3: serial, in spec order — verify, publish, record, emit.
        let mut out = ResolvedDependencies::default();
        for (spec, state) in specs.iter().zip(classified) {
            let outcome = match state {
                Classified::Done(outcome) => outcome,
                Classified::NeedsFetch { locator } => {
                    Self::publish_fetched(cache, spec, &fetched[locator]).await
                }
            };
            match outcome {
                Ok(key) => out.jars.push(ResolvedJar {
                    name: spec.name.clone(),
                    key,
                    recursive: spec.recursive,
                }),
                Err(warning) => out.warnings.push(warning),
            }
        }
        out
    }

    /// Everything that can be decided before fetching: project reads/publication, verified
    /// external lookups, and locator-index recovery. `None` means the spec needs a fetch.
    async fn classify<C: CacheBackend>(
        view: &ProjectView,
        cache: &mut ArtifactCache<C>,
        spec: &DependencySpec,
    ) -> Option<Result<CacheKey, Warning>> {
        match &spec.location {
            DependencyLocation::Project(file) => {
                Some(Self::publish_project(view, cache, spec, file).await)
            }
            DependencyLocation::Artifact(key) => Some(match cache.open_verified(key).await {
                Ok(Some(_)) => Ok(key.clone()),
                Ok(None) => Err(Warning::new(
                    WarningOrigin::Artifact(key.clone()),
                    format!("dependency `{}` artifact is not cached", spec.name),
                )),
                Err(error) => Err(Warning::new(
                    WarningOrigin::Artifact(key.clone()),
                    format!("dependency `{}` artifact is invalid: {error:?}", spec.name),
                )),
            }),
            DependencyLocation::External { locator, expected } => {
                if let Some(content) = expected {
                    let key = CacheKey::new(
                        CacheNamespace::DependencyJar,
                        Self::external_provenance(locator),
                        *content,
                    );
                    match cache.open_verified(&key).await {
                        Ok(Some(_)) => return Some(Ok(key)),
                        Ok(None) => {}
                        Err(error) => {
                            return Some(Err(Warning::new(
                                WarningOrigin::External(locator.clone()),
                                format!(
                                    "dependency `{}` cache lookup failed: {error:?}",
                                    spec.name
                                ),
                            )));
                        }
                    }
                } else if ExternalLocator::is_remote(locator.as_str()) {
                    // No pinned digest: recover the content half of the key from the cache's
                    // locator index, so an already-fetched dependency resolves from the
                    // persistent cache (and offline). The artifact is still read through the
                    // verified lookup; any index or artifact problem just falls back to a fetch.
                    let provenance = Self::external_provenance(locator);
                    if let Ok(Some(key)) = cache
                        .indexed_key(CacheNamespace::DependencyJar, provenance)
                        .await
                        && matches!(cache.open_verified(&key).await, Ok(Some(_)))
                    {
                        return Some(Ok(key));
                    }
                }
                None
            }
        }
    }

    async fn publish_project<C: CacheBackend>(
        view: &ProjectView,
        cache: &mut ArtifactCache<C>,
        spec: &DependencySpec,
        file: &FileKey,
    ) -> Result<CacheKey, Warning> {
        let bytes = view
            .file(file)
            .map_err(|error| {
                Warning::new(
                    WarningOrigin::ProjectFile(file.clone()),
                    format!("dependency `{}` cannot be read: {error}", spec.name),
                )
            })?
            .bytes();
        let key = CacheKey::derive(
            CacheNamespace::DependencyJar,
            b"project\0",
            file.to_string().as_bytes(),
            ContentDigest::of(bytes),
        );
        cache.publish(&key, bytes).await.map_err(|error| {
            Warning::new(
                WarningOrigin::ProjectFile(file.clone()),
                format!("dependency `{}` cache publish failed: {error:?}", spec.name),
            )
        })?;
        Ok(key)
    }

    /// The pass-3 half of an external resolution: verify the fetched bytes against a pinned
    /// digest, publish write-once, and record the locator index for remote locators.
    async fn publish_fetched<C: CacheBackend>(
        cache: &mut ArtifactCache<C>,
        spec: &DependencySpec,
        fetched: &Result<Vec<u8>, String>,
    ) -> Result<CacheKey, Warning> {
        let DependencyLocation::External { locator, expected } = &spec.location else {
            unreachable!("only external specs are fetched");
        };
        let bytes = fetched.as_ref().map_err(|message| {
            Warning::new(
                WarningOrigin::External(locator.clone()),
                format!("dependency `{}` fetch failed: {message}", spec.name),
            )
        })?;
        let actual = ContentDigest::of(bytes);
        if let Some(expected) = expected
            && *expected != actual
        {
            return Err(Warning::new(
                WarningOrigin::External(locator.clone()),
                format!(
                    "dependency `{}` digest mismatch: expected {}, got {}",
                    spec.name,
                    expected.to_hex(),
                    actual.to_hex()
                ),
            ));
        }
        let key = CacheKey::new(
            CacheNamespace::DependencyJar,
            Self::external_provenance(locator),
            actual,
        );
        cache.publish(&key, bytes).await.map_err(|error| {
            Warning::new(
                WarningOrigin::External(locator.clone()),
                format!("dependency `{}` cache publish failed: {error:?}", spec.name),
            )
        })?;
        // Best-effort: remember this locator's content so a digest-less request can recover it
        // next time. Resolution already succeeded; an index write failure only costs a refetch
        // later.
        if ExternalLocator::is_remote(locator.as_str()) {
            let _ = cache.record_index(&key).await;
        }
        Ok(key)
    }

    /// The provenance shared by an external jar's published key and its locator-index
    /// recovery. Publish and recovery must fold identically or every recovery misses and the
    /// jar is refetched forever — never inline one side.
    fn external_provenance(locator: &ExternalLocator) -> ContentDigest {
        let mut fold = ProvenanceFold::new(b"external\0");
        fold.bytes(locator.as_str().as_bytes());
        fold.finish()
    }
}
