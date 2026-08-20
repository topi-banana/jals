//! R0.3 — modifier ordering, as a plan the `MODIFIERS` visitor emits.
//!
//! google-java-format's `ModifierOrderer`, which it always runs. The canonical sequence is the
//! JLS one (`javax.lang.model.element.Modifier`'s ordinal order), which the Google Java Style
//! restates as:
//!
//! ```text
//! public protected private abstract default static sealed non-sealed
//! final transient volatile synchronized native strictfp
//! ```
//!
//! Only **runs of consecutive keyword modifiers** are sorted, and each within itself: an
//! annotation ends a run and never moves, so `final @A C c` is left exactly as written. That is
//! `ModifierOrderer`'s own shape — it walks the token stream and stops a run at the first token
//! that is not a modifier. Neither Eclipse nor IntelliJ has an equivalent, so
//! `[imports] reorder-modifiers` is off by default (that section owns it because it is a
//! token-reordering pass, not a layout rule).
//!
//! Like [`ImportPlan`](super::import_order::ImportPlan) this is a **reordering of the original
//! nodes**, so the token multiset is preserved and each modifier's comments travel with it.
//!
//! # When it declines
//!
//! A `MODIFIERS` node holding an `ERROR` child is emitted in source order. Error recovery can
//! park unrelated debris there — a dangling `<` from a half-typed generic, say — and moving
//! tokens across it would reorder things that are not modifiers at all.

use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub(crate) use api::plan;

/// Canonical modifier ordering.
pub(crate) mod api {
    use super::{SyntaxElement, SyntaxKind, SyntaxNode, Vec};

    /// The JLS order. A kind's index here is its sort key; anything absent sorts last, keeping
    /// its relative position.
    const CANONICAL: [SyntaxKind; 14] = [
        SyntaxKind::PUBLIC_KW,
        SyntaxKind::PROTECTED_KW,
        SyntaxKind::PRIVATE_KW,
        SyntaxKind::ABSTRACT_KW,
        SyntaxKind::DEFAULT_KW,
        SyntaxKind::STATIC_KW,
        SyntaxKind::SEALED_KW,
        SyntaxKind::NON_SEALED_KW,
        SyntaxKind::FINAL_KW,
        SyntaxKind::TRANSIENT_KW,
        SyntaxKind::VOLATILE_KW,
        SyntaxKind::SYNCHRONIZED_KW,
        SyntaxKind::NATIVE_KW,
        SyntaxKind::STRICTFP_KW,
    ];

    /// The emission order of a `MODIFIERS` node's children.
    ///
    /// Returns `None` when nothing should move — the rule is off, the node holds error-recovery
    /// debris, or the source is already canonical — so the visitor can take its ordinary path.
    pub(crate) fn plan(node: &SyntaxNode, enabled: bool) -> Option<Vec<SyntaxElement>> {
        if !enabled {
            return None;
        }
        let children: Vec<SyntaxElement> = node
            .children_with_tokens()
            .filter(|child| !child.as_token().is_some_and(|tok| tok.kind().is_trivia()))
            .collect();
        if children.iter().any(is_debris) {
            return None;
        }

        // Each *run* of consecutive keyword modifiers is sorted within itself; an annotation ends
        // a run and never moves. `final @A C c` is left alone, and `@A public static` keeps the
        // annotation in front of the pair it already precedes.
        let mut ordered: Vec<SyntaxElement> = Vec::with_capacity(children.len());
        let mut run: Vec<SyntaxElement> = Vec::new();
        for child in &children {
            if rank(child).is_some() {
                run.push(child.clone());
                continue;
            }
            flush_run(&mut run, &mut ordered);
            ordered.push(child.clone());
        }
        flush_run(&mut run, &mut ordered);

        let moved = ordered
            .iter()
            .zip(&children)
            .any(|(after, before)| after != before);
        moved.then_some(ordered)
    }

    /// Append `run` to `ordered` in canonical order, emptying it.
    pub(crate) fn flush_run(run: &mut Vec<SyntaxElement>, ordered: &mut Vec<SyntaxElement>) {
        run.sort_by_key(|child| rank(child).unwrap_or(usize::MAX));
        ordered.append(run);
    }

    /// Whether a child is error-recovery debris that makes reordering unsafe.
    pub(crate) fn is_debris(child: &SyntaxElement) -> bool {
        match child {
            SyntaxElement::Node(node) => !matches!(
                node.kind(),
                SyntaxKind::ANNOTATION | SyntaxKind::ATTRIBUTE | SyntaxKind::NON_SEALED_KW
            ),
            SyntaxElement::Token(tok) => tok.kind() == SyntaxKind::ERROR,
        }
    }

    /// A keyword modifier's rank in the JLS order, or `None` for anything that is not one — an
    /// annotation, a jals attribute, or a keyword this list does not know.
    pub(crate) fn rank(child: &SyntaxElement) -> Option<usize> {
        let kind = match child {
            SyntaxElement::Node(node) => node.kind(),
            SyntaxElement::Token(tok) => tok.kind(),
        };
        CANONICAL.iter().position(|canonical| *canonical == kind)
    }
}
