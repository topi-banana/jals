//! Modifier layout: canonical ordering (`reorder-modifiers`) and annotation placement
//! (`annotation-placement`).
//!
//! [`lower_modifiers`] lowers a `MODIFIERS` node. When `reorder-modifiers` is enabled the
//! node's keyword modifiers (`public`, `static`, `final`, …) are sorted into a fixed
//! canonical order and the leading declaration annotations are hoisted to the front (keeping
//! their relative order). A trailing *type-use* annotation run — annotations sitting after a
//! keyword and directly before the type (`public @Nullable Object`) — annotates the type, so it is
//! left in place rather than hoisted. This mirrors the position-based half of google-java-format's
//! split; gjf additionally keeps a *leading* annotation inline when its `@Target` is `TYPE_USE`
//! (`@Nullable Foo m()`), which a syntactic formatter cannot resolve, so leading annotations keep
//! the declaration-annotation behavior here. The pure planning step ([`plan`]: pick the permutation
//! of the element list) is kept separate from `Doc` emission so it is unit-testable without
//! rendering. Emission reuses the original elements, so every token keeps its byte offset, attached
//! comments travel with their modifier, and the significant-token *multiset* is preserved.
//!
//! When `annotation-placement = expanded` and the node belongs to a declaration-level target
//! (a type / method / constructor / field / initializer / local-variable declaration), each
//! annotation in the leading *declaration*-annotation run is broken onto its own line; the
//! trailing type-use run stays inline against the type. This only moves whitespace, so the
//! significant-token *sequence* is preserved exactly.

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::{AnnotationPlacement, Config};
use crate::doc::{Doc, concat, hardline};
use crate::lower::{
    Ctx, first_sig_token, last_sig_token, lower, lower_elements, lower_generic, sep, tok,
};
use crate::rules::StructuralRule;

/// The `reorder-modifiers` / `annotation-placement` rule: owns lowering of a `MODIFIERS` node.
pub(crate) struct ModifierRule;

impl StructuralRule for ModifierRule {
    fn lower(&self, node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
        lower_modifiers(node, ctx)
    }
}

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

/// Whether a `TYPE` node immediately follows this `MODIFIERS` node — i.e. the declaration carries
/// a type whose leading annotations sit in *type-use* position. `rowan`'s [`SyntaxNode::next_sibling`]
/// skips tokens, so this is `Some(TYPE)` for a method (its return type, including `void` / primitives,
/// is a `TYPE` node), a field, a local variable, a parameter, and a record component; it is *not*
/// `TYPE` for a constructor (next sibling is the `PARAM_LIST`), a type declaration (a `CLASS_BODY`
/// etc.), an initializer (a `BLOCK`), or a generic method whose `TYPE_PARAMS` come first. In those
/// type-less / type-params-first cases there is no trailing type-use run, so every annotation is a
/// declaration annotation — as google-java-format also formats them.
fn type_node_follows(node: &SyntaxNode) -> bool {
    node.next_sibling().map(|n| n.kind()) == Some(S::TYPE)
}

/// The index at which the trailing *type-use* annotation run begins in `els` (a `MODIFIERS` node's
/// significant elements, in emitted order): the maximal run of trailing `ANNOTATION`s that sit after
/// the last keyword modifier and directly before the type. These annotate the type, so they stay
/// inline and are never hoisted — the position-based half of google-java-format's split that a
/// syntactic formatter can reproduce. Two cases yield an empty run (`els.len()`): no type follows
/// (`type_follows == false`, e.g. a constructor — no type-use position exists), and a run with no
/// preceding keyword modifier (the whole list, which gjf classifies by `@Target`; see the body).
fn trailing_type_use_start(els: &[SyntaxElement], type_follows: bool) -> usize {
    if !type_follows {
        return els.len();
    }
    let mut i = els.len();
    while i > 0 && els[i - 1].kind() == S::ANNOTATION {
        i -= 1;
    }
    // Only a run *after* a keyword modifier is recognized as type-use position. A run with no
    // preceding keyword (the whole list, e.g. `@Override void m()` or `@Nullable Foo handle()`)
    // is syntactically a declaration annotation; google-java-format keeps it inline only when its
    // `@Target` is `TYPE_USE`, which a purely syntactic formatter cannot resolve, so those keep
    // the declaration-annotation behavior (break under `expanded`, hoist under `reorder`).
    if i == 0 { els.len() } else { i }
}

