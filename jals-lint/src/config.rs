//! Linting configuration, deserialized from `jalslint.toml`.
//!
//! Every key is optional; omitted keys fall back to [`Config::default`]. The only section is
//! `[rules]`, a map from rule name (kebab-case, e.g. `wildcard-import`) to a [`Severity`]
//! (`"allow"` / `"warn"` / `"error"`). A rule not listed uses its built-in default severity.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::diagnostic::Severity;

/// Linter configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// Per-rule severity overrides. Keys are rule names (kebab-case); a missing key means the
    /// rule keeps its built-in default severity.
    pub rules: BTreeMap<String, Severity>,
}

impl Config {
    /// The configured severity for `rule`, falling back to `default` when unconfigured.
    pub fn severity(&self, rule: &str, default: Severity) -> Severity {
        self.rules.get(rule).copied().unwrap_or(default)
    }

    /// Load and parse a specific `jalslint.toml` file.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the file cannot be read or contains invalid TOML.
    pub fn from_file(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Search upward from `start_dir` for `jalslint.toml`.
    ///
    /// Returns the parsed config if a file is found, otherwise [`Config::default`].
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a discovered file cannot be read or parsed.
    pub fn discover(start_dir: &Path) -> Result<Config, ConfigError> {
        let mut dir = Some(start_dir);
        while let Some(d) = dir {
            let candidate = d.join("jalslint.toml");
            if candidate.is_file() {
                return Config::from_file(&candidate);
            }
            dir = d.parent();
        }
        Ok(Config::default())
    }
}

/// An error loading or parsing a config file.
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read.
    Io {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The file contained invalid TOML.
    Parse {
        /// The path that failed to parse.
        path: PathBuf,
        /// The underlying parse error.
        source: toml::de::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io { path, source } => {
                write!(f, "failed to read config {}: {source}", path.display())
            }
            ConfigError::Parse { path, source } => {
                write!(f, "failed to parse config {}: {source}", path.display())
            }
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ConfigError::Io { source, .. } => Some(source),
            ConfigError::Parse { source, .. } => Some(source),
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
