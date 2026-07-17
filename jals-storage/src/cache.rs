use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use sha2::{Digest, Sha256};

use crate::error::CacheError;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
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
    entries: BTreeMap<CacheKey, Vec<u8>>,
    index: BTreeMap<(CacheNamespace, ContentDigest), ContentDigest>,
}

impl private::Sealed for MemoryCache {}

impl CacheBackend for MemoryCache {
    fn load(&self, key: &CacheKey) -> core::result::Result<Option<Vec<u8>>, CacheError> {
        Ok(self.entries.get(key).cloned())
    }

    fn publish_once(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
    ) -> core::result::Result<(), CacheError> {
        match self.entries.get(key) {
            Some(existing) if existing == bytes => Ok(()),
            Some(_) => Err(CacheError::Conflict),
            None => {
                self.entries.insert(key.clone(), bytes.to_vec());
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
}