/// Reorder a `MODIFIERS` node's elements: the leading declaration annotations (and any non-modifier
/// element) first in their original order, then the keyword modifiers in canonical order, then the
/// trailing *type-use* annotation run left untouched at the end. `type_follows` reports whether a
/// type follows the modifiers (see [`type_node_follows`]); when it does, the trailing annotation run
/// directly before the type annotates the type and is kept in place rather than hoisted to the front.
/// The pure planning step, separate from `Doc` emission so it is unit-testable without rendering.
///
/// Every input element is returned exactly once (the multiset is preserved). The sort is
/// **stable**, so equal-rank duplicates keep their order and an already-canonical list is
/// returned unchanged — which keeps formatting idempotent (the trailing run is at a fixed point
/// because it sits after every keyword and so is never re-classified as leading).
pub(crate) fn plan(els: Vec<SyntaxElement>, type_follows: bool) -> Vec<SyntaxElement> {
    let split = trailing_type_use_start(&els, type_follows);
    let mut head = els;
    let trailing = head.split_off(split);
    let (mut mods, front): (Vec<SyntaxElement>, Vec<SyntaxElement>) =
        head.into_iter().partition(|e| rank_of(e.kind()).is_some());
    mods.sort_by_key(|e| rank_of(e.kind()).unwrap_or(usize::MAX));
    let mut out = front;
    out.extend(mods);
    out.extend(trailing);
    out
}

/// The first significant token of an element: the first non-trivia token of a node, or the
/// token itself.
fn element_first_token(el: &SyntaxElement) -> Option<SyntaxToken> {
    match el.as_node() {
        Some(n) => first_sig_token(n),
        None => el.as_token().cloned(),
    }
}

/// The last significant token of an element: the last non-trivia token of a node, or the token
/// itself.
fn element_last_token(el: &SyntaxElement) -> Option<SyntaxToken> {
    match el.as_node() {
        Some(n) => last_sig_token(n),
        None => el.as_token().cloned(),
    }
}

/// Whether reordering this `MODIFIERS` node is safe — i.e. it sits in a genuine declaration
/// context and is not poisoned by a preceding annotation-absorbing token. In valid code a
/// `MODIFIERS` node always has one of these parents. An error-recovery artifact does not: e.g.
/// `<public@` parses a stray `MODIFIERS` (holding `public` and an incomplete `@`) directly under
/// `SOURCE_FILE`, next to a `TYPE_PARAMS` sibling. Hoisting the annotation to the front there
/// changes the significant-token *sequence* such that re-parsing the output regroups the `@` into
/// the preceding `<…>` as a type-parameter annotation — a different tree, so the layout never
/// reaches a fixed point. Reordering is confined to these contexts so the multiset-preserving
/// relaxation never costs idempotency; elsewhere the node emits in source order (the
/// byte-for-byte-stable baseline).
fn is_reorderable_context(node: &SyntaxNode) -> bool {
    matches!(
        node.parent().map(|p| p.kind()),
        Some(
            S::CLASS_DECL
                | S::INTERFACE_DECL
                | S::ENUM_DECL
                | S::RECORD_DECL
                | S::ANNOTATION_TYPE_DECL
                | S::MODULE_DECL
                | S::METHOD_DECL
                | S::CONSTRUCTOR_DECL
                | S::FIELD_DECL
                | S::INITIALIZER
                | S::LOCAL_VAR_DECL
                | S::PARAM
                | S::FOR_EACH_STMT
                | S::RESOURCE
                | S::CATCH_CLAUSE
        )
    ) && !preceded_by_dangling_lt(node)
}

