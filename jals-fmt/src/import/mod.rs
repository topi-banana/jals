//! **Config importers** — lower a native Java-formatter configuration into a jals [`Config`].
//!
//! Each importer is two pieces: a `Deserialize` **model** of the native config, and an
//! `impl From<Model> for Config` that **projects** it onto jals's option surface. The two have
//! deliberately different completeness criteria (`jals-fmt/MAPPING.md` §3):
//!
//! - the **model is total** — every option the vendor has, with none missing: 416 for Eclipse,
//!   297 for IntelliJ, 6 for google-java-format, 2 for palantir-java-format, and Spotless's
//!   whole step list. Each vendor module ships an `inventory.tsv` machine-extracted from the
//!   product's own sources, and a coverage test that fails when a listed option is not modeled;
//! - the **projection is partial**, because `jals_config::fmt::Config` is a curated common
//!   vocabulary rather than the union of four surfaces. A native option with no jals equivalent
//!   is still modeled, named, and typed — it just is not carried across. `MAPPING.md` §7 lists
//!   what is deliberately not projected, and why.
//!
//! Native values stay **typed** rather than stringly-typed — an enum is a Rust enum, Eclipse's
//! `alignment_for_*` is a bitmask newtype, an import layout is an ordered list — so two distinct
//! native values can never collapse before the projection decides.
//!
//! # Layout
//! - [`gjf`] / [`palantir`] — near-empty models; these tools are non-configurable by design and
//!   have no file, so the surface is a style flag plus a handful of pass toggles.
//! - [`eclipse`] / [`intellij`] — real files, each modeled as a set of family structs. The
//!   portable readers (`.prefs` properties, `.editorconfig`) live in the private `text` module;
//!   the XML readers (private `xml` module) need a real XML parser and are gated behind the
//!   `std` feature.
//! - [`spotless`] — an orchestrator: it owns no engine, it selects a delegate (GJF / Palantir /
//!   Eclipse) and wraps it in generic steps.
//! - The private `serde_kv` module is the `key → value` map → typed model bridge shared by the
//!   file-backed importers.

// The importer docs name many native products / files (IntelliJ, EditorConfig, …) in prose.
#![allow(clippy::doc_markdown)]

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use jals_config::fmt::Config;

pub mod eclipse;
pub mod gjf;
pub mod intellij;
pub mod palantir;
mod serde_kv;
pub mod spotless;
mod text;
#[cfg(feature = "std")]
mod xml;

#[cfg(test)]
mod tests;

pub use eclipse::{EclipseConfig, EclipsePrefs};
pub use gjf::{GjfStyle, GoogleJavaFormatConfig};
pub use intellij::{IntellijConfig, IntellijEditorConfig};
pub use palantir::{PalantirJavaFormatConfig, PalantirStyle};
pub use spotless::{LeadingWhitespace, SpotlessConfig, SpotlessDelegate};

#[cfg(feature = "std")]
pub use eclipse::EclipseXmlProfile;
#[cfg(feature = "std")]
pub use intellij::IntellijXmlScheme;

/// A failure while importing a native formatter config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The parsed key/value settings did not fit the importer model (a malformed value serde
    /// could not coerce). Carries a human-readable detail.
    Deserialize(String),
    /// The XML document was malformed. Only produced by the `std`-gated XML importers.
    Xml(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deserialize(detail) => write!(f, "config deserialization failed: {detail}"),
            Self::Xml(detail) => write!(f, "config XML is malformed: {detail}"),
        }
    }
}

impl core::error::Error for ImportError {}

/// Pre-rustfmt Java-idiomatic floor for the keys [`Config::default`] now takes from rustfmt.
///
/// Vendor `From` impls apply this immediately after `Config::default()`, then overlay native
/// values, so an omitted native option cannot leak rustfmt into a Java profile.
pub(crate) const fn pin_java_baseline(config: &mut Config) {
    use jals_config::fmt::{ForceBraces, ImportOrder, LineEnding, WrapPolicy};
    config.layout.line_ending = LineEnding::Lf;
    config.imports.order = ImportOrder::Preserve;
    config.wrapping.method_parameters = WrapPolicy::IfLong;
    config.wrapping.fill_item_width = 0;
    config.wrapping.import_group = WrapPolicy::Never;
    config.wrapping.remove_nested_parens = false;
    config.comments.code_block_width = 0;
    config.braces.force_switch_arm = ForceBraces::Never;
}

/// A native formatter config that can be parsed from its file text and lowered to a jals
/// [`Config`].
///
/// Implemented by the file-backed importers (Eclipse, IntelliJ). google-/palantir-java-format
/// have no file to parse — they are constructed from their style flag directly and expose only
/// the model + [`From`] impl. [`spotless`] resolves to a delegate rather than parsing layout, so
/// it too skips this trait.
pub trait ConfigImporter {
    /// The typed, `Deserialize`-derived model of the native config.
    type Native: Into<Config>;

    /// Parse the native config's file text into its model.
    fn parse(src: &str) -> Result<Self::Native, ImportError>;

    /// Parse and lower to a jals [`Config`] in one step.
    fn import(src: &str) -> Result<Config, ImportError> {
        Ok(Self::parse(src)?.into())
    }
}

/// The shared normalization of a native import-group entry into a jals `import_groups` prefix.
///
/// Every native formatter spells its groups differently (IntelliJ `java.**`, Spotless `java`), but
/// they all mean the same thing: *the package `java` and everything under it*. jals matches
/// `import_groups` by raw string prefix, so the representation has to carry a trailing `.` — it is
/// the dot that stops `java` from also capturing `javax.*`. Both importers go through here so they
/// cannot drift apart on the encoding.
struct ImportGroups;

impl ImportGroups {
    /// The catch-all group (every import not claimed by a named prefix).
    const CATCH_ALL: &'static str = "*";
    /// The single group jals uses for static imports; every native static group collapses into it.
    const STATIC: &'static str = "static";

    /// Normalize one package prefix, already stripped of its native wildcard / marker syntax.
    /// An empty prefix is the catch-all; anything else is returned dotted.
    fn prefix(package: &str) -> String {
        if package.is_empty() {
            Self::CATCH_ALL.to_owned()
        } else if package.ends_with('.') {
            package.to_owned()
        } else {
            format!("{package}.")
        }
    }

    /// Append the `"static"` group unless it is already present — native configs may declare
    /// several static groups (`$*`, `\#com.acme`), and jals models only one.
    fn push_static(groups: &mut Vec<String>) {
        if !groups.iter().any(|group| group == Self::STATIC) {
            groups.push(Self::STATIC.to_owned());
        }
    }
}
