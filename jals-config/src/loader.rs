//! The shared config loader: a single [`ConfigError`] and the [`DiscoverableConfig`] trait whose
//! provided `load` / `discover` back every config file's `from_file` / `discover`.
//!
//! `jalsfmt.toml` and `jalslint.toml` historically carried byte-identical loaders (an `Io`/`Parse`
//! error, a read-and-parse `from_file`, and an upward-walking `discover`). They are unified here as
//! provided methods on a trait any `Deserialize` config model implements by naming its file (plus
//! `Default` for `discover`'s "no file found" branch), so a future config language server can drive
//! them for any schema.

use alloc::string::{String, ToString};

use jals_fs::FileTree;
use serde::Deserialize;

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

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "failed to read config {path}: {source}")
            }
            Self::Parse { path, source } => {
                write!(f, "failed to parse config {path}: {source}")
            }
        }
    }
}

impl core::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
        }
    }
}

/// A config model that is loaded from — and discovered upward through — a [`FileTree`] by its
/// well-known file name.
///
/// Implementors only name their file ([`FILE_NAME`](Self::FILE_NAME)); `load` / `discover` are
/// provided. The config structs keep inherent `from_file` / `discover` wrappers with the same
/// signatures, so consumers do not need this trait in scope.
pub trait DiscoverableConfig: Sized + for<'de> Deserialize<'de> {
    /// The file name this config is discovered by (e.g. `jalsfmt.toml`).
    const FILE_NAME: &'static str;

    /// Load and parse the config file at `path`, read through `fs`.
    ///
    /// `fs` is any [`FileTree`] — a [`jals_fs::OsFileTree`] on the host, or a
    /// [`jals_fs::InMemoryFileTree`] for wasm / tests; `path` is a `/`-separated virtual path.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the file cannot be read or contains invalid TOML.
    fn load(fs: &dyn FileTree, path: &str) -> Result<Self, ConfigError> {
        let text = fs.read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_string(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_string(),
            source,
        })
    }

    /// Search upward from `start_dir` (a `/`-separated virtual path) for
    /// [`FILE_NAME`](Self::FILE_NAME), read through `fs`.
    ///
    /// Returns the parsed config if a file is found, otherwise `Self::default()`.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a discovered file cannot be read or parsed.
    fn discover(fs: &dyn FileTree, start_dir: &str) -> Result<Self, ConfigError>
    where
        Self: Default,
    {
        let mut dir = Some(start_dir);
        while let Some(d) = dir {
            let candidate = jals_fs::path::VPath::join(d, Self::FILE_NAME);
            if fs.is_file(&candidate) {
                return Self::load(fs, &candidate);
            }
            dir = jals_fs::path::VPath::parent(d);
        }
        Ok(Self::default())
    }
}
