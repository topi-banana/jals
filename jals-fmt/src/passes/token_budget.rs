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
//! | `wrapping.reflow-long-strings` | the literals and the `+` between them leave the multiset; what the concatenation *spells* is compared instead. A text block leaves it too: `indentTextBlocks` rewrites its incidental whitespace, which is layout rather than content |
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

/// Which part of a compilation unit a [`Budget`] covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Every significant token.
    Everything,
    /// Only tokens inside an `import` declaration.
    ImportsOnly,
    /// Everything except those.
    OutsideImports,
}

impl Scope {
    /// Whether this scope counts `tok`.
    fn admits(self, tok: &jals_syntax::SyntaxToken) -> bool {
        match self {
            Self::Everything => true,
            Self::ImportsOnly => Self::in_import(tok),
            Self::OutsideImports => !Self::in_import(tok),
        }
    }

    /// Whether a token sits inside an `import` declaration.
    fn in_import(tok: &jals_syntax::SyntaxToken) -> bool {
        tok.parent_ancestors()
            .any(|node| node.kind() == SyntaxKind::IMPORT_DECL)
    }
}

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

        let by_kind = LiteralRewrite::is_active(style.cfg.literals);
        let allow_extra_braces = Self::forces_braces(style);

        // A reflowed concatenation is re-split at different boundaries, so neither the literals'
        // texts nor the number of `+` between them survives. What does survive — and what the
        // pass is actually promising — is the *text* the concatenation evaluates to, so that is
        // what gets compared, and the two token kinds it rearranges leave the multiset.
        let reflows = style.cfg.wrapping.reflow_long_strings;
        if reflows && Self::string_content(src_tree) != Self::string_content(&reparsed.syntax()) {
            return false;
        }

        if !style.cfg.imports.remove_unused {
            let before = Self::collect(src_tree, by_kind, Scope::Everything, reflows);
            let after = Self::collect(&reparsed.syntax(), by_kind, Scope::Everything, reflows);
            return Self::compare(&before, &after, false, allow_extra_braces);
        }

        // Unused-import removal deletes whole declarations, so the import block is checked as a
        // *subset* — but only the import block. Splitting the two scopes keeps the allowance from
        // masking a token dropped anywhere else, which is exactly the class of bug this check
        // exists to catch.
        let before_code = Self::collect(src_tree, by_kind, Scope::OutsideImports, reflows);
        let after_code = Self::collect(&reparsed.syntax(), by_kind, Scope::OutsideImports, reflows);
        if !Self::compare(&before_code, &after_code, false, allow_extra_braces) {
            return false;
        }
        let before_imports = Self::collect(src_tree, by_kind, Scope::ImportsOnly, reflows);
        let after_imports = Self::collect(&reparsed.syntax(), by_kind, Scope::ImportsOnly, reflows);
        Self::compare(&before_imports, &after_imports, true, false)
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

    /// The significant-token multiset of a tree, restricted to `scope`.
    ///
    /// `by_kind` drops the text, which is what makes the check survive `[literals]` rewriting a
    /// literal's spelling while still catching a literal that turned into something else.
    fn collect(root: &SyntaxNode, by_kind: bool, scope: Scope, reflows: bool) -> Budget {
        let mut budget = Budget::new();
        for tok in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| !tok.kind().is_trivia())
            .filter(|tok| scope.admits(tok))
            .filter(|tok| {
                !reflows
                    || !matches!(
                        tok.kind(),
                        SyntaxKind::STRING_LITERAL | SyntaxKind::PLUS | SyntaxKind::TEXT_BLOCK
                    )
            })
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

    /// Every string literal's body, in source order, concatenated.
    ///
    /// The one thing a reflow may not change: where the pieces are cut is layout, what they spell
    /// together is the program.
    fn string_content(root: &SyntaxNode) -> String {
        root.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| tok.kind() == SyntaxKind::STRING_LITERAL)
            .map(|tok| {
                let text = tok.text();
                let body = text
                    .strip_prefix('"')
                    .and_then(|inner| inner.strip_suffix('"'))
                    .unwrap_or(text);
                String::from(body)
            })
            .collect()
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
