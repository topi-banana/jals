//! Formatting configuration, deserialized from `jalsfmt.toml`.
//!
//! # Shape
//!
//! The config is a set of **sections**, each a TOML table and each its own module here. Every
//! key is optional; an omitted key — or an omitted whole section — falls back to
//! [`Config::default`]. Keys use kebab-case.
//!
//! ```toml
//! [layout]
//! indent-width = 2
//! max-width = 100
//!
//! [wrapping]
//! call-arguments = "if-long"
//! ```
//!
//! # What belongs here
//!
//! This is jals's **common style vocabulary**, not the union of what native Java formatters can
//! express. A rule lives here when *two reachable target configurations disagree on that
//! behavior*; a behavior every target agrees on is the formatter's fixed behavior instead, and a
//! knob no target can produce is not modeled at all. Eclipse's 416 settings and IntelliJ's 297
//! stay complete — and typed — in [`jals_fmt::import`]'s native models, which project onto this
//! surface. `jals-fmt/MAPPING.md` is the ledger: it records the vendor inventories, the
//! selection criterion, and the per-rule correspondence, and every section module below points
//! into it.
//!
//! [`jals_fmt::import`]: https://docs.rs/jals-fmt

// Native product / setting names (IntelliJ, EditorConfig, Javadoc, `UPPER_SNAKE` option ids, …)
// recur throughout this module tree's docs as prose, not as Rust items.
#![allow(clippy::doc_markdown)]

use serde::{Deserialize, Serialize};

mod blank_lines;
mod braces;
mod comments;
mod imports;
mod layout;
mod literals;
mod spacing;
mod wrapping;

#[cfg(test)]
mod tests;

pub use crate::loader::ConfigError;

pub use blank_lines::BlankLines;
pub use braces::{BraceStyle, Braces, ForceBraces, KeepOnOneLine};
pub use comments::Comments;
pub use imports::{ImportOrder, Imports};
pub use layout::{IndentStyle, Layout, LineEnding};
pub use literals::{FloatLiteralTrailingZero, HexLiteralCase, LiteralSuffixCase, Literals};
pub use spacing::Spacing;
pub use wrapping::{ParenPositions, WrapPolicy, Wrapping};

/// Formatter style settings.
///
/// Eight sections, each documented in its own module: [`Layout`], [`BlankLines`], [`Braces`],
/// [`Wrapping`], [`Spacing`], [`Comments`], [`Imports`], and [`Literals`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// `[layout]` — indentation, the column limit, and the line-level output shape.
    pub layout: Layout,
    /// `[blank-lines]` — how many empty lines survive, and how many are enforced.
    pub blank_lines: BlankLines,
    /// `[braces]` — brace placement, brace forcing, and one-line collapsing.
    pub braces: Braces,
    /// `[wrapping]` — how a construct breaks across lines when it does not fit.
    pub wrapping: Wrapping,
    /// `[spacing]` — where a single space is emitted between two tokens.
    pub spacing: Spacing,
    /// `[comments]` — comment and Javadoc reflow.
    pub comments: Comments,
    /// `[imports]` — import ordering and modifier ordering.
    pub imports: Imports,
    /// `[literals]` — opt-in numeric-literal rewrites.
    pub literals: Literals,
}

impl crate::DiscoverableConfig for Config {
    const FILE_NAME: &'static str = "jalsfmt.toml";
}
