//! palantir-java-format: the near-empty configuration surface and its jals mapping.
//!
//! Palantir inherits GJF's non-configurability verbatim (DESIGN §12.4, §A.7): there is no
//! config file, and the whole surface is `JavaFormatterOptions`' two members — the [`Style`]
//! (`PALANTIR` / `GOOGLE` / `AOSP`) and the `formatJavadoc` boolean (default `false`, reachable
//! only through the API / Gradle plugin / Spotless `formatJavadoc(true)`). [`Config`]'s mapping
//! reuses the GJF family profile ([`super::gjf`]) — Palantir keeps GJF's token-level passes and
//! canonical conventions, differing in its break *engine* (which a config cannot express) and
//! in the style-derived indents / column limit below.
//!
//! [`Style`]: PalantirStyle

use jals_config::fmt::Config;
use serde::Deserialize;

use super::gjf;

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

/// palantir-java-format's whole configuration surface: style plus `formatJavadoc`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PalantirJavaFormatConfig {
    /// The selected style.
    pub style: PalantirStyle,
    /// `formatJavadoc`: reflow Javadoc prose (off by default, unlike GJF).
    pub format_javadoc: bool,
}

impl From<PalantirJavaFormatConfig> for Config {
    fn from(native: PalantirJavaFormatConfig) -> Self {
        let (indent_width, continuation_indent, max_width) = match native.style {
            PalantirStyle::Palantir => (4, 8, 120),
            PalantirStyle::Google => (2, 4, 100),
            PalantirStyle::Aosp => (4, 8, 100),
        };
        gjf::GoogleJavaFormatConfig::family(
            indent_width,
            continuation_indent,
            max_width,
            native.format_javadoc,
        )
    }
}
