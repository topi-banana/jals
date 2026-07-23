#![cfg_attr(not(any(feature = "std", feature = "std-io", test)), no_std)]
//! Deterministic, revisioned project storage.
//!
//! The portable Interface contains only validated project-relative keys and immutable snapshots.
//! Host paths and live filesystem I/O are confined to the `std`-gated native Adapter.

extern crate alloc;

mod cache;
mod error;
pub mod io;
#[cfg(any(feature = "std", test))]
mod native;
mod storage;
mod tree;
mod value;

pub use cache::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, MemoryCache,
    ProvenanceFold,
};
pub use error::{CacheError, Diagnostic, Error, NameError, PathError, Result, TreeError};
#[cfg(any(feature = "std", test))]
pub use native::{NativeArtifactReader, NativeCache, NativeScope, NativeSource};
pub use storage::{
    Change, MemorySource, ProjectStorage, ProjectView, RefreshOutcome, SourceBackend, Transaction,
};
pub use tree::{CodeFile, CodeTree, Entry, EntryRef};
pub use value::{DirKey, FileKey, Name, RelativePath, Revision};

pub type MemoryStorage = ProjectStorage<MemorySource, MemoryCache>;
#[cfg(any(feature = "std", test))]
pub type NativeStorage = ProjectStorage<NativeSource, NativeCache>;
