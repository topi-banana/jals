use alloc::string::String;
use core::fmt;

use crate::{DirKey, FileKey, Revision};

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Empty,
    CurrentDirectory,
    ParentDirectory,
    Separator,
    ControlCharacter,
    WindowsReservedCharacter,
    WindowsReservedSuffix,
    WindowsReservedName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    Absolute,
    Drive,
    Unc,
    FileIsRoot,
    /// A directory key that must name a real subdirectory resolved to the project root itself.
    DirectoryIsRoot,
    /// A `..` segment climbed above the project root. Only
    /// [`RelativePath::resolve`](crate::RelativePath::resolve) folds `..`, so only it raises this.
    Escape,
    InvalidName(NameError),
}

/// Rendered for a reader, because a declared `[build]` entry or dependency path is something they
/// wrote and can go and fix. Two crates used to spell these sentences themselves — the one place
/// they disagreed was whether escaping the root left "the project root" or "the root project tree",
/// which named the same rule twice.
impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absolute => f.write_str("path must be relative to the project root"),
            Self::Drive => f.write_str("path must not name a drive"),
            Self::Unc => f.write_str("path must not be a UNC path"),
            Self::FileIsRoot => f.write_str("path names the project root, not a file"),
            Self::DirectoryIsRoot => f.write_str("path names the project root, not a subdirectory"),
            Self::Escape => f.write_str("path leaves the project root"),
            // The one segment error worth its own sentence: a Windows-style path reaches here as a
            // separator inside a segment, and "invalid segment: Separator" would not say so.
            Self::InvalidName(NameError::Separator) => {
                f.write_str("path must use portable `/` separators")
            }
            Self::InvalidName(error) => write!(f, "path contains an invalid segment: {error:?}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeError {
    DuplicateFile(FileKey),
    DuplicateDirectory(DirKey),
    FileDirectoryCollision(FileKey),
    FileAncestor(FileKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    Conflict,
    Corrupt,
    DigestMismatch,
    TooLarge { size: u64, limit: usize },
    Io(String),
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict => {
                f.write_str("a concurrent write conflicted with this cache operation")
            }
            Self::Corrupt => f.write_str("a cached artifact is corrupt"),
            Self::DigestMismatch => {
                f.write_str("a cached artifact failed its content-digest check")
            }
            Self::TooLarge { size, limit } => {
                write!(
                    f,
                    "artifact is {size} bytes, over the cache's {limit}-byte limit"
                )
            }
            Self::Io(message) => write!(f, "cache I/O failed: {message}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    ExternalChangeShadowed(FileKey),
    NonUtf8Entry(String),
    SymlinkEscapesRoot(String),
    SymlinkCycle(String),
    UnreadableEntry(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidTree(TreeError),
    Cache(CacheError),
    StaleRevision {
        expected: Revision,
        actual: Revision,
    },
    NotFoundFile(FileKey),
    NotFoundDirectory(DirKey),
    ExpectedFile(DirKey),
    ExpectedDirectory(FileKey),
    AlreadyExistsFile(FileKey),
    AlreadyExistsDirectory(DirKey),
    InvalidUtf8(FileKey),
    /// A native mutation would overwrite a file whose on-disk content no longer matches the base
    /// snapshot it was planned against — an external write landed between snapshot and commit.
    /// Refused so the concurrent edit is not silently lost.
    ExternalConflict(FileKey),
    /// A native directory removal observed files or directories that differ from the transaction's
    /// base snapshot.
    ExternalDirectoryConflict(DirKey),
    Io(String),
}

impl From<TreeError> for Error {
    fn from(value: TreeError) -> Self {
        Self::InvalidTree(value)
    }
}
impl From<CacheError> for Error {
    fn from(value: CacheError) -> Self {
        Self::Cache(value)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl core::error::Error for Error {}
