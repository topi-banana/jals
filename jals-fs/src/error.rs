//! The virtual-filesystem error type, hand-rolled to stay `no_std`.

use alloc::string::String;
use core::fmt;

/// The result of a fallible [`FileTree`](crate::FileTree) operation.
pub type Result<T> = core::result::Result<T, FsError>;

/// A failure from a [`FileTree`](crate::FileTree) operation.
///
/// Holds only owned `String`s (a rendered path or message) so the type stays `no_std` and is
/// `Clone + PartialEq + Eq`. [`OsFileTree`](crate::OsFileTree) maps each `std::io::Error` into one
/// of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    /// No file or directory exists at the path.
    NotFound(String),
    /// A directory operation targeted a path that is not a directory.
    NotADirectory(String),
    /// A file operation targeted a path that is not a regular file.
    NotAFile(String),
    /// The file's bytes were not valid UTF-8 (from
    /// [`read_to_string`](crate::FileTree::read_to_string)).
    InvalidUtf8(String),
    /// Any other host I/O failure, rendered to a message (keeps `FsError` `no_std`).
    Io(String),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(path) => write!(f, "no such file or directory: {path}"),
            Self::NotADirectory(path) => write!(f, "not a directory: {path}"),
            Self::NotAFile(path) => write!(f, "not a file: {path}"),
            Self::InvalidUtf8(path) => write!(f, "file is not valid UTF-8: {path}"),
            Self::Io(message) => write!(f, "I/O error: {message}"),
        }
    }
}

impl core::error::Error for FsError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn display_renders_each_variant() {
        assert_eq!(
            FsError::NotFound("/x".into()).to_string(),
            "no such file or directory: /x"
        );
        assert_eq!(
            FsError::NotADirectory("/x".into()).to_string(),
            "not a directory: /x"
        );
        assert_eq!(FsError::NotAFile("/x".into()).to_string(), "not a file: /x");
        assert_eq!(
            FsError::InvalidUtf8("/x".into()).to_string(),
            "file is not valid UTF-8: /x"
        );
        assert_eq!(FsError::Io("boom".into()).to_string(), "I/O error: boom");
    }
}
