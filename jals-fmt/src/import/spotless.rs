//! Spotless importer — a thin **orchestrator**, not an engine.
//!
//! # Coverage
//!
//! Spotless owns no layout algorithm: for Java it delegates to google-/palantir-java-format or
//! Eclipse JDT and wraps the delegate in an ordered list of generic `String → String` steps.
//! [`SpotlessConfig`] models the delegate plus **every** Java-applicable step the Gradle and
//! Maven plugins document — the ones that shape output (`importOrder`, `endWithNewline`,
//! `trimTrailingWhitespace`, the leading-whitespace converters, `toggleOffOn`), the semantic
//! ones (`removeUnusedImports`, `formatAnnotations`), and the opaque ones (`licenseHeader`,
//! `replace` / `replaceRegex` / `custom`), which are carried as counted/typed presences because
//! their effect is not expressible as a style rule.
//!
//! # Not a text parser
//!
//! Spotless's configuration is a Gradle / Maven build DSL — *code*, not data — so extracting
//! values from `build.gradle` / `pom.xml` is out of scope (`DESIGN.md` P-gen-4). What is modeled
//! is the **resolved** pipeline, constructible through serde from a `[compat.spotless]` table.
//! That is also why there is no [`ConfigImporter`](super::ConfigImporter) impl here.

use crate::import;
use alloc::string::String;
use alloc::vec::Vec;

use jals_config::fmt::{Config, ImportOrder, IndentStyle};
use serde::Deserialize;

use super::eclipse::EclipseConfig;
use super::gjf::GoogleJavaFormatConfig;
use super::palantir::PalantirJavaFormatConfig;

/// The engine a Spotless `java {}` block delegates to.
///
/// The layout comes entirely from the delegate; Spotless only orders steps around it. Note that
/// the [`Eclipse`](Self::Eclipse) variant deserializes from the *stringified* setting map an
/// exported profile lowers to (`"org.eclipse.jdt.core.formatter.lineSplit" = "120"`), matching
/// `eclipse().configFile(...)`, not a typed TOML table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "engine", rename_all = "kebab-case")]
// The Eclipse variant is inherently the large one — it carries a 416-setting profile — and a
// pipeline holds exactly one delegate, so boxing would only add an indirection.
#[allow(clippy::large_enum_variant)]
pub enum SpotlessDelegate {
    /// `googleJavaFormat(version)`, with `.aosp()` / `.skipJavadocFormatting()` /
    /// `.reflowLongStrings()` / `.reorderImports()`.
    GoogleJavaFormat(GoogleJavaFormatConfig),
    /// `palantirJavaFormat(version)`, with `.style(...)` / `.formatJavadoc(true)`.
    PalantirJavaFormat(PalantirJavaFormatConfig),
    /// `eclipse(version).configFile(...)`.
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

/// How a `leadingTabsToSpaces` / `leadingSpacesToTabs` step converts leading whitespace.
///
/// Spelled `indentWithSpaces` / `indentWithTabs` before Spotless 6.x.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeadingWhitespace {
    /// `leadingTabsToSpaces(n)`.
    Spaces,
    /// `leadingSpacesToTabs(n)`.
    Tabs,
}

