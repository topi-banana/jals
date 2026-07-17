use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use sha2::{Digest, Sha256};

use crate::error::CacheError;
use crate::io::{self, IoError, Seek as _};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// Digest an entire reader by streaming fixed-size chunks, never materializing the content.
    pub fn of_reader<R: io::Read>(reader: &mut R) -> core::result::Result<Self, IoError> {
        let mut hasher = Sha256::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut chunk)? {
                0 => return Ok(Self(hasher.finalize().into())),
                n => hasher.update(&chunk[..n]),
            }
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
/// `Sync` is part of the sealed contract: a content-addressed, read-mostly cache is shareable
/// across threads by nature, so verified `lookup`s can fan out to concurrent readers.
pub trait CacheBackend: private::Sealed + Sync {
    /// Owning, cheap-to-clone reader over one stored artifact. Every clone reads at an
    /// independent position: the parallel archive walkers clone one open archive per worker
    /// and interleave reads.
    #[doc(hidden)]
    type Reader: io::Read + io::Seek + Clone + Send + Sync;
    #[doc(hidden)]
    fn open(&self, key: &CacheKey) -> core::result::Result<Option<Self::Reader>, CacheError>;
    #[doc(hidden)]
    fn load(&self, key: &CacheKey) -> core::result::Result<Option<Vec<u8>>, CacheError>;
    #[doc(hidden)]
    fn publish_once(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
    ) -> core::result::Result<(), CacheError>;
    #[doc(hidden)]
    fn load_index(
        &self,
        namespace: CacheNamespace,
        provenance: &ContentDigest,
    ) -> core::result::Result<Option<ContentDigest>, CacheError>;
    #[doc(hidden)]
    fn store_index(
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

    fn open(&self, key: &CacheKey) -> core::result::Result<Option<Self::Reader>, CacheError> {
        Ok(self
            .entries
            .get(key)
            .map(|bytes| io::Cursor::new(Arc::clone(bytes))))
    }

    fn load(&self, key: &CacheKey) -> core::result::Result<Option<Vec<u8>>, CacheError> {
        Ok(self.entries.get(key).map(|bytes| bytes.to_vec()))
    }

    fn publish_once(
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

    fn load_index(
        &self,
        namespace: CacheNamespace,
        provenance: &ContentDigest,
    ) -> core::result::Result<Option<ContentDigest>, CacheError> {
        Ok(self.index.get(&(namespace, *provenance)).copied())
    }

    fn store_index(
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

    pub fn lookup(&self, key: &CacheKey) -> core::result::Result<Option<Vec<u8>>, CacheError> {
        let Some(bytes) = self.backend.load(key)? else {
            return Ok(None);
        };
        if ContentDigest::of(&bytes) != key.content {
            return Err(CacheError::Corrupt);
        }
        Ok(Some(bytes))
    }

    /// Reader-based verified lookup: stream the stored bytes through one digest pass, rewind,
    /// and hand the reader out — the artifact is never materialized in memory. `Ok(None)` is a
    /// miss; a digest mismatch is [`CacheError::Corrupt`]; a source failure is
    /// [`CacheError::Io`], never conflated with a miss. Verification pins the opened backend
    /// resource, so a post-verification swap of the stored location cannot redirect reads.
    pub fn open_verified(
        &self,
        key: &CacheKey,
    ) -> core::result::Result<Option<C::Reader>, CacheError> {
        fn io_failure(error: &IoError) -> CacheError {
            CacheError::Io(error.to_string())
        }
        let Some(mut reader) = self.backend.open(key)? else {
            return Ok(None);
        };
        let digest = ContentDigest::of_reader(&mut reader).map_err(|error| io_failure(&error))?;
        if digest != key.content {
            return Err(CacheError::Corrupt);
        }
        reader
            .seek(io::SeekFrom::Start(0))
            .map_err(|error| io_failure(&error))?;
        Ok(Some(reader))
    }

    pub fn publish(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
    ) -> core::result::Result<(), CacheError> {
        if ContentDigest::of(bytes) != key.content {
            return match self.backend.load(key)? {
                Some(existing) if existing != bytes => Err(CacheError::Conflict),
                _ => Err(CacheError::DigestMismatch),
            };
        }
        // Keys are content-addressed, so a stored artifact whose digest matches the key IS the
        // write-once winner: a warm publish returns without re-writing. This is not the
        // forbidden contains-then-write — the hit is verified, and a miss still goes through
        // the backend's atomic create-once, which arbitrates real races.
        if let Some(existing) = self.backend.load(key)?
            && ContentDigest::of(&existing) == key.content
        {
            return Ok(());
        }
        // The digest check above plus the backend's write-once winner comparison already
        // guarantee the stored bytes match `key`; no read-back verification is needed.
        self.backend.publish_once(key, bytes)
    }

    /// The full key most recently recorded for `(namespace, provenance)` through
    /// [`record_index`](Self::record_index), if any. The index is advisory recovery metadata —
    /// it lets a caller that knows only an artifact's provenance (say, a dependency locator
    /// with no pinned digest) rediscover the content half of the key. The artifact must still
    /// be read through verified [`lookup`](Self::lookup), which is what keeps a stale or
    /// tampered index entry harmless: it can cause a miss, never wrong bytes.
    pub fn indexed_key(
        &self,
        namespace: CacheNamespace,
        provenance: ContentDigest,
    ) -> core::result::Result<Option<CacheKey>, CacheError> {
        Ok(self
            .backend
            .load_index(namespace, &provenance)?
            .map(|content| CacheKey::new(namespace, provenance, content)))
    }

    /// Remember `key` as the current content for its `(namespace, provenance)` pair. Unlike
    /// artifact publication this is last-writer-wins by design: every racer records a digest
    /// its own verified artifact backs, so either outcome is valid.
    pub fn record_index(&mut self, key: &CacheKey) -> core::result::Result<(), CacheError> {
        self.backend
            .store_index(key.namespace(), &key.provenance(), &key.content())
    }

    pub const fn backend(&self) -> &C {
        &self.backend
    }
}

#[cfg(test)]
mod tests {
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
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let artifact_key = key(b"jar");
        cache.publish(&artifact_key, b"jar").unwrap();
        cache.publish(&artifact_key, b"jar").unwrap();
        assert_eq!(cache.lookup(&artifact_key).unwrap(), Some(b"jar".to_vec()));
        assert_eq!(
            cache.publish(&artifact_key, b"other"),
            Err(CacheError::Conflict)
        );
        assert_eq!(
            cache.publish(&key(b"missing"), b"other"),
            Err(CacheError::DigestMismatch)
        );
    }

    #[test]
    fn open_verified_returns_a_rewound_reader() {
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let artifact_key = key(b"jar-bytes");
        cache.publish(&artifact_key, b"jar-bytes").unwrap();
        let mut reader = cache.open_verified(&artifact_key).unwrap().unwrap();
        let mut out = alloc::vec![0u8; 9];
        io::Read::read_exact(&mut reader, &mut out).unwrap();
        assert_eq!(out, b"jar-bytes");
        assert!(cache.open_verified(&key(b"missing")).unwrap().is_none());
    }

    #[test]
    fn open_verified_rejects_corrupt_entries_structurally() {
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let artifact_key = key(b"jar-bytes");
        cache.publish(&artifact_key, b"jar-bytes").unwrap();
        cache
            .backend
            .entries
            .insert(artifact_key.clone(), Arc::from(&b"tampered"[..]));
        assert!(matches!(
            cache.open_verified(&artifact_key),
            Err(CacheError::Corrupt)
        ));
    }

    #[test]
    fn of_reader_matches_of_for_multi_chunk_input() {
        let bytes = alloc::vec![7u8; 200 * 1024];
        let mut reader = io::Cursor::new(bytes.as_slice());
        assert_eq!(
            ContentDigest::of_reader(&mut reader).unwrap(),
            ContentDigest::of(&bytes)
        );
    }
}
