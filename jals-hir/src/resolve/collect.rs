//! Helpers for pulling binding tokens and byte ranges out of the CST.
//!
//! The fiddly token walks live here, isolated and unit-tested, so the scope builder reads cleanly.
//! Multi-declarator names (`int a, b;`) and catch / resource bindings come from the bespoke
//! accessors added to `jals-syntax`'s `ast::ext`; this module covers the rest.

use alloc::vec::Vec;
use core::ops::Range;

use jals_syntax::SyntaxKind::{
    ANNOTATION, ANNOTATION_TYPE_DECL, CLASS_DECL, ENUM_DECL, FIELD_DECL, IDENT, INTERFACE_DECL,
    MODIFIERS, PRIVATE_KW, RECORD_DECL, STATIC_KW, TYPE_PATTERN,
};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::def::DefKind;

/// What a declaration node says about itself beyond the name it binds — the three facts
/// [`Def`](crate::Def) carries so a consumer need not walk back to the CST for them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DeclFacts {
    /// The declaration is written `private`.
    pub is_private: bool,
    /// The declaration is `static` — written, or implied by JLS §9.3.
    pub is_static: bool,
    /// At least one annotation is written on the declaration.
    pub is_annotated: bool,
}

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

    /// What the declaration `node` says about itself: `private`, `static`, and whether anything
    /// annotates it.
    ///
    /// Two shapes carry an annotation and the grammar keeps them apart, so both are read: most
    /// declarations park theirs inside the `MODIFIERS` child, while a type parameter, an enum
    /// constant, and a parameter's type-use position write them as direct `ANNOTATION` children.
    /// A declaration with neither — a pattern variable, a lambda parameter written bare — yields
    /// the default, which is the honest answer rather than a missing one.
    pub(crate) fn decl_facts(node: &SyntaxNode) -> DeclFacts {
        let mut facts = DeclFacts::default();
        // One walk of the child list, not one per question: this runs for every definition the
        // resolver registers, which is a four-figure count on a large file. The keyword scan is
        // one pass over the modifier tokens for the same reason.
        for child in node.children() {
            match child.kind() {
                ANNOTATION => facts.is_annotated = true,
                MODIFIERS => {
                    for token in Self::direct_tokens(&child) {
                        match token.kind() {
                            PRIVATE_KW => facts.is_private = true,
                            STATIC_KW => facts.is_static = true,
                            _ => {}
                        }
                    }
                    facts.is_annotated |= child.children().any(|inner| inner.kind() == ANNOTATION);
                }
                _ => {}
            }
        }
        facts.is_static |= Self::implicitly_static_field(node);
        facts
    }

    /// Whether `node` is a field its enclosing type declares `static` without the source writing
    /// the keyword: JLS §9.3 makes every field in an interface or annotation-type body implicitly
    /// `public static final`.
    ///
    /// The test is the **innermost** enclosing type declaration and not merely *some* interface
    /// ancestor, because a type nested in an interface declares ordinary instance state:
    /// `interface I { record R(int x) {} }` binds `x` to `R`, and `interface I { class C { int x; } }`
    /// binds `x` to `C`. Neither is static.
    ///
    /// Only a field. An interface *method* is not implicitly `static`, which is why the project
    /// index's twin of this rule folds `in_interface` into its `FIELD_DECL` arm alone.
    fn implicitly_static_field(node: &SyntaxNode) -> bool {
        node.kind() == FIELD_DECL
            && node
                .ancestors()
                .find(|ancestor| Self::type_decl_kind(ancestor.kind()).is_some())
                .is_some_and(|ty| matches!(ty.kind(), INTERFACE_DECL | ANNOTATION_TYPE_DECL))
    }

    /// The [`DefKind`] for a type-declaration node kind, or `None` if it is not a type declaration.
    ///
    /// Lives here rather than beside one of its callers because every layer asks it: the resolver
    /// naming a declaration, the index deciding what to record, and the two passes that walk out
    /// to the type a name is written in. It is a fact about the *grammar*, so a consumer that
    /// spelled the five kinds itself would be a copy that a new declaration form silently misses.
    pub(crate) const fn type_decl_kind(kind: SyntaxKind) -> Option<DefKind> {
        match kind {
            CLASS_DECL => Some(DefKind::Class),
            INTERFACE_DECL => Some(DefKind::Interface),
            ENUM_DECL => Some(DefKind::Enum),
            RECORD_DECL => Some(DefKind::Record),
            ANNOTATION_TYPE_DECL => Some(DefKind::AnnotationType),
            _ => None,
        }
    }

    /// The byte span of `node` with the trivia rowan parks inside it trimmed off both ends.
    ///
    /// This CST attaches the trivia between two siblings to the *following* node, so a declaration's
    /// own range starts at the newline before it and a diagnostic drawn over it would underline the
    /// blank line above rather than the code. A node holding no significant token at all (error
    /// recovery) keeps its whole span: there is nothing better to point at.
    pub(crate) fn significant_span(node: &SyntaxNode) -> Range<usize> {
        let mut tokens = node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| !token.kind().is_trivia())
            .map(|token| token.text_range());
        let Some(first) = tokens.next() else {
            return Self::node_span(node);
        };
        let last = tokens.last().unwrap_or(first);
        usize::from(first.start())..usize::from(last.end())
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
