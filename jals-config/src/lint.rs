//! Linting configuration, deserialized from `jalslint.toml`.
//!
//! Every key is optional; omitted keys fall back to [`Config::default`]. The only section is
//! `[rules]`, a map from rule name (kebab-case, e.g. `wildcard-import`) to a [`Severity`]
//! (`"allow"` / `"warn"` / `"error"`). A rule not listed uses its built-in default severity.

use alloc::collections::BTreeMap;
use alloc::string::String;

use jals_fs::FileTree;
use serde::Deserialize;

pub use crate::loader::ConfigError;

/// How serious a lint finding is. Doubles as the per-rule configuration value: a rule set
/// to [`Allow`](Severity::Allow) is disabled and never runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The rule is disabled; it produces no diagnostics.
    Allow,
    /// The finding is a warning.
    Warn,
    /// The finding is an error.
    Error,
}

impl Severity {
    /// The lowercase name (`"allow"` / `"warn"` / `"error"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl core::fmt::Display for Severity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    /// It drives version-gated rules: e.g. compact source files with a top-level `main` are a
    /// preview feature before Java 25. `None` disables every such gate.
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
    pub fn from_file(fs: &dyn FileTree, path: &str) -> Result<Self, ConfigError> {
        crate::loader::load(fs, path)
    }

    /// Search upward from `start_dir` (a `/`-separated virtual path) for `jalslint.toml`, read
    /// through `fs`.
    ///
    /// Returns the parsed config if a file is found, otherwise [`Config::default`].
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a discovered file cannot be read or parsed.
    pub fn discover(fs: &dyn FileTree, start_dir: &str) -> Result<Self, ConfigError> {
        crate::loader::discover(fs, start_dir, "jalslint.toml")
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
