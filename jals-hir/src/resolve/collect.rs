//! Helpers for pulling binding tokens and byte ranges out of the CST.
//!
//! The fiddly token walks live here, isolated and unit-tested, so the scope builder reads cleanly.
//! Multi-declarator names (`int a, b;`) and catch / resource bindings come from the bespoke
//! accessors added to `jals-syntax`'s `ast::ext`; this module covers the rest.

use std::ops::Range;

use jals_syntax::SyntaxKind::{IDENT, TYPE_PATTERN};
use jals_syntax::{SyntaxNode, SyntaxToken};

/// The first directly-declared name (`IDENT` token child) of `node`, e.g. a type, method, or
/// parameter name. The type of a declaration is a nested `TYPE` node, so its identifiers are not
/// direct token children and are correctly skipped.
pub(crate) fn first_ident_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|t| t.kind() == IDENT)
}

/// Every pattern variable bound anywhere within `node` (a switch label or guard).
///
/// Each `TYPE_PATTERN` contributes its binding name; record-pattern nesting is handled by walking
/// descendants, and an unnamed `_` pattern contributes nothing (it has no `IDENT`).
pub(crate) fn pattern_var_tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
    node.descendants()
        .filter(|n| n.kind() == TYPE_PATTERN)
        .filter_map(|n| first_ident_token(&n))
        .collect()
}

/// The byte range of `token` in the source.
pub(crate) fn byte_range(token: &SyntaxToken) -> Range<usize> {
    let r = token.text_range();
    usize::from(r.start())..usize::from(r.end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jals_syntax::SyntaxKind::{METHOD_DECL, SWITCH_LABEL};

    fn text(tokens: Vec<SyntaxToken>) -> Vec<String> {
        tokens.iter().map(|t| t.text().to_string()).collect()
    }

    fn node_of(src: &str, kind: jals_syntax::SyntaxKind) -> SyntaxNode {
        jals_syntax::parse(src)
            .syntax()
            .descendants()
            .find(|n| n.kind() == kind)
            .expect("node present")
    }

    #[test]
    fn first_ident_token_is_the_method_name_not_its_type() {
        let method = node_of("class C { int compute() { return 0; } }", METHOD_DECL);
        assert_eq!(
            first_ident_token(&method)
                .map(|t| t.text().to_string())
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
        assert_eq!(text(pattern_var_tokens(&label)), ["x", "y"]);
    }

    #[test]
    fn pattern_vars_for_a_plain_type_pattern() {
        let label = node_of(
            "class C { void m(Object o) { switch (o) { case Integer i -> {} default -> {} } } }",
            SWITCH_LABEL,
        );
        assert_eq!(text(pattern_var_tokens(&label)), ["i"]);
    }
}
