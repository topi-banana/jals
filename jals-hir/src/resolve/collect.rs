//! Helpers for pulling binding tokens and byte ranges out of the CST.
//!
//! The fiddly token walks live here, isolated and unit-tested, so the scope builder reads cleanly.
//! Multi-declarator names (`int a, b;`) and catch / resource bindings come from the bespoke
//! accessors added to `jals-syntax`'s `ast::ext`; this module covers the rest.

use alloc::vec::Vec;
use core::ops::Range;

use jals_syntax::SyntaxKind::{IDENT, TYPE_PATTERN};
use jals_syntax::{SyntaxElement, SyntaxNode, SyntaxToken};

/// Namespace for the CST token/range extraction helpers shared across the resolver, the project
/// index, and inference.
pub(crate) struct Collect;

impl Collect {
    /// The tokens directly under `node` (its own trivia and punctuation; operands / types / other
    /// structure are child *nodes*, not direct tokens). The base walk the other extraction helpers
    /// filter.
    pub(crate) fn direct_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
        node.children_with_tokens()
            .filter_map(SyntaxElement::into_token)
    }

    /// The direct `IDENT` token children of `node` (a declaration's names; its type is a nested node,
    /// so its identifiers are not direct token children and are correctly skipped).
    pub(crate) fn direct_ident_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
        Self::direct_tokens(node).filter(|t| t.kind() == IDENT)
    }

    /// The first directly-declared name (`IDENT` token child) of `node`, e.g. a type, method, or
    /// parameter name.
    pub(crate) fn first_ident_token(node: &SyntaxNode) -> Option<SyntaxToken> {
        Self::direct_ident_tokens(node).next()
    }

    /// Every pattern variable bound anywhere within `node` (a switch label or guard).
    ///
    /// Each `TYPE_PATTERN` contributes its binding name; record-pattern nesting is handled by walking
    /// descendants, and an unnamed `_` pattern contributes nothing (it has no `IDENT`).
    pub(crate) fn pattern_var_tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
        node.descendants()
            .filter(|n| n.kind() == TYPE_PATTERN)
            .filter_map(|n| Self::first_ident_token(&n))
            .collect()
    }

    /// The byte range of `token` in the source.
    pub(crate) fn byte_range(token: &SyntaxToken) -> Range<usize> {
        let r = token.text_range();
        usize::from(r.start())..usize::from(r.end())
    }

    /// The start byte offset of `token` in the source.
    pub(crate) fn token_start(token: &SyntaxToken) -> usize {
        usize::from(token.text_range().start())
    }

    /// The byte span of `node` in the source — the key shape used to look an expression's type up in
    /// a `TypeInference` and to anchor a `TypeMismatch`.
    pub(crate) fn node_span(node: &SyntaxNode) -> Range<usize> {
        let r = node.text_range();
        usize::from(r.start())..usize::from(r.end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jals_syntax::SyntaxKind::{METHOD_DECL, SWITCH_LABEL};

    #[allow(clippy::needless_pass_by_value)]
    fn text(tokens: Vec<SyntaxToken>) -> Vec<String> {
        tokens.iter().map(|t| t.text().to_owned()).collect()
    }

    fn node_of(src: &str, kind: jals_syntax::SyntaxKind) -> SyntaxNode {
        jals_exec::block_on_inline(jals_syntax::Parse::parse(src))
            .syntax()
            .descendants()
            .find(|n| n.kind() == kind)
            .expect("node present")
    }

    #[test]
    fn first_ident_token_is_the_method_name_not_its_type() {
        let method = node_of("class C { int compute() { return 0; } }", METHOD_DECL);
        assert_eq!(
            Collect::first_ident_token(&method)
                .map(|t| t.text().to_owned())
                .as_deref(),
            Some("compute"),
        );
    }

    #[test]
    fn pattern_vars_include_nested_record_components() {
        let label = node_of(
            "class C { void m(Object o) { switch (o) { case Point(int x, int y) -> {} default -> {} } } }",
            SWITCH_LABEL,
        );
        assert_eq!(text(Collect::pattern_var_tokens(&label)), ["x", "y"]);
    }

    #[test]
    fn pattern_vars_for_a_plain_type_pattern() {
        let label = node_of(
            "class C { void m(Object o) { switch (o) { case Integer i -> {} default -> {} } } }",
            SWITCH_LABEL,
        );
        assert_eq!(text(Collect::pattern_var_tokens(&label)), ["i"]);
    }
}
