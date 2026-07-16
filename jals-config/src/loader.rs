//! The shared config loader: a single [`ConfigError`] and the [`DiscoverableConfig`] trait whose
//! provided `load` / `discover` back every config file's `from_file` / `discover`.
//!
//! `jalsfmt.toml` and `jalslint.toml` historically carried byte-identical loaders (an `Io`/`Parse`
//! error, a read-and-parse `from_file`, and an upward-walking `discover`). They are unified here as
//! provided methods on a trait any `Deserialize` config model implements by naming its file (plus
//! `Default` for `discover`'s "no file found" branch), so a future config language server can drive
//! them for any schema.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;

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
            path: path.to_owned(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
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

    /// Whether `path` (a `/`-separated virtual path) names this config file — its final component
    /// equals [`FILE_NAME`](Self::FILE_NAME). Hosts use this to decide which discovery cache a
    /// changed file invalidates.
    fn is_config_filename(path: &str) -> bool {
        jals_fs::path::VPath::file_name(path) == Some(Self::FILE_NAME)
    }
}

/// Per-directory memoized config discovery, with an optional explicit override.
///
/// This is the reusable discovery state every host needs: an explicit config (a `--config` flag)
/// short-circuits discovery entirely; otherwise each directory's upward
/// [`discover`](DiscoverableConfig::discover) walk runs once and is memoized until
/// [`clear`](Self::clear). Invalidation *policy* stays with the host — the CLI never invalidates
/// (one process, one run), the LSP clears on a watched config-file change.
pub struct ConfigResolver<C> {
    explicit: Option<C>,
    cache: BTreeMap<String, C>,
}

impl<C> Default for ConfigResolver<C> {
    fn default() -> Self {
        Self::new(None)
    }
}

impl<C> ConfigResolver<C> {
    /// A resolver that answers every directory with `explicit` when given, and discovers (then
    /// memoizes) per directory otherwise.
    pub const fn new(explicit: Option<C>) -> Self {
        Self {
            explicit,
            cache: BTreeMap::new(),
        }
    }

    /// Forget all memoized configs, e.g. after a config file changes on disk. Discovery reruns
    /// lazily on the next request. The explicit override, if any, is kept.
    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

impl<C: DiscoverableConfig + Clone + Default> ConfigResolver<C> {
    /// The config governing `dir` (a `/`-separated virtual path): the explicit override, the
    /// memoized discovery for `dir`, or a fresh upward [`discover`](DiscoverableConfig::discover)
    /// walk through `fs`.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a discovered file cannot be read or parsed.
    pub fn for_dir(&mut self, fs: &dyn FileTree, dir: &str) -> Result<C, ConfigError> {
        if let Some(cfg) = &self.explicit {
            return Ok(cfg.clone());
        }
        if let Some(cfg) = self.cache.get(dir) {
            return Ok(cfg.clone());
        }
        let cfg = C::discover(fs, dir)?;
        self.cache.insert(dir.to_owned(), cfg.clone());
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use jals_fs::{FileTree, InMemoryFileTree};

    use super::*;
    use crate::fmt::Config;

    #[test]
    fn resolver_explicit_override_wins_over_discovery() {
        let fs = InMemoryFileTree::new().with_file("/p/jalsfmt.toml", "indent-width = 7");
        let explicit = Config {
            indent_width: 3,
            ..Config::default()
        };
        let mut resolver = ConfigResolver::new(Some(explicit));
        assert_eq!(resolver.for_dir(&fs, "/p").unwrap().indent_width, 3);
    }

    #[test]
    fn resolver_memoizes_until_cleared() {
        let mut fs = InMemoryFileTree::new().with_file("/p/jalsfmt.toml", "indent-width = 7");
        let mut resolver = ConfigResolver::<Config>::default();
        assert_eq!(resolver.for_dir(&fs, "/p").unwrap().indent_width, 7);

        // The cached config survives an edit in the tree until the cache is cleared.
        fs.write("/p/jalsfmt.toml", b"indent-width = 3").unwrap();
        assert_eq!(resolver.for_dir(&fs, "/p").unwrap().indent_width, 7);

        resolver.clear();
        assert_eq!(resolver.for_dir(&fs, "/p").unwrap().indent_width, 3);
    }

    #[test]
    fn resolver_discovers_upward_and_defaults_without_a_file() {
        let fs = InMemoryFileTree::new()
            .with_file("/p/jalsfmt.toml", "indent-width = 7")
            .with_file("/q/A.java", "class A {}");
        let mut resolver = ConfigResolver::<Config>::default();
        assert_eq!(
            resolver.for_dir(&fs, "/p/src/deep").unwrap().indent_width,
            7
        );
        assert_eq!(
            resolver.for_dir(&fs, "/q").unwrap(),
            Config::default(),
            "no config anywhere upward falls back to the default"
        );
    }

    #[test]
    fn resolver_propagates_a_parse_error() {
        let fs = InMemoryFileTree::new().with_file("/p/jalsfmt.toml", "indent-width = ");
        let mut resolver = ConfigResolver::<Config>::default();
        assert!(matches!(
            resolver.for_dir(&fs, "/p"),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn is_config_filename_matches_only_the_final_component() {
        assert!(Config::is_config_filename("/p/jalsfmt.toml"));
        assert!(Config::is_config_filename("jalsfmt.toml"));
        assert!(!Config::is_config_filename("/p/other.toml"));
        assert!(!Config::is_config_filename("/p/jalsfmt.toml/nested"));
        assert!(crate::lint::Config::is_config_filename("/p/jalslint.toml"));
        assert!(!crate::lint::Config::is_config_filename("/p/jalsfmt.toml"));
    }
}
