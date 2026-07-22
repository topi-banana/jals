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
    ExternalArtifactResolver, ExternalArtifactSpec, ExternalLocator, NetworkPolicy,
    ResolvedDependencies, ResolvedJar,
};
pub use skeleton::{SkeletonGroup, SkeletonMode, Skeletons};

#[cfg(feature = "archive")]
pub use load::{
    CachedJar, ClasspathEntry, ClasspathLoad, JarExtraction, SourceTree, SourceTreeExtraction,
    SourceTreeLimits,
};
#[cfg(feature = "archive")]
pub use mappings::Mappings;
#[cfg(feature = "native")]
pub use native::{NativeProjectPlan, ReqwestFetcher};
#[cfg(feature = "archive")]
pub use project::{ProjectInputOptions, ProjectInputPlan, ProjectInputs, SourceFile};
#[cfg(feature = "archive")]
pub use remap::{JarMerge, JarRemap, NestedJar};

use alloc::string::String;
use jals_storage::{CacheKey, DirKey, FileKey, RelativePath};

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
