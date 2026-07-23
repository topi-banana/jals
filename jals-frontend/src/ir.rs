//! What the driver hands a frontend, and what a frontend hands back.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use jals_storage::{CacheKey, CacheNamespace, ContentDigest, RelativePath};

use crate::level::IrLevel;

/// One input file, in canonical project order.
///
/// `bytes` is an `Arc` so the driver can hand the same buffer to several frontends, and to a
/// fan-out worker, without copying source text.
#[derive(Debug, Clone)]
pub struct IrFile {
    /// Project-relative logical path. Ordering by this — never by filesystem walk order — is
    /// what keeps digests identical across machines.
    pub path: RelativePath,
    pub bytes: Arc<[u8]>,
    pub digest: ContentDigest,
}

impl IrFile {
    pub fn new(path: RelativePath, bytes: Arc<[u8]>) -> Self {
        let digest = ContentDigest::of(&bytes);
        Self {
            path,
            bytes,
            digest,
        }
    }
}

/// The borrowed view a frontend receives. Exactly one variant per [`IrLevel`].
///
/// Modelling levels as an enum rather than as optional fields is what makes the contract
/// compile-checked: a `Bytes`-level frontend has no field to reach a project index through, so
/// observing more than it declared is not a discipline question.
#[derive(Debug)]
#[non_exhaustive]
pub enum Ir<'a> {
    Bytes { files: &'a [IrFile] },
}

impl Ir<'_> {
    pub const fn level(&self) -> IrLevel {
        match self {
            Self::Bytes { .. } => IrLevel::Bytes,
        }
    }

    pub const fn files(&self) -> &[IrFile] {
        match self {
            Self::Bytes { files } => files,
        }
    }
}

/// A frontend's diagnostic. Structured from the start so that adding spans later widens a type
/// nobody has to re-plumb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontendDiagnostic {
    pub severity: Severity,
    /// The *input* file the diagnostic is about, when the frontend can attribute it.
    pub file: Option<RelativePath>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// Where a span of emitted output came from in the input.
///
/// Empty for every frontend that does not rewrite (Vanilla emits its input verbatim, so output
/// offsets are input offsets). It exists in the type now because retrofitting origin tracking
/// after tools consume `FrontendOutput` is far more expensive than carrying an always-empty
/// `Vec`, and because `javac` reporting positions in *generated* text is the first thing a real
/// macro frontend must answer for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginSpan {
    pub generated: RelativePath,
    pub generated_offset: u32,
    pub generated_len: u32,
    pub source: RelativePath,
    pub source_offset: u32,
    pub source_len: u32,
}

/// What a frontend produces: Java source bytes at project-relative paths.
///
/// Bytes, not keys. A frontend never touches the cache — publishing is the driver's job, at one
/// boundary, exactly as the decompiler leaves publication to `jals-classpath`. This is also
/// forced: `ArtifactCache<C>` is generic over a non-object-safe backend, so it cannot appear in
/// a `&dyn Frontend` method at all.
#[derive(Debug, Default)]
pub struct FrontendOutput {
    pub files: Vec<(RelativePath, Vec<u8>)>,
    pub diagnostics: Vec<FrontendDiagnostic>,
    pub origins: Vec<OriginSpan>,
}

impl FrontendOutput {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

/// One published output file: a logical path plus the key its bytes live under.
///
/// Shaped after `jals_classpath::LibrarySource` — a lowered tree is passed around as a manifest
/// of digests, never as bytes in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredFile {
    pub path: RelativePath,
    pub key: CacheKey,
}

/// The frontend's published output tree — the intermediate currency a backend consumes.
///
/// Always sorted by `path` and deduplicated, so [`digest`](Self::digest) is a function of the
/// content alone and not of the order the files happened to be produced in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoweredTree {
    files: Vec<LoweredFile>,
}

impl LoweredTree {
    /// Build a tree, imposing canonical order. A duplicate path is a caller bug, not a
    /// last-one-wins merge, so it is reported rather than silently resolved.
    pub fn new(mut files: Vec<LoweredFile>) -> Result<Self, DuplicatePath> {
        files.sort_by(|left, right| left.path.cmp(&right.path));
        if let Some(window) = files.windows(2).find(|pair| pair[0].path == pair[1].path) {
            return Err(DuplicatePath(window[0].path.clone()));
        }
        Ok(Self { files })
    }

    pub fn files(&self) -> &[LoweredFile] {
        &self.files
    }

