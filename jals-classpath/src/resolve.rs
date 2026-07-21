//! Deterministic dependency resolution into the project artifact cache.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, FileKey, Name,
    ProjectView,
};
use sha1::{Digest as _, Sha1};

use crate::{Fetcher, Warning, WarningOrigin};

/// A non-project locator used by a host fetch adapter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExternalLocator(String);

impl ExternalLocator {
    /// Construct an external locator. It is deliberately not interpreted as a filesystem path.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `value` is a URL-shaped locator rather than a plain path — the one scheme set the
    /// host adapters share when deciding how a locator's bytes are obtained.
    pub fn is_url(value: &str) -> bool {
        ["http://", "https://", "file://"]
            .iter()
            .any(|scheme| value.starts_with(scheme))
    }

    /// Whether `value` is fetched over the network — the locators worth recovering from the
    /// cache's locator index instead of refetching. Local `file://` and plain-path locators are
    /// deliberately read fresh so edits to a local jar are always picked up.
    pub(crate) fn is_remote(value: &str) -> bool {
        ["http://", "https://"]
            .iter()
            .any(|scheme| value.starts_with(scheme))
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

/// One already-classified dependency request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencySpec {
    pub name: Name,
    pub location: DependencyLocation,
    /// Whether nested jars should be expanded by the archive adapter.
    pub recursive: bool,
}

/// A dependency jar published in an [`ArtifactCache`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedJar {
    pub name: Name,
    pub key: CacheKey,
    pub recursive: bool,
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

/// Stateless dependency resolver. Persistence belongs to [`ArtifactCache`].
pub struct DependencyResolver;

/// Whether an external-artifact resolution may use the fetch capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    Online,
    Offline,
}

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
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
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
        network: NetworkPolicy,
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
        if network == NetworkPolicy::Offline {
            return Err(format!(
                "external artifact `{}` is not available in the verified cache while offline",
                spec.locator.as_str()
            ));
        }
        let bytes = fetcher
            .fetch_bounded(spec.locator.as_str(), spec.max_bytes)
            .await?;
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

    fn provenance(spec: &ExternalArtifactSpec) -> ContentDigest {
        let expected = spec.expected.framed_bytes();
        let mut bytes = Vec::with_capacity(spec.locator.as_str().len() + expected.len() + 8);
        bytes.extend_from_slice(&(spec.locator.as_str().len() as u64).to_be_bytes());
        bytes.extend_from_slice(spec.locator.as_str().as_bytes());
        bytes.extend_from_slice(&expected);
        ContentDigest::of(&bytes)
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
                .map(|locator| fetcher.fetch(locator.as_str())),
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
                    let key = Self::cache_key_for_digest(
                        CacheNamespace::DependencyJar,
                        b"external\0",
                        locator.as_str().as_bytes(),
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
                    let provenance =
                        Self::provenance_digest(b"external\0", locator.as_str().as_bytes());
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
        let key = Self::cache_key(
            CacheNamespace::DependencyJar,
            b"project\0",
            file.to_string().as_bytes(),
            bytes,
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
        let key = Self::cache_key_for_digest(
            CacheNamespace::DependencyJar,
            b"external\0",
            locator.as_str().as_bytes(),
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

    pub(crate) fn cache_key(
        namespace: CacheNamespace,
        kind: &[u8],
        provenance: &[u8],
        bytes: &[u8],
    ) -> CacheKey {
        Self::cache_key_for_digest(namespace, kind, provenance, ContentDigest::of(bytes))
    }

    pub(crate) fn cache_key_for_digest(
        namespace: CacheNamespace,
        kind: &[u8],
        provenance: &[u8],
        content: ContentDigest,
    ) -> CacheKey {
        CacheKey::new(
            namespace,
            Self::provenance_digest(kind, provenance),
            content,
        )
    }

    /// The length-framed `(kind, provenance)` digest shared by every classpath cache key, so a
    /// key can also be recovered from provenance alone through the cache's locator index.
    pub(crate) fn provenance_digest(kind: &[u8], provenance: &[u8]) -> ContentDigest {
        let mut framed = Vec::with_capacity(kind.len() + 8 + provenance.len());
        framed.extend_from_slice(kind);
        framed.extend_from_slice(&(provenance.len() as u64).to_be_bytes());
        framed.extend_from_slice(provenance);
        ContentDigest::of(&framed)
    }
}
