#![cfg_attr(not(test), no_std)]
//! A formatter for JALS/Java source, driven by the `jals-syntax` CST.
//!
//! # One engine
//!
//! `jals-fmt` has exactly **one layout engine**: a port of google-java-format's greedy,
//! single-pass `computeBreaks` over a GJF-shaped [`Doc`](ir::Doc) IR. Every style target —
//! `google-java-format`, Eclipse JDT, IntelliJ IDEA, and `palantir-java-format` — is reached by
//! tuning [`Config`] on top of that engine, never by swapping engines. The four products really do have
//! four mutually incompatible resolution algorithms; porting all of them was considered and
//! rejected, and `DESIGN.md` §11 and §18 record both the decision and the differences it makes
//! permanent. **Do not add an engine trait, a second renderer, or a Wadler/prettier `fits`.**
//!
//! # The pipeline
//!
//! ```text
//!   L0  passes    import ordering, unused-import removal, modifier ordering  (token order)
//!   L2  visit     CST → Ops → Doc                                            (emission)
//!   L1  engine    compute_breaks → write                                     (resolution)
//!   L4  passes    long-string rewrapping, finalize                           (text)
//! ```
//!
//! Emission is declarative and per-node; resolution is a single left-to-right fold. That split is
//! why the ~50 syntax rules compose but the break algorithm cannot be decomposed.
//!
//! # Invariants
//!
//! - **Never panics, never loses input.** A node with no bespoke rule falls through to a generic
//!   path that still emits all of its tokens; an `ERROR` node is emitted verbatim. If the output
//!   fails [`TokenBudget`](passes::TokenBudget)'s check, the input is returned unchanged and
//!   [`FormatOutput::fell_back`] says so — the one way to tell that outcome from "already
//!   formatted", which it is otherwise byte-identical to.
//! - **Idempotent.** `format(format(x)) == format(x)`.
//! - **Significant tokens are preserved as a multiset**, except where an operation declared in
//!   [`OPERATIONS`](passes::token_license::OPERATIONS) applies. [`Config::default`] licenses
//!   exactly the unconditional dialect grouped-import trailing-comma drop, `[wrapping]
//!   remove-nested-parens`, and `[braces] force-*` (because `force-switch-arm = always`).
//!   `[imports] order = sort` is also on by default but is `Reorders` — sequence, not multiset.
//!   Every other configured row stays off/`preserve`. The table rather than this sentence is what
//!   [`TokenBudget`](passes::TokenBudget) reads.
//! - **Comments are never dropped.** Each is anchored to exactly one token and emitted with it.
//! - **Layout never reads input whitespace**, with one exception the engine shares with
//!   google-java-format: whether two significant tokens had a blank line between them. Rules that
//!   would read more are rounded to a canonical value and the rounding is reported as a
//!   [`Warning`] (`DESIGN.md` §17).

// Native product names (google-java-format, Eclipse JDT, IntelliJ IDEA, Spotless, Palantir) and
// their setting ids run through this crate's docs as prose, exactly as they do in
// `jals_config::fmt`. They are not Rust items and backticking them would read worse.
#![allow(clippy::doc_markdown)]

extern crate alloc;

mod comments;
mod engine;
pub mod generate;
pub mod import;
#[cfg(test)]
mod invariants;
mod ir;
mod javadoc;
mod ops;
mod output;
mod passes;
mod style;
mod visit;

use jals_config::FeatureSet;
use jals_config::fmt::Config;

pub use output::{FormatOutput, Warning};

impl FormatOutput {
    /// Format `src` according to `config`, for a project enabling `features`.
    ///
    /// Parsing is lossless and error-resilient, so a source with syntax errors is still formatted
    /// best-effort and the errors come back as [`Warning`]s. Configuration diagnostics — a rule
    /// rounded because it would have read input whitespace, or because it would have *written*
    /// syntax the project's dialect does not enable — arrive the same way, with no range.
    ///
    /// `features` is the project's `[package] features`, or [`FeatureSet::default`] for a source
    /// formatted outside any project (a bare file, stdin, a corpus fixture). Only a rule that
    /// emits dialect syntax reads it, and there is exactly one: `[imports] granularity =
    /// "package"` writes grouped imports, which do not compile unless `grouped-imports` is on.
    /// The formatter cannot discover that itself — a lossless parser accepts a grouped import in
    /// any project, and so does the fail-safe — so the answer is a parameter rather than a guess.
    pub async fn format_source(src: &str, config: &Config, features: FeatureSet) -> Self {
        let (style, mut warnings) = style::Style::reify(config, src, features);

        let parse = jals_syntax::Parse::parse(src).await;
        let errors = parse.errors().len();
        warnings.extend(parse.errors().iter().map(Warning::from_syntax_error));

        let outcome = passes::Formatter::run(&parse.syntax(), src, errors, &style).await;
        Self {
            vouched: outcome.vouched(),
            formatted: outcome.text(),
            warnings,
        }
    }
}