    pub const fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// This tree's identity: a length-framed fold over every `(path, provenance, content)` in
    /// sorted order.
    ///
    /// Folding each member's *provenance as well as its content* means the digest identifies
    /// how the tree was produced, not merely what bytes it holds — two trees with identical
    /// bytes from different frontends stay distinguishable.
    pub fn digest(&self) -> ContentDigest {
        let mut fold = jals_storage::ProvenanceFold::new(b"jals.frontend.tree\0");
        for file in &self.files {
            fold.bytes(file.path.to_string().as_bytes())
                .parent(&file.key);
        }
        fold.finish()
    }

    /// Encode the tree as a self-describing manifest.
    ///
    /// Restoring a lowering needs only this one blob: it carries every member's full key, so
    /// the edges are recovered without a side index. Layout, with all integers big-endian:
    ///
    /// Lengths are `u64` rather than `u32` so that encoding is total: a narrower width would
    /// have to either truncate — silently corrupting the manifest — or fail, and neither is
    /// worth the four bytes it saves.
    ///
    /// ```text
    /// b"jals.frontend.tree-v1\0"
    /// u64  member count
    /// per member, in sorted path order:
    ///   u64  path byte length
    ///   ..   path bytes
    ///   32   provenance digest
    ///   32   content digest
    /// ```
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(TREE_MAGIC.len() + 8 + self.files.len() * 88);
        out.extend_from_slice(TREE_MAGIC);
        out.extend_from_slice(&(self.files.len() as u64).to_be_bytes());
        for file in &self.files {
            let path = file.path.to_string();
            out.extend_from_slice(&(path.len() as u64).to_be_bytes());
            out.extend_from_slice(path.as_bytes());
            out.extend_from_slice(file.key.provenance().as_bytes());
            out.extend_from_slice(file.key.content().as_bytes());
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, TreeDecodeError> {
        let mut cursor = Cursor::new(bytes);
        if cursor.take(TREE_MAGIC.len())? != TREE_MAGIC {
            return Err(TreeDecodeError);
        }
        // A declared count from untrusted-shaped bytes must never drive an allocation, so the
        // manifest is grown as members are actually read rather than reserved up front.
        let count = cursor.length()?;
        let mut files = Vec::new();
        for _ in 0..count {
            let len = cursor.length()?;
            let path = core::str::from_utf8(cursor.take(len)?).map_err(|_| TreeDecodeError)?;
            let path = RelativePath::parse(path).map_err(|_| TreeDecodeError)?;
            let provenance = cursor.digest()?;
            let content = cursor.digest()?;
            files.push(LoweredFile {
                path,
                key: CacheKey::new(CacheNamespace::FrontendOutput, provenance, content),
            });
        }
        if !cursor.is_empty() {
            return Err(TreeDecodeError);
        }
        // Re-impose the invariant rather than trusting the bytes: a manifest is cache content,
        // and a verified read guarantees the bytes are what were written, not that whoever
        // wrote them upheld our ordering rule.
        Self::new(files).map_err(|_| TreeDecodeError)
    }
}

const TREE_MAGIC: &[u8] = b"jals.frontend.tree-v1\0";

struct Cursor<'a> {
    rest: &'a [u8],
}

impl<'a> Cursor<'a> {
    const fn new(rest: &'a [u8]) -> Self {
        Self { rest }
    }

    const fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    const fn take(&mut self, len: usize) -> Result<&'a [u8], TreeDecodeError> {
        if self.rest.len() < len {
            return Err(TreeDecodeError);
        }
        let (head, tail) = self.rest.split_at(len);
        self.rest = tail;
        Ok(head)
    }

    /// Read a `u64` length and narrow it to `usize`.
    ///
    /// On a 32-bit target a manifest written on a 64-bit one could name a length this platform
    /// cannot represent; that is a miss, not a truncation.
    fn length(&mut self) -> Result<usize, TreeDecodeError> {
        let bytes: [u8; 8] = self.take(8)?.try_into().map_err(|_| TreeDecodeError)?;
        usize::try_from(u64::from_be_bytes(bytes)).map_err(|_| TreeDecodeError)
    }

    fn digest(&mut self) -> Result<ContentDigest, TreeDecodeError> {
        let bytes: [u8; 32] = self.take(32)?.try_into().map_err(|_| TreeDecodeError)?;
        Ok(ContentDigest::from_bytes(bytes))
    }
}

/// A tree manifest could not be decoded. Treated as a cache miss, never as a build failure:
/// the worst case is recomputing a lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeDecodeError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicatePath(pub RelativePath);
