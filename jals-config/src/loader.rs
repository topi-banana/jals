//! The shared config loader: a single [`ConfigError`] and the [`DiscoverableConfig`] trait whose
//! provided `load` / `discover` back every config file's `from_file` / `discover`.
//!
//! `jalsfmt.toml` and `jalslint.toml` historically carried byte-identical loaders (an `Io`/`Parse`
//! error, a read-and-parse `from_file`, and an upward-walking `discover`). They are unified here as
//! provided methods on a trait any `Deserialize` config model implements by naming its file (plus
//! `Default` for `discover`'s "no file found" branch), so a future config language server can drive
//! them for any schema.

use jals_storage::{DirKey, FileKey, Name, ProjectView};
use serde::Deserialize;

/// An error loading or parsing a config file. It carries a typed [`FileKey`] and wraps either a
/// [`jals_storage::Error`] or [`toml::de::Error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The file could not be read.
    Io {
        /// The path that failed to read.
        path: FileKey,
        /// The underlying filesystem error.
        source: jals_storage::Error,
    },
    /// The file contained invalid TOML.
    Parse {
        /// The path that failed to parse.
        path: FileKey,
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

/// A config model loaded from and discovered upward through one immutable project revision by its
/// well-known file name.
///
/// Implementors only name their file ([`FILE_NAME`](Self::FILE_NAME)); `load` / `discover` are
/// provided.
pub trait DiscoverableConfig: Sized + for<'de> Deserialize<'de> {
    /// The file name this config is discovered by (e.g. `jalsfmt.toml`).
    const FILE_NAME: &'static str;

    /// Load and parse the typed config file from `view`.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the file cannot be read or contains invalid TOML.
    fn load(view: &ProjectView, path: &FileKey) -> Result<Self, ConfigError> {
        let text = view.file_text(path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        Self::from_text(path, text)
    }

    /// Parse the typed config from already-read file text. Host adapters that read a config file
    /// directly (the CLI and LSP discovery walks) share the parse and error shape through this.
    ///
    /// # Errors
    /// Returns [`ConfigError::Parse`] when the text is not this config's valid TOML.
    fn from_text(path: &FileKey, text: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.clone(),
            source,
        })
    }

    /// Search upward from `start_dir` (a `/`-separated virtual path) for
    /// [`FILE_NAME`](Self::FILE_NAME), read through `fs`.
    ///
    /// Returns the parsed config if a file is found, otherwise `Self::default()`. The walk
    /// cooperates per ancestor step, so deep trees never monopolize the executor.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a discovered file cannot be read or parsed.
    #[allow(async_fn_in_trait)]
    async fn discover(view: &ProjectView, start_dir: &DirKey) -> Result<Self, ConfigError>
    where
        Self: Default,
    {
        let name = Name::new(Self::FILE_NAME).expect("config file constants are valid names");
        let mut yielder = jals_exec::Yielder::new();
        for dir in start_dir.ancestors() {
            let candidate = dir.file(name.clone());
            if view.tree().file(&candidate).is_some() {
                return Self::load(view, &candidate);
            }
            yielder.tick().await;
        }
        Ok(Self::default())
    }
}

#[cfg(test)]
mod tests {
    use jals_storage::{CodeTree, Entry, FileKey, MemoryStorage};

    use super::*;
    use crate::fmt::Config;

    fn storage(files: &[(&str, &str)]) -> MemoryStorage {
        let entries = files.iter().map(|(path, text)| {
            Entry::File(FileKey::parse(path).unwrap(), text.as_bytes().to_vec())
        });
        MemoryStorage::memory(CodeTree::new(entries).unwrap())
    }

    #[test]
    fn discover_walks_upward_and_defaults_without_a_file() {
        let fs = storage(&[
            ("p/jalsfmt.toml", "indent-width = 7"),
            ("q/A.java", "class A {}"),
        ]);
        assert_eq!(
            jals_exec::block_on_inline(Config::discover(
                &fs.view(),
                &DirKey::parse("p/src/deep").unwrap()
            ))
            .unwrap()
            .indent_width,
            7
        );
        assert_eq!(
            jals_exec::block_on_inline(Config::discover(&fs.view(), &DirKey::parse("q").unwrap()))
                .unwrap(),
            Config::default(),
            "no config anywhere upward falls back to the default"
        );
    }

    #[test]
    fn discover_propagates_a_parse_error() {
        let fs = storage(&[("p/jalsfmt.toml", "indent-width = ")]);
        assert!(matches!(
            jals_exec::block_on_inline(Config::discover(&fs.view(), &DirKey::parse("p").unwrap())),
            Err(ConfigError::Parse { .. })
        ));
    }
}
