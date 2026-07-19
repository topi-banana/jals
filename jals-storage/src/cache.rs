use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use jals_exec::Yielder;
use sha2::{Digest, Sha256};

use crate::error::CacheError;
use crate::io::{self, IoError, Read as _, Seek as _};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// Digest an entire reader by streaming fixed-size chunks, never materializing the content.
    /// Cooperates once per chunk, so digesting a large artifact never monopolizes the executor.
    pub async fn of_reader<R: io::Read>(reader: &mut R) -> core::result::Result<Self, IoError> {
        let mut hasher = Sha256::new();
        let mut chunk = vec![0u8; 64 * 1024];
        let mut yielder = Yielder::new();
        loop {
            match reader.read(&mut chunk).await? {
                0 => return Ok(Self(hasher.finalize().into())),
                n => hasher.update(&chunk[..n]),
            }
            yielder.tick().await;
        }
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        out
    }

    /// Parse the [`to_hex`](Self::to_hex) rendering back into a digest. `None` for anything
    /// that is not exactly 64 hex characters.
    pub fn from_hex(value: &str) -> Option<Self> {
        let bytes = value.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (index, chunk) in bytes.chunks_exact(2).enumerate() {
            let high = char::from(chunk[0]).to_digit(16)?;
            let low = char::from(chunk[1]).to_digit(16)?;
            out[index] = u8::try_from((high << 4) | low).ok()?;
        }
        Some(Self(out))
    }
}

impl fmt::Debug for ContentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CacheNamespace {
    DependencyJar,
    NestedJar,
    ExtractedSource,
    Skeleton,
    GitCheckout,
    PathSource,
    ExternalClasspath,
    BuildScriptState,
    BuildScriptOutput,
}

