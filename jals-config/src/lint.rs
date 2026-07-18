//! Linting configuration, deserialized from `jalslint.toml`.
//!
//! Every key is optional; omitted keys fall back to [`Config::default`]. The only section is
//! `[rules]`, a map from rule name (kebab-case, e.g. `wildcard-import`) to a [`Severity`]
//! (`"allow"` / `"warn"` / `"error"`). A rule not listed uses its built-in default severity.

use alloc::collections::BTreeMap;
use alloc::string::String;

use serde::Deserialize;

pub use crate::loader::ConfigError;
use crate::manifest::FeatureSet;

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
    /// The project's resolved language [`FeatureSet`], injected by the host from the manifest's
    /// `[package] features` (see [`Manifest::feature_set`](crate::Manifest::feature_set)) — **not**
    /// a `jalslint.toml` key (hence `serde(skip)`).
    ///
    /// It drives the feature-gated rules: a construct whose [`Feature`](crate::Feature) is *absent*
    /// from this set is flagged (e.g. a top-level `main` when `compact-source-files` is not enabled).
    /// An empty set (the default — no `[package] features` declared) disables every such gate.
    #[serde(skip)]
    pub features: FeatureSet,
}

impl Config {
    /// The configured severity for `rule`, falling back to `default` when unconfigured.
    pub fn severity(&self, rule: &str, default: Severity) -> Severity {
        self.rules.get(rule).copied().unwrap_or(default)
    }
}

impl crate::DiscoverableConfig for Config {
    const FILE_NAME: &'static str = "jalslint.toml";
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
