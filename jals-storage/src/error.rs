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
    InvalidName(NameError),
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
    Io(String),
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