/// Whether the significant token immediately preceding this `MODIFIERS` run is a `<` (`LT`). Even
/// inside a genuine declaration, an error-recovery `<` directly before the modifiers (a dangling
/// `TYPE_PARAMS` opener, as in `<public@x x`, where `public @x` lands in a real `FIELD_DECL` next
/// to a stray `<` sibling) would absorb a hoisted leading `@` into the preceding `<…>` on re-parse
/// as a type-parameter annotation — the same idempotency trap [`is_reorderable_context`] guards
/// against. A `@` is only ever legal directly after a `<` in type-parameter / type-use position,
/// so a preceding `<` fully characterizes this absorber; in valid code a declaration's modifiers
/// are never preceded by a bare `<`, so this never fires there.
///
/// Sibling traversal is used rather than [`SyntaxToken::prev_token`], which does not cross the
/// empty error-recovery nodes (an empty `TYPE_PARAM`) that produce exactly this shape.
fn preceded_by_dangling_lt(node: &SyntaxNode) -> bool {
    // Walk back from the modifiers' owning declaration to the nearest preceding significant token.
    let mut cursor = node.parent().and_then(|decl| decl.prev_sibling_or_token());
    while let Some(el) = cursor {
        match el {
            SyntaxElement::Token(t) => {
                if !t.kind().is_trivia() {
                    return t.kind() == S::LT;
                }
                cursor = t.prev_sibling_or_token();
            }
            SyntaxElement::Node(n) => match last_sig_token(&n) {
                Some(t) => return t.kind() == S::LT,
                None => cursor = n.prev_sibling_or_token(),
            },
        }
    }
    false
}

/// The first and last significant tokens of a `MODIFIERS` node *as emitted*. With
/// `reorder-modifiers` off (or in a non-reorderable error-recovery context) these are the
/// structural [`first_sig_token`] / [`last_sig_token`]; with it on, [`plan`] may move an
/// annotation to the front (or a keyword to the end), so the emitted boundary tokens differ from
/// the structural ones.
///
/// The parent node uses these (rather than the structural tokens) to compute the separators
/// around the `MODIFIERS` node, keeping the boundary spacing consistent with what is actually
/// emitted: when [`plan`] hoists an annotation that was structurally last to the front, the
/// emitted-last token becomes a keyword, and the parent's trailing separator must follow the
/// keyword (not the structural-last `@`) so the spacing is the same on every pass. Reordering is
/// confined to genuine declaration contexts ([`is_reorderable_context`]), so this only ever runs
/// where the boundary token is followed by ordinary, space-separated declaration syntax.
pub(crate) fn emitted_boundary_tokens(
    node: &SyntaxNode,
    cfg: &Config,
) -> (Option<SyntaxToken>, Option<SyntaxToken>) {
    if !cfg.reorder_modifiers || !is_reorderable_context(node) {
        return (first_sig_token(node), last_sig_token(node));
    }
    let els: Vec<SyntaxElement> = node
        .children_with_tokens()
        .filter(|e| !e.kind().is_trivia())
        .collect();
    let planned = plan(els, type_node_follows(node));
    let first = planned.first().and_then(element_first_token);
    let last = planned.last().and_then(element_last_token);
    (first, last)
}

/// The declaration-level targets whose leading declaration-annotation run
/// `annotation-placement = expanded` breaks onto its own line. A parameter's `MODIFIERS` (parent
/// `PARAM`) is deliberately excluded so parameter annotations stay inline; enum-constant /
/// type-parameter annotations never live in a `MODIFIERS` node and so never reach this code at
/// all. A type-use annotation written directly before the type *does* land in `MODIFIERS` (the
/// lexer-driven `modifiers()` rule is greedy), but [`lower_modifiers_with_breaks`] keeps that
/// trailing run inline regardless of this predicate.
fn is_decl_level_modifiers(node: &SyntaxNode) -> bool {
    matches!(
        node.parent().map(|p| p.kind()),
        Some(
            S::CLASS_DECL
                | S::INTERFACE_DECL
                | S::ENUM_DECL
                | S::RECORD_DECL
                | S::ANNOTATION_TYPE_DECL
                | S::METHOD_DECL
                | S::CONSTRUCTOR_DECL
                | S::FIELD_DECL
                | S::INITIALIZER
                | S::LOCAL_VAR_DECL
        )
    )
}

