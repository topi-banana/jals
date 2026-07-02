//! Linting configuration, deserialized from `jalslint.toml`.
//!
//! Every key is optional; omitted keys fall back to [`Config::default`]. The only section is
//! `[rules]`, a map from rule name (kebab-case, e.g. `wildcard-import`) to a [`Severity`]
//! (`"allow"` / `"warn"` / `"error"`). A rule not listed uses its built-in default severity.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};

use jals_fs::FileTree;
use serde::Deserialize;

use crate::diagnostic::Severity;

pub use error::ConfigError;

/// Linter configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// Per-rule severity overrides. Keys are rule names (kebab-case); a missing key means the
    /// rule keeps its built-in default severity.
    pub rules: BTreeMap<String, Severity>,
    /// The project's target Java version (feature release, e.g. `24`), injected by the host from
    /// the manifest's `[package] edition` — **not** a `jalslint.toml` key (hence `serde(skip)`).
    ///
    /// It drives version-gated rules (`Checker::Versioned`): e.g. compact source files with a
    /// top-level `main` are a preview feature before Java 25. `None` disables every such gate.
    #[serde(skip)]
    pub target_java_version: Option<u32>,
}

impl Config {
    /// The configured severity for `rule`, falling back to `default` when unconfigured.
    pub fn severity(&self, rule: &str, default: Severity) -> Severity {
        self.rules.get(rule).copied().unwrap_or(default)
    }

    /// Load and parse the `jalslint.toml` at `path`, read through `fs`.
    ///
    /// `fs` is any [`FileTree`] — a [`jals_fs::OsFileTree`] on the host, or a
    /// [`jals_fs::InMemoryFileTree`] for wasm / tests; `path` is a `/`-separated virtual path.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the file cannot be read or contains invalid TOML.
    pub fn from_file(fs: &dyn FileTree, path: &str) -> Result<Config, ConfigError> {
        let text = fs.read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_string(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_string(),
            source,
        })
    }

    /// Search upward from `start_dir` (a `/`-separated virtual path) for `jalslint.toml`, read
    /// through `fs`.
    ///
    /// Returns the parsed config if a file is found, otherwise [`Config::default`].
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a discovered file cannot be read or parsed.
    pub fn discover(fs: &dyn FileTree, start_dir: &str) -> Result<Config, ConfigError> {
        let mut dir = Some(start_dir);
        while let Some(d) = dir {
            let candidate = jals_fs::path::join(d, "jalslint.toml");
            if fs.is_file(&candidate) {
                return Config::from_file(fs, &candidate);
            }
            dir = jals_fs::path::parent(d);
        }
        Ok(Config::default())
    }
}

/// The error type returned when loading or parsing a `jalslint.toml` file. `no_std`: it holds a
/// rendered path `String` and wraps [`jals_fs::FsError`] (the read failure) or [`toml::de::Error`]
/// (the parse failure).
mod error {
    use alloc::string::String;
    use core::fmt;

    /// An error loading or parsing a config file.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_empty() {
        let c = Config::default();
        assert!(c.rules.is_empty());
        assert_eq!(
            c.severity("wildcard-import", Severity::Warn),
            Severity::Warn
        );
    }

    #[test]
    fn parses_rule_overrides() {
        let c: Config = toml::from_str(
            r#"
            [rules]
            wildcard-import = "error"
            empty-catch = "allow"
            "#,
        )
        .unwrap();
        assert_eq!(
            c.severity("wildcard-import", Severity::Warn),
            Severity::Error
        );
        assert_eq!(c.severity("empty-catch", Severity::Warn), Severity::Allow);
        assert_eq!(
            c.severity("naming-convention", Severity::Warn),
            Severity::Warn
        );
    }
}
