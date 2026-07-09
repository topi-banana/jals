//! The literal-normalization rules: pure text rewrites of a numeric-literal token, each reading
//! one config option.
//!
//! [`build`] resolves the active chain from `&Config` (skipping `Preserve`); the
//! [`LiteralRegistry`] applies them in turn, exactly as the prior hand-written chain in
//! `token_text` did. The three rules touch disjoint parts of a literal — the fraction, the hex
//! mantissa, and the trailing suffix letter respectively — so they compose without interference;
//! the build order (trailing-zero, then hex case, then suffix case) mirrors the original to keep
//! the output byte-identical.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use jals_syntax::SyntaxKind as S;

use crate::config::Config;
use crate::rules::LiteralRule;

mod float_zero;
mod hex_case;
mod suffix_case;

pub(crate) use float_zero::FloatTrailingZero;
pub(crate) use hex_case::HexCase;
pub(crate) use suffix_case::LiteralSuffix;

/// The active literal-rewrite chain, built once per format from `&Config`.
pub(crate) struct LiteralRegistry {
    rules: Vec<Box<dyn LiteralRule>>,
}

impl LiteralRegistry {
    /// Resolve the active rules from `cfg`. A rule whose option is `Preserve` is omitted, so the
    /// default config yields an empty chain (and literal emission stays verbatim).
    pub(crate) fn from_config(cfg: &Config) -> Self {
        Self { rules: build(cfg) }
    }

    /// Apply every active rewrite in turn — the output of one feeds the next — returning the final
    /// text, or `None` when no rule changed anything (the common case).
    pub(crate) fn apply(&self, text: &str, kind: S) -> Option<String> {
        let mut current: Option<String> = None;
        for rule in &self.rules {
            let input = current.as_deref().unwrap_or(text);
            if let Some(next) = rule.rewrite(input, kind) {
                current = Some(next);
            }
        }
        current
    }
}

/// Build the active literal rules from `cfg`, in the order `token_text` applied them. Each
/// constructor returns `None` for its `Preserve` default, so only opted-in rules are boxed.
fn build(cfg: &Config) -> Vec<Box<dyn LiteralRule>> {
    let mut rules: Vec<Box<dyn LiteralRule>> = Vec::new();
    if let Some(rule) = FloatTrailingZero::new(cfg.float_literal_trailing_zero) {
        rules.push(Box::new(rule));
    }
    if let Some(rule) = HexCase::new(cfg.hex_literal_case) {
        rules.push(Box::new(rule));
    }
    if let Some(rule) = LiteralSuffix::new(cfg.literal_suffix_case) {
        rules.push(Box::new(rule));
    }
    rules
}
