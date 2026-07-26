//! The fail-safe: does the output still hold the input's tokens?
//!
//! Formatting must never silently damage a file. After rendering, the output is re-parsed and its
//! significant tokens are compared against the input's. If the comparison fails, the **input is
//! returned unchanged** — a file the formatter cannot handle is better left alone than corrupted.
//!
//! # Why this is not "no tokens were lost"
//!
//! The naive check would defeat half the feature set. `imports.remove-unused` deletes
//! declarations by design; `[literals]` rewrites a token's spelling; `[braces] force-*` inserts
//! braces. Each is a *configured* exception, so the check carries the same exemption list the
//! invariant does (`DESIGN.md` §9, §20):
//!
//! | when | comparison |
//! |---|---|
//! | always | the token **multiset**, not the sequence — so import and modifier reordering pass |
//! | `[literals]` non-`preserve` | by token **kind**, since a literal's text is allowed to change |
//! | `imports.remove-unused` | output ⊆ input instead of equality |
//! | `[braces] force-*` ≠ `never` | extra `{` / `}` in the output are allowed |
//! | `wrapping.reflow-long-strings` | multiset equality still holds; only arrangement changes |
//!
//! The new-syntax-error half of the check is unconditional: no rule may make a file stop parsing.

use alloc::collections::BTreeMap;
use alloc::string::String;

use jals_config::fmt::ForceBraces;
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use crate::passes::literals::LiteralRewrite;
use crate::style::Style;

/// A significant-token multiset, keyed either by `(kind, text)` or by kind alone.
type Budget = BTreeMap<(SyntaxKind, String), usize>;

/// Verifies that a formatted output still accounts for its input.
pub(crate) struct TokenBudget;

impl TokenBudget {
    /// Whether `formatted` is an acceptable rendering of `src` under `style`.
    pub(crate) async fn accepts(
        src: &str,
        src_tree: &SyntaxNode,
        src_errors: usize,
        formatted: &str,
        style: &Style,
    ) -> bool {
        // An output that no longer parses is never acceptable, whatever the input's own state.
        let reparsed = jals_syntax::Parse::parse(formatted).await;
        if reparsed.errors().len() > src_errors {
            return false;
        }
        // A formatter that produced nothing from a non-empty file has lost everything.
        if formatted.trim().is_empty() && !src.trim().is_empty() {
            return false;
        }

        let by_kind = LiteralRewrite::is_active(&style.cfg.literals);
        let before = Self::collect(src_tree, by_kind);
        let after = Self::collect(&reparsed.syntax(), by_kind);

        let allow_missing = style.cfg.imports.remove_unused;
        let allow_extra_braces = Self::forces_braces(style);
        Self::compare(&before, &after, allow_missing, allow_extra_braces)
    }

    /// Whether any `[braces] force-*` rule can insert a brace.
    fn forces_braces(style: &Style) -> bool {
        let braces = &style.cfg.braces;
        [
            braces.force_if,
            braces.force_for,
            braces.force_while,
            braces.force_do_while,
        ]
        .iter()
        .any(|force| *force != ForceBraces::Never)
    }

    /// The significant-token multiset of a tree.
    ///
    /// `by_kind` drops the text, which is what makes the check survive `[literals]` rewriting a
    /// literal's spelling while still catching a literal that turned into something else.
    fn collect(root: &SyntaxNode, by_kind: bool) -> Budget {
        let mut budget = Budget::new();
        for tok in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| !tok.kind().is_trivia())
        {
            let text = if by_kind {
                String::new()
            } else {
                tok.text().into()
            };
            *budget.entry((tok.kind(), text)).or_insert(0) += 1;
        }
        budget
    }

    /// Compare two multisets under the configured allowances.
    fn compare(
        before: &Budget,
        after: &Budget,
        allow_missing: bool,
        allow_extra_braces: bool,
    ) -> bool {
        for (key, &count) in after {
            let expected = before.get(key).copied().unwrap_or(0);
            if count > expected
                && !(allow_extra_braces && matches!(key.0, SyntaxKind::LBRACE | SyntaxKind::RBRACE))
            {
                return false;
            }
        }
        if allow_missing {
            return true;
        }
        for (key, &count) in before {
            if after.get(key).copied().unwrap_or(0) < count {
                return false;
            }
        }
        true
    }
}
