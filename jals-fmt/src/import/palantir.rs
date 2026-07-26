//! palantir-java-format — the whole (near-empty) configuration surface.
//!
//! # Coverage
//!
//! Palantir inherits google-java-format's non-configurability verbatim, down to the javadoc
//! wording. `JavaFormatterOptions` has exactly two members — the [`Style`](PalantirStyle) and
//! `formatJavadoc` (default `false`, unlike GJF, and reachable only through the API, the Gradle
//! plugin, or Spotless `.formatJavadoc(true)`) — and [`PalantirJavaFormatConfig`] models both.
//! There is no config file to detect or parse.
//!
//! The projection reuses the GJF family profile: Palantir keeps GJF's token-level passes and
//! canonical conventions, and differs in its break *engine* (`BreakBehaviour` /
//! `PartialInlineability` / `Obs` backtracking), which a config cannot express at all — see
//! `DESIGN.md` §12.4.

use jals_config::fmt::Config;
use serde::Deserialize;

use super::gjf::GoogleJavaFormatConfig;

/// palantir-java-format's `Style`: the only layout degree of freedom.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PalantirStyle {
    /// The Palantir style: doubled indents (block 4, continuation 8) and a 120-column limit.
    #[default]
    Palantir,
    /// GJF's Google style: block 2, continuation 4, 100 columns.
    Google,
    /// GJF's AOSP variant: block 4, continuation 8, 100 columns.
    Aosp,
}

impl PalantirStyle {
    /// `(block indent, continuation indent, column limit)`.
    const fn metrics(self) -> (usize, usize, usize) {
        match self {
            Self::Palantir => (4, 8, 120),
            Self::Google => (2, 4, 100),
            Self::Aosp => (4, 8, 100),
        }
    }
}

/// palantir-java-format's whole configuration surface: style plus `formatJavadoc`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PalantirJavaFormatConfig {
    /// `JavaFormatterOptions.style`.
    pub style: PalantirStyle,
    /// `JavaFormatterOptions.formatJavadoc` — off by default, unlike GJF.
    pub format_javadoc: bool,
}

impl From<PalantirJavaFormatConfig> for Config {
    fn from(native: PalantirJavaFormatConfig) -> Self {
        let (indent_width, continuation_indent, max_width) = native.style.metrics();
        GoogleJavaFormatConfig::family(
            indent_width,
            continuation_indent,
            max_width,
            GoogleJavaFormatConfig {
                format_javadoc: native.format_javadoc,
                ..GoogleJavaFormatConfig::default()
            },
        )
    }
}
