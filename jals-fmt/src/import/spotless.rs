//! Spotless importer — a thin **orchestrator**, not an engine.
//!
//! Spotless owns no layout algorithm: for Java it delegates to google-/palantir-java-format or
//! Eclipse JDT and wraps them in an ordered list of generic string→string steps (DESIGN §12.1).
//! Its configuration is a Gradle / Maven build DSL — *code*, not data — so extracting values from
//! `build.gradle` / `pom.xml` is explicitly out of scope for this importer (it was the un-chosen
//! "all five" option). What is modeled is the **resolved** shape: which delegate engine was
//! selected, plus the handful of generic steps that map onto jals options.
//!
//! Accordingly there is no `ConfigImporter` text parser here; the surface is the
//! [`SpotlessConfig`] model (constructible via serde from a resolved `[compat.spotless]` table)
//! and its `From` impl, which starts from the delegate's [`Config`] and layers the generic steps
//! on top.

use alloc::string::String;
use alloc::vec::Vec;

use jals_config::fmt::Config;
use serde::Deserialize;

use super::ImportGroups;
use super::eclipse::EclipseConfig;
use super::gjf::GoogleJavaFormatConfig;
use super::palantir::PalantirJavaFormatConfig;

/// The engine a Spotless `java {}` block delegates to. The layout comes entirely from the
/// delegate; Spotless only orders steps around it.
///
/// Note the [`Eclipse`](Self::Eclipse) variant embeds an [`EclipseConfig`], whose numeric / enum
/// fields deserialize through the string-coercing `Kv` helpers — i.e. it expects the *stringified*
/// setting representation the [`ConfigImporter`](super::ConfigImporter)s produce, where every value
/// is a string, not a typed TOML table with integer / bool literals.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "engine", rename_all = "kebab-case")]
pub enum SpotlessDelegate {
    /// `googleJavaFormat(...)`.
    GoogleJavaFormat(GoogleJavaFormatConfig),
    /// `palantirJavaFormat(...)`.
    PalantirJavaFormat(PalantirJavaFormatConfig),
    /// `eclipse(...).configFile(...)`.
    Eclipse(EclipseConfig),
}

impl Default for SpotlessDelegate {
    fn default() -> Self {
        Self::GoogleJavaFormat(GoogleJavaFormatConfig::default())
    }
}

impl From<SpotlessDelegate> for Config {
    fn from(delegate: SpotlessDelegate) -> Self {
        match delegate {
            SpotlessDelegate::GoogleJavaFormat(gjf) => gjf.into(),
            SpotlessDelegate::PalantirJavaFormat(palantir) => palantir.into(),
            SpotlessDelegate::Eclipse(eclipse) => eclipse.into(),
        }
    }
}

/// A resolved Spotless `java {}` pipeline: a delegate engine plus the generic steps that map onto
/// jals options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct SpotlessConfig {
    /// The delegate engine (defaults to google-java-format, Spotless's most common Java step).
    pub delegate: SpotlessDelegate,
    /// `endWithNewline()` / its absence — `None` leaves the delegate's final-newline behavior.
    pub end_with_newline: Option<bool>,
    /// `importOrder(...)` groups: a spotless prefix list where `""` is the catch-all and `"\#"`
    /// marks the static-import group (DESIGN §A.5 / §A.6). Empty ⇒ the step was not configured.
    pub import_order: Vec<String>,
}

impl From<SpotlessConfig> for Config {
    fn from(native: SpotlessConfig) -> Self {
        let mut config: Self = native.delegate.into();

        if let Some(end_with_newline) = native.end_with_newline {
            config.insert_final_newline = end_with_newline;
        }
        let groups = SpotlessConfig::map_import_order(&native.import_order);
        if !groups.is_empty() {
            config.group_imports = true;
            config.import_groups = groups;
        }

        config
    }
}

impl SpotlessConfig {
    /// Map a Spotless `importOrder` prefix list to jals import-group prefixes.
    ///
    /// A `\#` prefix marks a *static* group — bare (`\#`, every static import) or scoped to a
    /// package (`\#com.acme`); jals models one `"static"` group, so all of them collapse into it.
    /// Everything else is a package prefix (`""` being the catch-all) and is normalized by
    /// [`ImportGroups::prefix`], which is what keeps this importer's encoding identical to the
    /// IntelliJ one.
    fn map_import_order(order: &[String]) -> Vec<String> {
        let mut groups = Vec::with_capacity(order.len());
        for entry in order {
            let entry = entry.as_str();
            if entry.starts_with("\\#") || entry.starts_with('#') {
                ImportGroups::push_static(&mut groups);
                continue;
            }
            groups.push(ImportGroups::prefix(entry));
        }
        groups
    }
}
