//! R0.3 — modifier ordering, as a plan the `MODIFIERS` visitor emits.
//!
//! google-java-format's `ModifierOrderer`, which it always runs. The canonical sequence is the
//! JLS one (`javax.lang.model.element.Modifier`'s ordinal order), which Google Java Style §4.8.6
//! restates:
//!
//! ```text
//! public protected private abstract default static sealed non-sealed
//! final transient volatile synchronized native strictfp
//! ```
//!
//! Annotations are hoisted ahead of every keyword, keeping their relative order. Neither Eclipse
//! nor IntelliJ has an equivalent, so `[imports] reorder-modifiers` is off by default.
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

/// Canonical modifier ordering.
pub(crate) struct ModifierOrder;

impl ModifierOrder {
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
        if children.iter().any(Self::is_debris) {
            return None;
        }

        let mut ordered: Vec<(usize, usize, SyntaxElement)> = children
            .iter()
            .enumerate()
            .map(|(at, child)| (Self::rank(child), at, child.clone()))
            .collect();
        ordered.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let moved = ordered.iter().enumerate().any(|(at, entry)| entry.1 != at);
        moved.then(|| ordered.into_iter().map(|entry| entry.2).collect())
    }

    /// Whether a child is error-recovery debris that makes reordering unsafe.
    fn is_debris(child: &SyntaxElement) -> bool {
        match child {
            SyntaxElement::Node(node) => !matches!(
                node.kind(),
                SyntaxKind::ANNOTATION | SyntaxKind::ATTRIBUTE | SyntaxKind::NON_SEALED_KW
            ),
            SyntaxElement::Token(tok) => tok.kind() == SyntaxKind::ERROR,
        }
    }

    /// A child's sort rank: annotations and jals attributes first, then the JLS order, then
    /// anything unrecognized.
    fn rank(child: &SyntaxElement) -> usize {
        let kind = match child {
            SyntaxElement::Node(node) => node.kind(),
            SyntaxElement::Token(tok) => tok.kind(),
        };
        if matches!(kind, SyntaxKind::ANNOTATION | SyntaxKind::ATTRIBUTE) {
            return 0;
        }
        Self::CANONICAL
            .iter()
            .position(|canonical| *canonical == kind)
            .map_or(Self::CANONICAL.len() + 2, |at| at + 1)
    }
}
