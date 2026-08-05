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

pub use io::Fetcher;
pub use resolve::{
    DependencyLocation, DependencyResolver, DependencySpec, ExpectedDigest,
    ExternalArtifactResolver, ExternalArtifactSpec, ExternalLocator, MappingLocation,
    MappingResolver, MappingSpec, NetworkPolicy, ResolvedDependencies, ResolvedJar,
};
pub use skeleton::{SkeletonGroup, SkeletonMode, Skeletons};

#[cfg(feature = "archive")]
pub use jar::JarPackage;
#[cfg(feature = "archive")]
pub use load::{
    CachedJar, ClasspathCoverage, ClasspathEntry, ClasspathLoad, JarExtraction, SourceTree,
    SourceTreeExtraction, SourceTreeLimits,
};
#[cfg(feature = "native")]
pub use native::{NativeProjectPlan, ReqwestFetcher};
#[cfg(feature = "archive")]
pub use project::{
    MemoryProjectPlan, ProjectInputOptions, ProjectInputPlan, ProjectInputs, SourceFile,
};
#[cfg(feature = "archive")]
pub use remap::{JarMerge, JarRemap, NestedJar, RemapRequest};

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
/// One variant today. It exists as an enum so that adding tiny/tsrg/enigma is a new arm here and in
/// [`Mappings::parse`], rather than a second entry point every caller has to learn about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MappingFormat {
    /// The ProGuard-style text Mojang publishes per Minecraft release.
    Proguard,
}

impl MappingFormat {
    /// The format's cache identity. Scoped and gated like `RemapDirection::tag_name` above, and for
    /// the same reason: the remapper's provenance fold is its only reader.
    #[cfg(feature = "archive")]
    const fn tag_name(self) -> &'static str {
        match self {
            Self::Proguard => "proguard",
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
