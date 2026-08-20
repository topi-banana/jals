#![cfg_attr(not(any(feature = "native", test)), no_std)]
//! Deterministic classpath resolution over revisioned project storage.
//!
//! The portable API contains no host paths and no existence predicates. Project files are addressed
//! by typed keys and read from an immutable [`jals_storage::ProjectView`]; generated, downloaded, and
//! extracted bytes are addressed by SHA-256-backed [`jals_storage::CacheKey`] values. Archive support
//! (`archive`) decodes jars in-house over the portable [`jals_storage::io`] byte streams, so it is
//! `no_std + alloc` and wasm-safe; it still operates on bytes, never paths.

extern crate alloc;

mod io;
mod resolve;
mod skeleton;

#[cfg(feature = "archive")]
mod jar;
#[cfg(feature = "archive")]
mod load;
#[cfg(feature = "archive")]
mod mappings;
#[cfg(feature = "native")]
mod native;
#[cfg(feature = "archive")]
mod project;
#[cfg(feature = "archive")]
mod remap;
#[cfg(feature = "archive")]
mod zip;

pub use io::{Fetcher, NetworkPolicy};
pub use resolve::{
    DependencyLocation, DependencySpec, ExpectedDigest, ExternalArtifactSpec, ExternalLocator,
    MappingLocation, MappingSpec, ResolvedDependencies, ResolvedJar, dependency_resolver,
    external_artifact_resolver, mapping_resolver,
};
pub use skeleton::{SkeletonGroup, SkeletonMode, Skeletons};

#[cfg(feature = "archive")]
pub use jar::write as write_jar;
#[cfg(feature = "archive")]
pub use load::{
    CachedJar, ClasspathCoverage, ClasspathEntry, ClasspathLoad, JarExtraction, SourceTree,
    SourceTreeLimits, source_tree_extraction,
};
#[cfg(feature = "native")]
pub use native::{NativeProjectPlan, ReqwestFetcher};
#[cfg(feature = "archive")]
pub use project::{
    MemoryProjectPlan, ProjectInputOptions, ProjectInputPlan, ProjectInputs, SourceFile,
};
#[cfg(feature = "archive")]
pub use remap::{RemapRequest, jar_merge, jar_remap, nested_jar};

use alloc::string::String;
use core::fmt;

use jals_storage::{CacheKey, DirKey, FileKey, RelativePath};

/// Which way a mapping file is applied.
///
/// The pair of namespaces is fixed by the file; this says which of them the jar being remapped is
/// written in. There is deliberately no default: a jar in the wrong namespace remaps to nothing at
/// all and produces a *plausible* archive, so the direction is something a caller states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemapDirection {
    /// Obfuscated → official: a shipped library becomes the names a project is written against.
    Deobfuscate,
    /// Official → obfuscated: a project's own output becomes the names its runtime loads.
    Reobfuscate,
}

impl RemapDirection {
    /// The direction's cache identity. A stable string rather than the enum discriminant, for the
    /// reason every other tag in the workspace is one.
    ///
    /// Private to the crate root, so the remapper below reaches it and nothing outside can key a
    /// cache on the tag independently. It carries `archive`'s gate because the fold that reads it is
    /// the remapper, which only exists there — an ungated declaration would have no caller in the
    /// portable configuration.
    #[cfg(feature = "archive")]
    const fn tag_name(self) -> &'static str {
        match self {
            Self::Deobfuscate => "deobfuscate",
            Self::Reobfuscate => "reobfuscate",
        }
    }
}

