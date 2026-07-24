//! **Config importers** — lower a native Java-formatter configuration into a jals [`Config`].
//!
//! Each importer is two pieces (per the task's shape): a `#[derive(Deserialize)]` *model* of the
//! native config, and an `impl From<Model> for Config` that projects it onto jals's option
//! surface. The projection is deliberately **lossy** — native surfaces range from *nothing*
//! (google-/palantir-java-format are non-configurable) to ~400 (Eclipse) / ~270 (IntelliJ)
//! options, while jals exposes one common-rule set. We model only the subset that has a jals
//! equivalent; a native option with no counterpart is simply not carried (a full bijection is
//! impossible — DESIGN §11 / §15).
//!
//! The requirement that the native → jals map be *injective on the modeled subset* is met by
//! keeping the native models **typed** rather than stringly-typed: a native setting that can take
//! an enum / a bitmask / an ordered list is modeled as a Rust enum / newtype / `Vec`, so two
//! distinct native values never collapse to one Rust value before the mapping decides.
//!
//! # Layout
//! - [`gjf`] / [`palantir`] — minimal (near-empty) models; no file, selected by a style flag.
//! - [`eclipse`] / [`intellij`] — real files. The portable readers (`.prefs` properties,
//!   `.editorconfig`) live in the private `text` module; the XML readers (private `xml` module)
//!   need a real XML parser and are gated behind the `std` feature.
//! - [`spotless`] — an orchestrator: it owns no engine, it selects a delegate (GJF / Palantir /
//!   Eclipse) plus a few generic steps.
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

pub use eclipse::EclipseConfig;
pub use gjf::{GjfStyle, GoogleJavaFormatConfig};
pub use intellij::IntellijConfig;
pub use palantir::{PalantirJavaFormatConfig, PalantirStyle};
pub use spotless::{SpotlessConfig, SpotlessDelegate};

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
