//! The pipeline driver: L0 plans, L2 lowering, the engine, then L4.
//!
//! [`api::run`] is where the whole crate's pipeline is wired, and it is the only place that
//! knows the order. It ends with [`TokenBudget`](super::TokenBudget), the fail-safe that returns
//! the input untouched rather than hand back an output it cannot vouch for.
//!
//! # Why it lives here and not in `visit`
//!
//! `DESIGN.md` §8.1 names seam **S4** — which L0/L3/L4 passes run — as a *pipeline stage*, and
//! [`style`](crate::style)'s seam table says `passes` reads it. Driving the sequence is not
//! lowering a node, so it does not belong in [`visit`](crate::visit): that module answers "what
//! document does this construct emit", and this one answers "what runs, in what order, and is the
//! result trustworthy".

use crate::passes::finalize;
use crate::passes::off_on;
use crate::passes::string_wrapper;
use crate::passes::token_budget;
use crate::passes::unused_imports;
use alloc::borrow::ToOwned;
use alloc::string::String;

use jals_syntax::SyntaxNode;

use crate::engine::Engine;
use crate::style::Style;
use crate::visit::Ctx;

/// The outcome of one format run, and whether the formatter could vouch for it.
///
/// The fail-safe's rejection is otherwise **silent**: the input comes back byte-identical, so
/// `--check` reports no diff and a test that only asserts a preservation property sees nothing
/// wrong — `output == input` is exactly what preservation permits. Naming the two outcomes makes
/// "the whole file went unformatted" an assertable fact rather than an invisible one.
///
/// It is deliberately *not* a [`Warning`](crate::Warning): `tests/coverage.rs` counts a range-less
/// warning as "the formatter noticed this rule", so a fallback warning would let a genuinely inert
/// rule pass `every_rule_reaches_the_formatter` by tripping the fail-safe instead of changing a
/// layout.
///
/// That rules out the *warning* route, not every route, and the distinction does reach callers: it
/// becomes [`FormatOutput::fell_back`](crate::FormatOutput::fell_back), which the coverage test does
/// not read. `jals fmt --check` fails on a fallback instead of calling the file clean, and the LSP
/// says so rather than looking like it found nothing to do.
#[derive(Debug)]
pub(crate) enum Formatted {
    /// The output holds everything the input did, under the license the config granted.
    Vouched(String),
    /// The output could not be vouched for, so this is the input, unchanged.
    FellBack(String),
}

impl Formatted {
    /// The text to hand the caller, whichever way it was reached.
    pub(crate) fn text(self) -> String {
        match self {
            Self::Vouched(text) | Self::FellBack(text) => text,
        }
    }

    /// Whether the formatter could vouch for the text.
    pub(crate) const fn vouched(&self) -> bool {
        matches!(self, Self::Vouched(_))
    }
}

pub(crate) use api::run;

/// Drives the whole formatting pipeline.
pub(crate) mod api {
    use super::{
        Ctx, Engine, Formatted, String, Style, SyntaxNode, ToOwned, finalize, off_on,
        string_wrapper, token_budget, unused_imports,
    };

    /// Format a parsed tree, falling back to `src` if the result cannot be vouched for.
    pub(crate) async fn run(
        root: &SyntaxNode,
        src: &str,
        src_errors: usize,
        style: &Style,
    ) -> Formatted {
        let laid_out = format_tree(root, src, style).await;

        // L4: re-wrap long string concatenations, but only when re-formatting the candidate
        // reproduces it exactly (`DESIGN.md` §R4.1) *and* the result still holds the input's
        // tokens. Checking the budget here rather than only at the end is what keeps a rewrap the
        // formatter cannot vouch for from costing the whole file: it costs the rewrap.
        let (text, vouched) = match string_wrapper::candidate(&laid_out, style).await {
            Some(candidate) => {
                // The candidate is a re-split concatenation on one logical line; the engine
                // places the breaks. Adopt its formatting only if formatting *that* is a fixed
                // point, which is the guarantee `DESIGN.md` §R4.1 asks for.
                let wrapped = format_source_text(&candidate, style).await;
                if format_source_text(&wrapped, style).await == wrapped
                    && token_budget::budget::accepts(src, root, src_errors, &wrapped, style.license)
                        .await
                {
                    // Already vouched for, against the same five arguments the final check would
                    // pass. `accepts` is pure, so asking twice can only get the same answer.
                    (wrapped, true)
                } else {
                    (laid_out, false)
                }
            }
            None => (laid_out, false),
        };

        if vouched
            || token_budget::budget::accepts(src, root, src_errors, &text, style.license).await
        {
            Formatted::Vouched(text)
        } else {
            Formatted::FellBack(src.to_owned())
        }
    }

    /// Parse and format a string, without the string-wrapping pass — the verification path.
    pub(crate) async fn format_source_text(src: &str, style: &Style) -> String {
        let parse = jals_syntax::Parse::parse(src).await;
        format_tree(&parse.syntax(), src, style).await
    }

    /// L0 → L2 → L1 → finalize, with no fail-safe and no string wrapping.
    pub(crate) async fn format_tree(root: &SyntaxNode, src: &str, style: &Style) -> String {
        let disabled = off_on::scan(root, style);
        let used = if style.cfg.imports.remove_unused {
            Some(unused_imports::used_names(root).await)
        } else {
            None
        };

        let mut ctx = Ctx::new(root, src, style, used, disabled).await;
        ctx.visit(root).await;
        let (mut doc, tags) = ctx.finish();

        let rendered = Engine::new(style, tags).render(&mut doc).await;
        finalize::apply(&rendered, src, style)
    }
}
