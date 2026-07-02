//! The error type returned when loading or parsing a `jalsfmt.toml` file.

use alloc::string::String;
use core::fmt;

/// An error loading or parsing a config file. `no_std`: it holds a rendered path `String` and wraps
/// [`jals_fs::FsError`] (the read failure) or [`toml::de::Error`] (the parse failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The file could not be read.
    Io {
        /// The path that failed to read.
        path: String,
        /// The underlying filesystem error.
        source: jals_fs::FsError,
    },
    /// The file contained invalid TOML.
    Parse {
        /// The path that failed to parse.
        path: String,
        /// The underlying parse error.
        source: toml::de::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io { path, source } => {
                write!(f, "failed to read config {path}: {source}")
            }
            ConfigError::Parse { path, source } => {
                write!(f, "failed to parse config {path}: {source}")
            }
        }
    }
}

impl core::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            ConfigError::Io { source, .. } => Some(source),
            ConfigError::Parse { source, .. } => Some(source),
        }
    }
}