/// A resolved Spotless `java {}` pipeline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct SpotlessConfig {
    /// The delegate engine (defaults to google-java-format, Spotless's most common Java step).
    pub delegate: SpotlessDelegate,
    /// `endWithNewline()`. `None` leaves the delegate's final-newline behavior.
    pub end_with_newline: Option<bool>,
    /// `trimTrailingWhitespace()`. `None` leaves the delegate's behavior.
    pub trim_trailing_whitespace: Option<bool>,
    /// `leadingTabsToSpaces(n)` / `leadingSpacesToTabs(n)` — which unit leading whitespace is
    /// converted to.
    pub leading_whitespace: Option<LeadingWhitespace>,
    /// The `n` of the step above, when it was given one: the tab stop the conversion assumes,
    /// which projects onto both `layout.indent-width` and `layout.tab-width`.
    pub leading_whitespace_size: Option<usize>,
    /// `importOrder(...)` / `importOrderFile(...)` groups: a prefix list where `""` is the
    /// catch-all and `\#` marks a static-import group. Empty ⇒ the step was not configured.
    pub import_order: Vec<String>,
    /// `removeUnusedImports()`. Semantic (it deletes declarations), so it is recorded rather
    /// than projected — jals has no equivalent rule.
    pub remove_unused_imports: bool,
    /// `formatAnnotations()` — moves type-use annotations back onto the type's line using a
    /// hardcoded name table. A heuristic post-pass with no jals equivalent.
    pub format_annotations: bool,
    /// `toggleOffOn()` — honor `spotless:off` / `spotless:on` regions.
    pub toggle_off_on: bool,
    /// The off marker `toggleOffOn(off, on)` was given, when it was not the default.
    pub toggle_off_tag: Option<String>,
    /// The on marker, likewise.
    pub toggle_on_tag: Option<String>,
    /// `licenseHeader(...)` — the header text, if the step is configured. Opaque to layout.
    pub license_header: Option<String>,
    /// How many `replace` / `replaceRegex` / `custom` steps the pipeline declares. Their effect
    /// is arbitrary text substitution, so only their presence is modeled.
    pub custom_steps: usize,
}

impl From<SpotlessConfig> for Config {
    fn from(native: SpotlessConfig) -> Self {
        let mut config: Self = native.delegate.into();

        if let Some(end_with_newline) = native.end_with_newline {
            config.layout.insert_final_newline = end_with_newline;
        }
        if let Some(trim) = native.trim_trailing_whitespace {
            config.layout.trim_trailing_whitespace = trim;
        }
        if let Some(leading) = native.leading_whitespace {
            config.layout.indent_style = match leading {
                LeadingWhitespace::Spaces => IndentStyle::Space,
                LeadingWhitespace::Tabs => IndentStyle::Tab,
            };
            if let Some(size) = native.leading_whitespace_size {
                // The step's `n` is the tab stop on *both* sides of the conversion — the spaces
                // one tab becomes, and the spaces that become one tab — so it fixes the literal
                // tab's display width as much as the indentation level's.
                config.layout.indent_width = size;
                config.layout.tab_width = size;
            }
        }
        if native.toggle_off_on {
            config.layout.formatter_tags = true;
            if let Some(off) = native.toggle_off_tag {
                config.layout.formatter_off_tag = off;
            }
            if let Some(on) = native.toggle_on_tag {
                config.layout.formatter_on_tag = on;
            }
        }
        let groups = SpotlessConfig::map_import_order(&native.import_order);
        if !groups.is_empty() {
            config.imports.order = ImportOrder::Group;
            config.imports.groups = groups;
        }

        config
    }
}

impl SpotlessConfig {
    /// Map a Spotless `importOrder` prefix list to jals import-group prefixes.
    ///
    /// A `\#` prefix marks a *static* group — bare (`\#`, every static import) or scoped to a
    /// package (`\#com.acme`); jals models one `"static"` group, so all of them collapse into
    /// it. Everything else is a package prefix (`""` being the catch-all) and is normalized by
    /// [`import::prefix`], which keeps this importer's encoding identical to the IntelliJ
    /// one. The number of backslashes is host-dependent (`.importorder` `\#`, Groovy `'\\#'`),
    /// so both the escaped and bare spellings are accepted.
    fn map_import_order(order: &[String]) -> Vec<String> {
        let mut groups = Vec::with_capacity(order.len());
        for entry in order {
            let entry = entry.as_str();
            if entry.starts_with("\\#") || entry.starts_with('#') {
                import::push_static(&mut groups);
                continue;
            }
            groups.push(import::prefix(entry));
        }
        groups
    }
}