impl CacheNamespace {
    #[cfg(any(feature = "std", test))]
    pub(crate) const fn directory(self) -> &'static str {
        match self {
            Self::DependencyJar => "dependency-jar",
            Self::NestedJar => "nested-jar",
            Self::ExtractedSource => "extracted-source",
            Self::Skeleton => "skeleton",
            Self::GitCheckout => "git-checkout",
            Self::PathSource => "path-source",
            Self::ExternalClasspath => "external-classpath",
            Self::BuildScriptState => "build-script-state",
            Self::BuildScriptOutput => "build-script-output",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheKey {
    namespace: CacheNamespace,
    provenance: ContentDigest,
    content: ContentDigest,
}

impl CacheKey {
    pub const fn new(
        namespace: CacheNamespace,
        provenance: ContentDigest,
        content: ContentDigest,
    ) -> Self {
        Self {
            namespace,
            provenance,
            content,
        }
    }
    pub const fn namespace(&self) -> CacheNamespace {
        self.namespace
    }
    pub const fn provenance(&self) -> ContentDigest {
        self.provenance
    }
    pub const fn content(&self) -> ContentDigest {
        self.content
    }
}

pub(crate) mod private {
    pub trait Sealed {}
}

/// Closed persistence seam used by [`ArtifactCache`].
///
/// The backend itself lives on the main task — every method runs there. What crosses threads is
/// an owned [`Reader`](Self::Reader) clone handed to a fan-out worker as a `Send` input; the
/// read futures it produces are `!Send` and are driven entirely on that worker.
#[allow(async_fn_in_trait)]
pub trait CacheBackend: private::Sealed {
    /// Owning, cheap-to-clone reader over one stored artifact. Every clone reads at an
    /// independent position: the parallel archive walkers clone one open archive per worker
    /// and interleave reads.
    #[doc(hidden)]
    type Reader: io::Read + io::Seek + Clone + Send + 'static;
    #[doc(hidden)]
    async fn open(&self, key: &CacheKey) -> core::result::Result<Option<Self::Reader>, CacheError>;
    #[doc(hidden)]
    async fn load(&self, key: &CacheKey) -> core::result::Result<Option<Vec<u8>>, CacheError>;
    #[doc(hidden)]
    async fn publish_once(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
    ) -> core::result::Result<(), CacheError>;
    #[doc(hidden)]
    async fn load_index(
        &self,
        namespace: CacheNamespace,
        provenance: &ContentDigest,
    ) -> core::result::Result<Option<ContentDigest>, CacheError>;
    #[doc(hidden)]
    async fn store_index(
        &mut self,
        namespace: CacheNamespace,
        provenance: &ContentDigest,
        content: &ContentDigest,
    ) -> core::result::Result<(), CacheError>;
}

#[derive(Debug, Clone, Default)]
pub struct MemoryCache {
    entries: BTreeMap<CacheKey, Arc<[u8]>>,
    index: BTreeMap<(CacheNamespace, ContentDigest), ContentDigest>,
}

impl private::Sealed for MemoryCache {}

impl CacheBackend for MemoryCache {
    type Reader = io::Cursor<Arc<[u8]>>;

    async fn open(&self, key: &CacheKey) -> core::result::Result<Option<Self::Reader>, CacheError> {
        Ok(self
            .entries
            .get(key)
            .map(|bytes| io::Cursor::new(Arc::clone(bytes))))
    }

    async fn load(&self, key: &CacheKey) -> core::result::Result<Option<Vec<u8>>, CacheError> {
        Ok(self.entries.get(key).map(|bytes| bytes.to_vec()))
    }

    async fn publish_once(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
    ) -> core::result::Result<(), CacheError> {
        match self.entries.get(key) {
            Some(existing) if existing[..] == *bytes => Ok(()),
            Some(_) => Err(CacheError::Conflict),
            None => {
                self.entries.insert(key.clone(), Arc::from(bytes));
                Ok(())
            }
        }
    }

    async fn load_index(
        &self,
        namespace: CacheNamespace,
        provenance: &ContentDigest,
    ) -> core::result::Result<Option<ContentDigest>, CacheError> {
        Ok(self.index.get(&(namespace, *provenance)).copied())
    }

    async fn store_index(
        &mut self,
        namespace: CacheNamespace,
        provenance: &ContentDigest,
        content: &ContentDigest,
    ) -> core::result::Result<(), CacheError> {
        self.index.insert((namespace, *provenance), *content);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactCache<C: CacheBackend> {
    backend: C,
}

impl<C: CacheBackend> ArtifactCache<C> {
    pub const fn new(backend: C) -> Self {
        Self { backend }
    }

    pub async fn lookup(
        &self,
        key: &CacheKey,
    ) -> core::result::Result<Option<Vec<u8>>, CacheError> {
        let Some(bytes) = self.backend.load(key).await? else {
            return Ok(None);
        };
        let digest = ContentDigest::of_reader(&mut bytes.as_slice())
            .await
            .map_err(|error| CacheError::Io(error.to_string()))?;
        if digest != key.content {
            return Err(CacheError::Corrupt);
        }
        Ok(Some(bytes))
    }

    /// Reader-based verified lookup: stream the stored bytes through one digest pass, rewind,
    /// and hand the reader out — the artifact is never materialized in memory. `Ok(None)` is a
    /// miss; a digest mismatch is [`CacheError::Corrupt`]; a source failure is
    /// [`CacheError::Io`], never conflated with a miss. Verification pins the opened backend
    /// resource, so a post-verification swap of the stored location cannot redirect reads.
    pub async fn open_verified(
        &self,
        key: &CacheKey,
    ) -> core::result::Result<Option<C::Reader>, CacheError> {
        fn io_failure(error: &IoError) -> CacheError {
            CacheError::Io(error.to_string())
        }
        let Some(mut reader) = self.backend.open(key).await? else {
            return Ok(None);
        };
        let digest = ContentDigest::of_reader(&mut reader)
            .await
            .map_err(|error| io_failure(&error))?;
        if digest != key.content {
            return Err(CacheError::Corrupt);
        }
        reader
            .seek(io::SeekFrom::Start(0))
            .await
            .map_err(|error| io_failure(&error))?;
        Ok(Some(reader))
    }

    /// Verified whole-buffer lookup with an allocation bound.
    ///
    /// The artifact is first verified through [`open_verified`](Self::open_verified), which uses
    /// fixed-size streaming memory. Its pinned reader is then measured before the output buffer is
    /// allocated. An artifact larger than `max_bytes` returns [`CacheError::TooLarge`].
    pub async fn lookup_bounded(
        &self,
        key: &CacheKey,
        max_bytes: usize,
    ) -> core::result::Result<Option<Vec<u8>>, CacheError> {
        fn io_failure(error: &IoError) -> CacheError {
            CacheError::Io(error.to_string())
        }

        let Some(mut reader) = self.open_verified(key).await? else {
            return Ok(None);
        };
        let size = reader
            .seek(io::SeekFrom::End(0))
            .await
            .map_err(|error| io_failure(&error))?;
        if size > u64::try_from(max_bytes).unwrap_or(u64::MAX) {
            return Err(CacheError::TooLarge {
                size,
                limit: max_bytes,
            });
        }
        reader
            .seek(io::SeekFrom::Start(0))
            .await
            .map_err(|error| io_failure(&error))?;
        let len = usize::try_from(size).map_err(|_| CacheError::TooLarge {
            size,
            limit: max_bytes,
        })?;
        let mut bytes = vec![0; len];
        reader
            .read_exact(&mut bytes)
            .await
            .map_err(|error| io_failure(&error))?;
        Ok(Some(bytes))
    }

    pub async fn publish(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
    ) -> core::result::Result<(), CacheError> {
        if ContentDigest::of(bytes) != key.content {
            return match self.backend.load(key).await? {
                Some(existing) if existing != bytes => Err(CacheError::Conflict),
                _ => Err(CacheError::DigestMismatch),
            };
        }
        // Keys are content-addressed, so a stored artifact whose digest matches the key IS the
        // write-once winner: a warm publish returns without re-writing. This is not the
        // forbidden contains-then-write — the hit is verified, and a miss still goes through
        // the backend's atomic create-once, which arbitrates real races.
        if let Some(existing) = self.backend.load(key).await?
            && ContentDigest::of(&existing) == key.content
        {
            return Ok(());
        }
        // The digest check above plus the backend's write-once winner comparison already
        // guarantee the stored bytes match `key`; no read-back verification is needed.
        self.backend.publish_once(key, bytes).await
    }

    /// The full key most recently recorded for `(namespace, provenance)` through
    /// [`record_index`](Self::record_index), if any. The index is advisory recovery metadata —
    /// it lets a caller that knows only an artifact's provenance (say, a dependency locator
    /// with no pinned digest) rediscover the content half of the key. The artifact must still
    /// be read through verified [`lookup`](Self::lookup), which is what keeps a stale or
    /// tampered index entry harmless: it can cause a miss, never wrong bytes.
    pub async fn indexed_key(
        &self,
        namespace: CacheNamespace,
        provenance: ContentDigest,
    ) -> core::result::Result<Option<CacheKey>, CacheError> {
        Ok(self
            .backend
            .load_index(namespace, &provenance)
            .await?
            .map(|content| CacheKey::new(namespace, provenance, content)))
    }

    /// Remember `key` as the current content for its `(namespace, provenance)` pair. Unlike
    /// artifact publication this is last-writer-wins by design: every racer records a digest
    /// its own verified artifact backs, so either outcome is valid.
    pub async fn record_index(&mut self, key: &CacheKey) -> core::result::Result<(), CacheError> {
        self.backend
            .store_index(key.namespace(), &key.provenance(), &key.content())
            .await
    }

    pub const fn backend(&self) -> &C {
        &self.backend
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;

    use super::*;

    fn key(bytes: &[u8]) -> CacheKey {
        CacheKey::new(
            CacheNamespace::DependencyJar,
            ContentDigest::of(b"source"),
            ContentDigest::of(bytes),
        )
    }

    #[test]
    fn publish_is_write_once_and_verified() {
        block_on_inline(async {
            let mut cache = ArtifactCache::new(MemoryCache::default());
            let artifact_key = key(b"jar");
            cache.publish(&artifact_key, b"jar").await.unwrap();
            cache.publish(&artifact_key, b"jar").await.unwrap();
            assert_eq!(
                cache.lookup(&artifact_key).await.unwrap(),
                Some(b"jar".to_vec())
            );
            assert_eq!(
                cache.publish(&artifact_key, b"other").await,
                Err(CacheError::Conflict)
            );
            assert_eq!(
                cache.publish(&key(b"missing"), b"other").await,
                Err(CacheError::DigestMismatch)
            );
        });
    }

    #[test]
    fn open_verified_returns_a_rewound_reader() {
        block_on_inline(async {
            let mut cache = ArtifactCache::new(MemoryCache::default());
            let artifact_key = key(b"jar-bytes");
            cache.publish(&artifact_key, b"jar-bytes").await.unwrap();
            let mut reader = cache.open_verified(&artifact_key).await.unwrap().unwrap();
            let mut out = alloc::vec![0u8; 9];
            io::Read::read_exact(&mut reader, &mut out).await.unwrap();
            assert_eq!(out, b"jar-bytes");
            assert!(
                cache
                    .open_verified(&key(b"missing"))
                    .await
                    .unwrap()
                    .is_none()
            );
        });
    }

    #[test]
    fn open_verified_rejects_corrupt_entries_structurally() {
        block_on_inline(async {
            let mut cache = ArtifactCache::new(MemoryCache::default());
            let artifact_key = key(b"jar-bytes");
            cache.publish(&artifact_key, b"jar-bytes").await.unwrap();
            cache
                .backend
                .entries
                .insert(artifact_key.clone(), Arc::from(&b"tampered"[..]));
            assert!(matches!(
                cache.open_verified(&artifact_key).await,
                Err(CacheError::Corrupt)
            ));
        });
    }

    #[test]
    fn bounded_lookup_rejects_before_materializing_oversized_artifacts() {
        block_on_inline(async {
            let mut cache = ArtifactCache::new(MemoryCache::default());
            let artifact_key = key(b"oversized");
            cache.publish(&artifact_key, b"oversized").await.unwrap();

            assert_eq!(
                cache.lookup_bounded(&artifact_key, 4).await,
                Err(CacheError::TooLarge { size: 9, limit: 4 })
            );
            assert_eq!(
                cache.lookup_bounded(&artifact_key, 9).await.unwrap(),
                Some(b"oversized".to_vec())
            );
        });
    }

    #[test]
    fn of_reader_matches_of_for_multi_chunk_input() {
        block_on_inline(async {
            let bytes = alloc::vec![7u8; 200 * 1024];
            let mut reader = io::Cursor::new(bytes.as_slice());
            assert_eq!(
                ContentDigest::of_reader(&mut reader).await.unwrap(),
                ContentDigest::of(&bytes)
            );
        });
    }
}