/// Lower a `MODIFIERS` node. With `reorder-modifiers` off and `annotation-placement = compact`
/// this is exactly [`lower_generic`] (byte-identical to the prior behavior). With
/// `reorder-modifiers` on, the node's significant children are reordered by [`plan`]; with
/// `annotation-placement = expanded` on a declaration-level target, the leading annotations are
/// broken onto their own lines by [`lower_modifiers_with_breaks`].
///
/// The reorder is confined to the `MODIFIERS` subtree; the rowan tree is unchanged. For valid
/// code the boundary spacing is order-invariant (any modifier/annotation end takes a single
/// space before the following type token), but malformed input can put an annotation
/// structurally last while reordering emits a keyword last, desyncing the parent's trailing
/// separator. So the parent computes the separators around this node from
/// [`emitted_boundary_tokens`] (the emitted-order first / last token), not the structural ones,
/// which keeps idempotency. When the last emitted part is a forced break, that trailing parent
/// space is trimmed by the renderer.
pub(crate) fn lower_modifiers(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let expanded = ctx.cfg.annotation_placement == AnnotationPlacement::Expanded;
    // The hot path: nothing to reorder and no annotation to break out.
    if !ctx.cfg.reorder_modifiers && !expanded {
        return lower_generic(node, ctx);
    }
    // Whether a type follows the modifiers; both the reorder and the break step keep the trailing
    // type-use annotation run (directly before that type) inline instead of treating it as a
    // declaration annotation.
    let type_follows = type_node_follows(node);
    let els: Vec<SyntaxElement> = node
        .children_with_tokens()
        .filter(|e| !e.kind().is_trivia())
        .collect();
    // Reorder only in a genuine declaration context; a stray `MODIFIERS` from error recovery is
    // emitted in source order so reordering can't change the re-parse and break idempotency
    // (see [`is_reorderable_context`]).
    let els = if ctx.cfg.reorder_modifiers && is_reorderable_context(node) {
        plan(els, type_follows)
    } else {
        els
    };
    if expanded && is_decl_level_modifiers(node) {
        lower_modifiers_with_breaks(&els, ctx, type_follows)
    } else {
        lower_elements(els.into_iter(), ctx, false)
    }
}