/// Which grammar a mapping text is written in.
///
/// Two variants today. It exists as an enum so that adding tsrg/enigma is a new arm here and in
/// [`Mappings::parse`], rather than a second entry point every caller has to learn about.
///
/// A format that names more than two namespaces carries the pair it is read through *inside* the
/// variant rather than beside it. A namespace pair is meaningless for ProGuard-style text — whose file
/// describes exactly one pair — and mandatory for tiny v2, so a field on the enum would be a value
/// half its inhabitants must ignore, and a caller could pair one format with the other's selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MappingFormat {
    /// The ProGuard-style text Mojang publishes per Minecraft release.
    Proguard,
    /// The tab-separated tiny v2 text Fabric publishes, read through one pair of its namespaces.
    ///
    /// A tiny v2 file names two or more namespaces (`official intermediary named`), so *which* pair
    /// is being read is not a property of the file. There is deliberately no default, for the same
    /// reason [`RemapDirection`] has none: the wrong pair remaps to nothing at all and still
    /// produces a plausible archive.
    TinyV2 {
        /// The namespace a [`RemapDirection::Deobfuscate`] reads names *from*.
        ///
        /// Named for that direction rather than for either namespace's role, because the file is
        /// symmetric: a [`RemapDirection::Reobfuscate`] reads the same pair the other way. For a
        /// Fabric jar this is typically `official` (the shipped, obfuscated names).
        from: String,
        /// The namespace a [`RemapDirection::Deobfuscate`] writes names *to* — typically `named`.
        to: String,
    },
}

impl MappingFormat {
    /// The format's cache identity. Scoped and gated like `RemapDirection::tag_name` above, and for
    /// the same reason: the remapper's provenance fold is its only reader.
    #[cfg(feature = "archive")]
    const fn tag_name(&self) -> &'static str {
        match self {
            Self::Proguard => "proguard",
            Self::TinyV2 { .. } => "tiny-v2",
        }
    }

    /// Absorb the whole format into a provenance fold: the tag, plus every value that selects a
    /// different renaming from the same mapping text.
    ///
    /// This lives here rather than at the fold site so the match is exhaustive over the enum. The
    /// tag alone was the entire contribution while there was one variant; leaving it that way would
    /// make `official→named` and `official→intermediary` over one tiny file share a cache key, and
    /// the second remap would be served the first one's jar. `ProvenanceFold::bytes` is
    /// length-framed, so no pair of namespace names can collide with another pair.
    #[cfg(feature = "archive")]
    fn fold_into(&self, fold: &mut jals_storage::ProvenanceFold) {
        fold.bytes(self.tag_name().as_bytes());
        match self {
            Self::Proguard => {}
            Self::TinyV2 { from, to } => {
                fold.bytes(from.as_bytes()).bytes(to.as_bytes());
            }
        }
    }
}

/// A navigation source stored as a verified cache artifact: an extracted `sources`-jar member, a
/// published Git checkout file, or a synthesized skeleton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySource {
    pub path: RelativePath,
    pub key: CacheKey,
}

/// Typed attribution for a non-fatal classpath diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarningOrigin {
    ProjectFile(FileKey),
    ProjectDirectory(DirKey),
    Artifact(CacheKey),
    External(ExternalLocator),
    Skeleton,
}

/// One advisory resolution, archive, parsing, or generation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub origin: WarningOrigin,
    pub message: String,
}

impl Warning {
    pub fn new(origin: WarningOrigin, message: impl Into<String>) -> Self {
        Self {
            origin,
            message: message.into(),
        }
    }
}

/// The location a host reports a warning against.
///
/// Many messages name no location at all — `classpath artifact is not cached` and `unrecognized
/// classpath file` carry their subject only in the origin — so a host that renders the message
/// alone drops the one piece of a warning a user can act on. That is why this rendering lives here
/// instead of in each host: the alternative is every producer restating its locator inside its own
/// message, which is the same fact written twice.
impl fmt::Display for WarningOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectFile(key) => write!(f, "`{key}`"),
            Self::ProjectDirectory(key) => write!(f, "`{key}`"),
            // A cached artifact has no name a user wrote, so this names it the way the cache does.
            // The content digest is truncated because it is here to be recognized, not resolved.
            Self::Artifact(key) => write!(
                f,
                "cached {:?} {:.12}",
                key.namespace(),
                key.content().to_hex()
            ),
            // Verbatim, and deliberately not as a path: an external locator is whatever the user
            // wrote — a URL, a host path, or a `[build]` entry that turned out to be neither.
            Self::External(locator) => write!(f, "`{locator}`"),
            Self::Skeleton => f.write_str("generated source"),
        }
    }
}

/// `<origin>: <message>` — what a host prints. Every host that reports these renders them through
/// this, so the attribution a producer chose is the attribution a user sees.
impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.origin, self.message)
    }
}
