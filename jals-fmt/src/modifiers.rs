//! Canonical ordering of a declaration's modifiers: `reorder-modifiers`.
//!
//! [`lower_modifiers`] lowers a `MODIFIERS` node; when `reorder-modifiers` is enabled the
//! node's keyword modifiers (`public`, `static`, `final`, …) are sorted into a fixed
//! canonical order and all annotations are hoisted to the front (keeping their relative
//! order). The pure planning step ([`plan`]: pick the permutation of the element list) is
//! kept separate from `Doc` emission so it is unit-testable without rendering. Emission
//! reuses the original elements, so every token keeps its byte offset, attached comments
//! travel with their modifier, and the significant-token *multiset* is preserved.

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode};

use crate::doc::Doc;
use crate::lower::{Ctx, lower_elements, lower_generic};

/// The canonical rank of a keyword modifier, or `None` for any other element (annotations and
/// anything else stay fixed, hoisted to the front). The order follows the JLS recommended
/// modifier order (§8.1.1 / §8.3.1 / §8.4.3) as codified by Checkstyle's `ModifierOrder` and
/// the Google Java Style Guide. `SEALED_KW` is a token (promoted from `IDENT` only inside the
/// `modifiers()` rule) and `NON_SEALED_KW` is a node; both rank uniformly via `SyntaxKind`.
fn rank_of(kind: S) -> Option<usize> {
    Some(match kind {
        S::PUBLIC_KW => 0,
        S::PROTECTED_KW => 1,
        S::PRIVATE_KW => 2,
        S::ABSTRACT_KW => 3,
        S::DEFAULT_KW => 4,
        S::STATIC_KW => 5,
        S::SEALED_KW => 6,
        S::NON_SEALED_KW => 7,
        S::FINAL_KW => 8,
        S::TRANSIENT_KW => 9,
        S::VOLATILE_KW => 10,
        S::SYNCHRONIZED_KW => 11,
        S::NATIVE_KW => 12,
        S::STRICTFP_KW => 13,
        _ => return None,
    })
}

/// Reorder a `MODIFIERS` node's elements: annotations (and any non-modifier element) first, in
/// their original order, then the keyword modifiers in canonical order. The pure planning step,
/// separate from `Doc` emission so it is unit-testable without rendering.
///
/// Every input element is returned exactly once (the multiset is preserved). The sort is
/// **stable**, so equal-rank duplicates keep their order and an already-canonical list is
/// returned unchanged — which keeps formatting idempotent.
pub(crate) fn plan(els: Vec<SyntaxElement>) -> Vec<SyntaxElement> {
    let (mut mods, front): (Vec<SyntaxElement>, Vec<SyntaxElement>) =
        els.into_iter().partition(|e| rank_of(e.kind()).is_some());
    mods.sort_by_key(|e| rank_of(e.kind()).unwrap_or(usize::MAX));
    let mut out = front;
    out.extend(mods);
    out
}

/// Lower a `MODIFIERS` node. With `reorder-modifiers` off this is exactly [`lower_generic`]
/// (byte-identical to the prior behavior). With it on, the node's significant children are
/// reordered by [`plan`] before emission.
///
/// The reorder is confined to the `MODIFIERS` subtree; the rowan tree is unchanged, so the
/// parent's separator before/after this node (computed from the node's source-order first /
/// last significant token) is unaffected. That is harmless: the spacing rule yields a single
/// space between any modifier/annotation end and any following type token regardless of which
/// modifier sorts last, so the boundary is order-invariant and idempotency holds.
pub(crate) fn lower_modifiers(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    if !ctx.cfg.reorder_modifiers {
        return lower_generic(node, ctx);
    }
    let els: Vec<SyntaxElement> = node
        .children_with_tokens()
        .filter(|e| !e.kind().is_trivia())
        .collect();
    lower_elements(plan(els).into_iter(), ctx, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first non-empty `MODIFIERS` node of the member in `class C { <member> }` (the outer
    /// class decl carries its own empty `MODIFIERS`, which is skipped).
    fn modifiers_of(member: &str) -> SyntaxNode {
        let src = format!("class C {{ {member} }}");
        jals_syntax::parse(&src)
            .syntax()
            .descendants()
            .filter(|n| n.kind() == S::MODIFIERS)
            .find(|n| n.children_with_tokens().any(|e| !e.kind().is_trivia()))
            .expect("test source has a non-empty MODIFIERS node")
    }

    /// The significant (non-trivia) child elements of a `MODIFIERS` node.
    fn sig_els(node: &SyntaxNode) -> Vec<SyntaxElement> {
        node.children_with_tokens()
            .filter(|e| !e.kind().is_trivia())
            .collect()
    }

    /// A compact label for an element: the modifier/annotation text (annotations as `@`). A
    /// node's text can carry leading trivia, so it is trimmed (e.g. `NON_SEALED_KW`).
    fn label(e: &SyntaxElement) -> String {
        if e.kind() == S::ANNOTATION {
            "@".to_string()
        } else {
            e.as_token()
                .map(|t| t.text().to_string())
                .unwrap_or_else(|| {
                    e.as_node()
                        .map(|n| n.text().to_string().trim().to_string())
                        .unwrap_or_default()
                })
        }
    }

    fn labels(els: &[SyntaxElement]) -> Vec<String> {
        els.iter().map(label).collect()
    }

    /// The labels of `member`'s modifiers after planning.
    fn planned(member: &str) -> Vec<String> {
        labels(&plan(sig_els(&modifiers_of(member))))
    }

    #[test]
    fn sorts_keyword_modifiers() {
        assert_eq!(
            planned("final public static int x;"),
            ["public", "static", "final"]
        );
    }

    #[test]
    fn already_canonical_is_unchanged() {
        assert_eq!(
            planned("public static final int x;"),
            ["public", "static", "final"]
        );
    }

    #[test]
    fn hoists_annotations_to_front() {
        // An annotation interleaved with keywords is moved to the front; keywords sort.
        assert_eq!(
            planned("public @Foo static int x;"),
            ["@", "public", "static"]
        );
    }

    #[test]
    fn keeps_relative_annotation_order() {
        assert_eq!(
            planned("static @Foo public @Bar int x;"),
            ["@", "@", "public", "static"]
        );
    }

    #[test]
    fn single_modifier_unchanged() {
        assert_eq!(planned("final int x;"), ["final"]);
    }

    #[test]
    fn annotation_only_unchanged() {
        assert_eq!(planned("@Foo int x;"), ["@"]);
    }

    #[test]
    fn sealed_and_non_sealed_rank() {
        // `non-sealed` (rank 7) follows `sealed` (rank 6) and both precede `final` (rank 8).
        assert_eq!(planned("final sealed class D {}"), ["sealed", "final"]);
        assert_eq!(
            planned("final non-sealed class D {}"),
            ["non-sealed", "final"]
        );
    }

    #[test]
    fn plan_is_idempotent() {
        let once = plan(sig_els(&modifiers_of(
            "volatile @Foo private static final int x;",
        )));
        let twice = plan(once.clone());
        assert_eq!(labels(&once), labels(&twice));
    }

    #[test]
    fn preserves_multiset() {
        // Duplicate keywords (error recovery) are kept; the output is a permutation of the input.
        let input = sig_els(&modifiers_of("static public static int x;"));
        let mut before = labels(&input);
        let output = plan(input);
        let mut after = labels(&output);
        before.sort();
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn empty_modifiers_plans_to_empty() {
        assert!(plan(Vec::new()).is_empty());
    }
}