/// Lay out a declaration's modifiers, breaking each annotation in the leading contiguous run
/// onto its own line (`annotation-placement = expanded`). Mirrors [`lower_elements`]'s inline
/// emission, but the separator *after* a leading-run annotation is a forced break instead of a
/// space. Only the leading *declaration*-annotation run breaks; two kinds of annotation stay
/// inline: one interleaved after a keyword (`public @A static`, possible only without
/// `reorder-modifiers`) and the trailing *type-use* run sitting after a keyword, directly before
/// the type (`public @Nullable Object`). The trailing run is identified by `type_follows` +
/// [`trailing_type_use_start`], the position-based half of google-java-format's split. With
/// `reorder-modifiers` on, [`plan`] has already grouped the leading declaration annotations into one
/// front run, so every one of those breaks.
fn lower_modifiers_with_breaks(els: &[SyntaxElement], ctx: &Ctx<'_>, type_follows: bool) -> Doc {
    let trailing_start = trailing_type_use_start(els, type_follows);
    let mut parts: Vec<Doc> = Vec::new();
    let mut prev: Option<SyntaxToken> = None;
    // Whether the previous emitted element was an annotation in the leading run (so this
    // element starts a fresh line).
    let mut prev_was_leading_annotation = false;
    // Whether we are still inside the leading contiguous run of annotations.
    let mut still_leading = true;

    for (idx, el) in els.iter().enumerate() {
        let is_annotation = el.kind() == S::ANNOTATION;
        // An annotation in the trailing type-use run annotates the type, so it stays inline.
        let in_trailing_run = idx >= trailing_start;
        if !is_annotation {
            still_leading = false;
        }
        let is_leading_annotation = is_annotation && still_leading && !in_trailing_run;

        let first = element_first_token(el);
        let el_doc = match el.as_node() {
            Some(n) => lower(n, ctx),
            None => tok(el.as_token().expect("element is a node or a token"), ctx),
        };
        let last = element_last_token(el);

        if let Some(first) = first.as_ref() {
            let s = if prev_was_leading_annotation {
                hardline()
            } else {
                sep(prev.as_ref(), first, ctx.cfg)
            };
            parts.push(s);
        }
        parts.push(el_doc);
        if last.is_some() {
            prev = last;
        }
        prev_was_leading_annotation = is_leading_annotation;
    }
    // When the leading run is the whole modifier list and no type follows (e.g. `@A @B class D`
    // or a constructor's `@A @B C()`), the break before the declaration keyword must be emitted
    // here as a trailing forced break — that keyword lives in the parent node, not in `MODIFIERS`.
    // The renderer drops the parent's following separator space at the fresh line's start. A
    // trailing type-use run never reaches this (it is not a leading annotation), so the modifiers
    // end inline against the type, as in `public @Nullable Object`.
    if prev_was_leading_annotation {
        parts.push(hardline());
    }
    concat(parts)
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

    /// The labels of `member`'s modifiers after planning (a field/method member, so a type
    /// follows the modifiers — the trailing type-use run is detected from the real node).
    fn planned(member: &str) -> Vec<String> {
        let node = modifiers_of(member);
        labels(&plan(sig_els(&node), type_node_follows(&node)))
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
        // `@Foo` is a leading declaration annotation (before a keyword) and is hoisted to the
        // front; `@Bar` sits directly before the type, so it is a trailing type-use annotation and
        // stays put after the sorted keywords. Their relative order (`@Foo` before `@Bar`) holds.
        assert_eq!(
            planned("static @Foo public @Bar int x;"),
            ["@", "public", "static", "@"]
        );
    }

    #[test]
    fn keeps_trailing_type_use_annotation_in_place() {
        // An annotation directly before the type *after a keyword* is recognized as type-use
        // position and is not hoisted to the front.
        assert_eq!(planned("public @Nullable Object x;"), ["public", "@"]);
        // With no preceding keyword the run cannot be classified positionally, so it is treated as
        // a (leading) declaration annotation — here a no-op since there is nothing to reorder.
        assert_eq!(planned("@Nullable Object x;"), ["@"]);
    }

    #[test]
    fn hoists_all_annotations_when_no_type_follows() {
        // A constructor has no type, so even an annotation written after a keyword is a
        // declaration annotation and is hoisted to the front.
        let node = modifiers_under("class C { public @Foo C() {} }", S::CONSTRUCTOR_DECL);
        assert!(!type_node_follows(&node));
        assert_eq!(
            labels(&plan(sig_els(&node), type_node_follows(&node))),
            ["@", "public"]
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
        let node = modifiers_of("volatile @Foo private static final int x;");
        let type_follows = type_node_follows(&node);
        let once = plan(sig_els(&node), type_follows);
        let twice = plan(once.clone(), type_follows);
        assert_eq!(labels(&once), labels(&twice));
    }

    #[test]
    fn preserves_multiset() {
        // Duplicate keywords (error recovery) are kept; the output is a permutation of the input.
        let node = modifiers_of("static public static int x;");
        let input = sig_els(&node);
        let mut before = labels(&input);
        let output = plan(input, type_node_follows(&node));
        let mut after = labels(&output);
        before.sort();
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn empty_modifiers_plans_to_empty() {
        assert!(plan(Vec::new(), true).is_empty());
    }

    /// The first `MODIFIERS` node in `src` whose parent has kind `parent`.
    fn modifiers_under(src: &str, parent: S) -> SyntaxNode {
        jals_syntax::parse(src)
            .syntax()
            .descendants()
            .filter(|n| n.kind() == S::MODIFIERS)
            .find(|n| n.parent().map(|p| p.kind()) == Some(parent))
            .unwrap_or_else(|| panic!("test source has a MODIFIERS node under {parent:?}"))
    }

    #[test]
    fn decl_level_modifiers_detected() {
        // A field / method member's MODIFIERS is a declaration-level target.
        assert!(is_decl_level_modifiers(&modifiers_of("public int x;")));
        assert!(is_decl_level_modifiers(&modifiers_of("public void m() {}")));
        // A local-variable declaration is included too.
        assert!(is_decl_level_modifiers(&modifiers_under(
            "class C { void m() { final int y = 1; } }",
            S::LOCAL_VAR_DECL,
        )));
    }

    #[test]
    fn param_modifiers_not_decl_level() {
        // A parameter's MODIFIERS is excluded so parameter annotations stay inline.
        let modifiers = modifiers_under("class C { void m(final int x) {} }", S::PARAM);
        assert!(!is_decl_level_modifiers(&modifiers));
    }

    #[test]
    fn dangling_lt_before_modifiers_is_not_reorderable() {
        // A normal declaration's modifiers reorder freely.
        assert!(is_reorderable_context(&modifiers_of("public @A int x;")));
        // Error recovery can place a real FIELD_DECL right after a stray `<` (a dangling
        // TYPE_PARAMS opener): `<public@x x` parses `public @x` into a FIELD_DECL next to a `<`
        // sibling. Hoisting the annotation to the front there would let the re-parse absorb the
        // `@` into the preceding `<…>` as a type-parameter annotation — a different tree — so the
        // node is not reorderable and emits in source order, keeping the layout idempotent.
        let modifiers = modifiers_under("<public@x x", S::FIELD_DECL);
        assert!(!is_reorderable_context(&modifiers));
    }
}
