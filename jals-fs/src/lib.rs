#![cfg_attr(not(any(feature = "std", test)), no_std)]
//! A minimal, synchronous virtual-filesystem abstraction shared by the pure analysis crates.
//!
//! The workspace's file access used to be wired directly to `std::fs`/`std::path`, which does not
//! exist on `no_std`/`wasm32` targets. This crate introduces one interface — [`FileTree`] — with
//! two self-contained implementations:
//!
//! - [`InMemoryFileTree`]: the whole file tree lives in memory. Pure `core`/`alloc`, so it builds
//!   for `wasm32`/`no_std` and is a drop-in for tests and browser hosts.
//! - [`OsFileTree`]: the same interface backed by synchronous `std::fs` I/O against the real
//!   filesystem. Host-only, gated behind the off-by-default `std` feature.
//!
//! Paths are **UTF-8, `/`-separated virtual paths** ([`&str`]): `std::path::Path`/`PathBuf` have no
//! `core`/`alloc` equivalent, so a `no_std` file tree cannot use them. For [`OsFileTree`] a virtual
//! path *is* the OS path string (`Path::new(s)`); free helpers in [`path`] navigate them the way
//! `std::path::Path` navigates real paths.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

mod error;
mod mem;
pub mod path;

#[cfg(any(feature = "std", test))]
mod os;

pub use error::{FsError, Result};
pub use mem::InMemoryFileTree;

#[cfg(any(feature = "std", test))]
pub use os::OsFileTree;

/// Synchronous access to a tree of files addressed by UTF-8, `/`-separated virtual paths.
///
/// The trait is deliberately object-safe (every method uses only `&self`/`&mut self` receivers and
/// concrete `&str`/`Vec`/`String` types), so it is usable both as `&dyn FileTree` (the config
/// loaders) and as a generic bound `F: FileTree` (the LSP workspace). The read methods take
/// `&self`; only [`write`](FileTree::write) needs `&mut self`.
///
/// # Path convention
/// Paths are `/`-separated UTF-8. A trailing `/` is ignored, `""` and `"/"` both denote the root,
/// and `.`/`..` are **not** resolved — callers pass already-resolved paths (the config loaders and
/// the LSP hold canonical absolute paths). Directory listings return **full** virtual paths, sorted
/// lexicographically for deterministic output.
pub trait FileTree {
    /// Read the file at `path` as UTF-8 text.
    fn read_to_string(&self, path: &str) -> Result<String>;

    /// Read the raw bytes of the file at `path`.
    fn read(&self, path: &str) -> Result<Vec<u8>>;

    /// Whether `path` exists and is a regular file.
    fn is_file(&self, path: &str) -> bool;

    /// Whether `path` exists and is a directory.
    fn is_dir(&self, path: &str) -> bool;

    /// The immediate children of the directory `path`, as full virtual paths, sorted
    /// lexicographically. Returns [`FsError::NotADirectory`] when `path` is not a directory.
    fn read_dir(&self, path: &str) -> Result<Vec<String>>;

    /// Every regular file at or below `root` (recursive) whose extension equals `ext` (without the
    /// dot, e.g. `"java"`), as full virtual paths, sorted lexicographically. A missing/unreadable
    /// `root` yields an empty vec rather than an error, mirroring the LSP's tolerant walk.
    fn walk_ext(&self, root: &str, ext: &str) -> Result<Vec<String>>;

    /// Write `contents` to `path`, creating or **atomically** replacing the file (and, for the OS
    /// impl, any missing parent directories). Atomic means a concurrent or later reader observes
    /// either the old file or the fully-written new one, never a truncated intermediate — so "the
    /// file exists" implies it is complete, which callers rely on for skip-if-exists caching.
    /// [`InMemoryFileTree`] is atomic by construction (a single map insert); [`OsFileTree`] writes a
    /// temp sibling and renames it into place. Takes `&mut self` so [`InMemoryFileTree`] needs no
    /// interior mutability.
    fn write(&mut self, path: &str, contents: &[u8]) -> Result<()>;
}
