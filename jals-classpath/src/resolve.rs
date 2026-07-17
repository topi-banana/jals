//! Deterministic dependency resolution into the project artifact cache.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, FileKey, Name,
    ProjectView,
};

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

/// Stateless dependency resolver. Persistence belongs to [`ArtifactCache`].
pub struct DependencyResolver;

impl DependencyResolver {
    /// Resolve project and external jars into the cache.
    ///
    /// Project content is read only from `view`; external content is published write-once after its
    /// SHA-256 content digest is known. An external request with `expected` first performs a verified
    /// lookup and therefore avoids a download on a cache hit.
    #[allow(clippy::future_not_send)]
    pub async fn resolve<F: Fetcher, C: CacheBackend>(
        fetcher: &F,
        view: &ProjectView,
        cache: &mut ArtifactCache<C>,
        specs: &[DependencySpec],
    ) -> ResolvedDependencies {
        let mut out = ResolvedDependencies::default();
        for spec in specs {
            match Self::resolve_one(fetcher, view, cache, spec).await {
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

    #[allow(clippy::future_not_send)]
    async fn resolve_one<F: Fetcher, C: CacheBackend>(
        fetcher: &F,
        view: &ProjectView,
        cache: &mut ArtifactCache<C>,
        spec: &DependencySpec,
    ) -> Result<CacheKey, Warning> {
        match &spec.location {
            DependencyLocation::Project(file) => {
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
                cache.publish(&key, bytes).map_err(|error| {
                    Warning::new(
                        WarningOrigin::ProjectFile(file.clone()),
                        format!("dependency `{}` cache publish failed: {error:?}", spec.name),
                    )
                })?;
                Ok(key)
            }
            DependencyLocation::External { locator, expected } => {
                if let Some(content) = expected {
                    let key = Self::cache_key_for_digest(
                        CacheNamespace::DependencyJar,
                        b"external\0",
                        locator.as_str().as_bytes(),
                        *content,
                    );
                    match cache.open_verified(&key) {
                        Ok(Some(_)) => return Ok(key),
                        Ok(None) => {}
                        Err(error) => {
                            return Err(Warning::new(
                                WarningOrigin::External(locator.clone()),
                                format!(
                                    "dependency `{}` cache lookup failed: {error:?}",
                                    spec.name
                                ),
                            ));
                        }
                    }
                } else if ExternalLocator::is_remote(locator.as_str()) {
                    // No pinned digest: recover the content half of the key from the cache's
                    // locator index, so an already-fetched dependency resolves from the
                    // persistent cache (and offline). The artifact is still read through the
                    // verified lookup; any index or artifact problem just falls back to a fetch.
                    let provenance =
                        Self::provenance_digest(b"external\0", locator.as_str().as_bytes());
                    if let Ok(Some(key)) =
                        cache.indexed_key(CacheNamespace::DependencyJar, provenance)
                        && matches!(cache.open_verified(&key), Ok(Some(_)))
                    {
                        return Ok(key);
                    }
                }
                let bytes = fetcher.fetch(locator.as_str()).await.map_err(|message| {
                    Warning::new(
                        WarningOrigin::External(locator.clone()),
                        format!("dependency `{}` fetch failed: {message}", spec.name),
                    )
                })?;
                let actual = ContentDigest::of(&bytes);
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
                cache.publish(&key, &bytes).map_err(|error| {
                    Warning::new(
                        WarningOrigin::External(locator.clone()),
                        format!("dependency `{}` cache publish failed: {error:?}", spec.name),
                    )
                })?;
                // Best-effort: remember this locator's content so a digest-less request can
                // recover it next time. Resolution already succeeded; an index write failure
                // only costs a refetch later.
                if ExternalLocator::is_remote(locator.as_str()) {
                    let _ = cache.record_index(&key);
                }
                Ok(key)
            }
        }
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
