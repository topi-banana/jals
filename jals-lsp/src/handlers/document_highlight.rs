//! Occurrence highlighting (`textDocument/documentHighlight`).
//!
//! Semantic first: with the cursor on an identifier that resolves to a file-local binding (a
//! local, parameter, field, method, type parameter, or sibling type), every occurrence of *that
//! binding* — its declaration and each reference to it — is highlighted, and nothing else.
//! Shadowing and name-spaces are respected, so a local does not light up a same-named field, and a
//! member access (`obj.field`) of the same spelling is left alone.
//!
//! Lexical fallback: when the identifier under the cursor has no file-local binding — an imported
//! or external type (`String`), an inherited member, an undeclared name — name resolution has
//! nothing to anchor to, so every `IDENT` token with the same text is highlighted instead. That
//! over-matches across unrelated roles, but it keeps an external name highlightable; it is what the
//! syntax layer alone can deliver.
//!
//! Each occurrence is classified from its syntactic context alone: declaration/binding names,
//! simple-name assignment targets (including compound operators like `+=`, which the LSP's
//! single-kind field forces to one side — Write, per rust-analyzer convention), and `++`/`--`
//! operands are [`DocumentHighlightKind::WRITE`]; every other occurrence is
//! [`DocumentHighlightKind::READ`]. Only plain `IDENT` tokens trigger: keywords (including
//! contextual ones like `var`, remapped at parse time), literals, trivia, and `_` yield no
//! highlights.

use std::ops::Range;

use async_lsp::lsp_types::{DocumentHighlight, DocumentHighlightKind, Position};
use jals_syntax::{Parse, SyntaxKind, SyntaxNode, SyntaxToken};
use text_size::TextSize;

use crate::line_index::LineIndex;

/// All occurrences of the symbol under `position`, in document order; empty if the cursor is not
/// on an identifier.
pub(crate) fn document_highlight(
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
    position: Position,
) -> Vec<DocumentHighlight> {
    let root = parse.syntax();
    // `offset` is clamped into `[0, len]`, so `token_at_offset`'s precondition holds. At a
    // boundary between two tokens it yields both; preferring the `IDENT` side keeps a cursor at
    // the end of a word highlighting it (standard editor UX).
    let Some(target) = super::ident_at(&root, line_index.offset(text, position)) else {
        return Vec::new();
    };

    let resolved = jals_hir::resolve_node(&root);
    let anchor = usize::from(target.text_range().start());
    match resolved.symbol_at(anchor) {
        // The identifier names a file-local binding: highlight that binding's declaration and every
        // reference to it — and nothing of the same spelling that means something else. Name
        // resolution yields bare byte ranges, so each token is re-found to read its Read/Write role.
        Some(id) => resolved
            .occurrences(id, true)
            .into_iter()
            .map(|range| highlight_at(&root, line_index, text, range))
            .collect(),
        // No file-local binding (external type, inherited member, undeclared name): fall back to
        // every same-text `IDENT` token. Preorder traversal keeps them in document order, and each
        // token is classified directly — no re-lookup, we already hold it.
        None => root
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|t| t.kind() == SyntaxKind::IDENT && t.text() == target.text())
            .map(|t| DocumentHighlight {
                range: line_index.range(text, t.text_range()),
                kind: Some(classify(&t)),
            })
            .collect(),
    }
}

/// The highlight for the occurrence at byte `range`, classifying the token there. A binding's
/// occurrence ranges arrive from name resolution as bare byte ranges, so the token is re-found to
/// read its syntactic role.
fn highlight_at(
    root: &SyntaxNode,
    line_index: &LineIndex,
    text: &str,
    range: Range<usize>,
) -> DocumentHighlight {
    let kind = super::ident_at(root, TextSize::from(range.start as u32))
        .map(|token| classify(&token))
        .unwrap_or(DocumentHighlightKind::READ);
    DocumentHighlight {
        range: line_index.byte_range(text, &range),
        kind: Some(kind),
    }
}

/// Write for declaration/binding names and mutating uses; Read for everything else.
fn classify(token: &SyntaxToken) -> DocumentHighlightKind {
    use SyntaxKind::*;
    let Some(parent) = token.parent() else {
        return DocumentHighlightKind::READ;
    };
    match parent.kind() {
        // Declaration / binding names. Types live under `TYPE` nodes and initializers under
        // expression nodes, so every *direct* `IDENT` child of these kinds names a declared
        // entity — including each declarator of a flat multi-declarator `FIELD_DECL` /
        // `LOCAL_VAR_DECL` (`int a = 1, b;`).
        CLASS_DECL | RECORD_DECL | INTERFACE_DECL | ANNOTATION_TYPE_DECL | ENUM_DECL
        | METHOD_DECL | CONSTRUCTOR_DECL | TYPE_PARAM | PARAM | RECORD_COMPONENT
        | ENUM_CONSTANT | FIELD_DECL | LOCAL_VAR_DECL | RESOURCE | CATCH_CLAUSE | TYPE_PATTERN
        | FOR_EACH_STMT => DocumentHighlightKind::WRITE,
        NAME_REF => classify_name_ref(&parent),
        _ => DocumentHighlightKind::READ,
    }
}

/// A simple name reference is a write when it is the target of an assignment or the operand
/// of `++`/`--`. Only simple names count: `o.f = 1` keeps `f` (under `FIELD_ACCESS`) a read.
fn classify_name_ref(name_ref: &SyntaxNode) -> DocumentHighlightKind {
    use SyntaxKind::*;
    let is_write = match name_ref.parent() {
        // The target is the first child *node* of `ASSIGNMENT_EXPR` (the operator is a
        // token), matching `AssignmentExpr::target()`.
        Some(p) if p.kind() == ASSIGNMENT_EXPR => p.children().next().as_ref() == Some(name_ref),
        // `POSTFIX_EXPR` is only `x++` / `x--`.
        Some(p) if p.kind() == POSTFIX_EXPR => true,
        // `UNARY_EXPR` also covers `!x`, `-x`, ...; only `++`/`--` mutate.
        Some(p) if p.kind() == UNARY_EXPR => p
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|t| matches!(t.kind(), PLUS_PLUS | MINUS_MINUS)),
        _ => false,
    };
    if is_write {
        DocumentHighlightKind::WRITE
    } else {
        DocumentHighlightKind::READ
    }
}

#[cfg(test)]
mod tests {
    use text_size::TextSize;

    use super::*;

    const W: DocumentHighlightKind = DocumentHighlightKind::WRITE;
    const R: DocumentHighlightKind = DocumentHighlightKind::READ;

    /// Highlights with the cursor at an explicit position, each decoded to
    /// `(line, start_char, end_char, kind)`.
    fn at_pos(
        text: &str,
        line: u32,
        character: u32,
    ) -> Vec<(u32, u32, u32, DocumentHighlightKind)> {
        let idx = LineIndex::new(text);
        document_highlight(
            &jals_syntax::parse(text),
            text,
            &idx,
            Position { line, character },
        )
        .into_iter()
        .map(|h| {
            assert_eq!(
                h.range.start.line, h.range.end.line,
                "identifiers are single-line"
            );
            (
                h.range.start.line,
                h.range.start.character,
                h.range.end.character,
                h.kind.expect("kind is always set"),
            )
        })
        .collect()
    }

    /// Highlights with the cursor at the start of the first occurrence of `needle`.
    fn at(text: &str, needle: &str) -> Vec<(u32, u32, u32, DocumentHighlightKind)> {
        let offset = text.find(needle).expect("needle not found in text");
        let idx = LineIndex::new(text);
        let pos = idx.position(text, TextSize::new(offset as u32));
        at_pos(text, pos.line, pos.character)
    }

    #[test]
    fn local_var_highlights_all_occurrences_with_kinds() {
        let text = "class C { void m() { int x = 1; x = x + 2; f(x); } }";
        assert_eq!(
            at(text, "x"),
            [
                (0, 25, 26, W), // declaration
                (0, 32, 33, W), // assignment target
                (0, 36, 37, R), // right-hand side
                (0, 45, 46, R), // call argument
            ]
        );
    }

    #[test]
    fn triggering_from_a_reference_matches_the_same_set() {
        let text = "class C { void m() { int x = 1; x = x + 2; f(x); } }";
        // Cursor on the call argument `x` (char 45) — same set as from the declaration.
        assert_eq!(at_pos(text, 0, 45), at(text, "x"));
    }

    #[test]
    fn keyword_whitespace_and_literal_yield_empty() {
        let text = "class C { void m() { var x = 1; } }";
        assert_eq!(at(text, "class"), []);
        // Boundary between `{` and whitespace — no identifier on either side. (A position
        // *touching* an identifier highlights it by design; see the boundary test below.)
        assert_eq!(at_pos(text, 0, 9), []);
        assert_eq!(at(text, "1"), []);
        // Contextual keyword: `var` is remapped to `VAR_KW` at parse time, so it is not an
        // `IDENT` and triggers nothing.
        assert_eq!(at(text, "var"), []);
        assert_eq!(at("class C { int i; }", "int"), []);
    }

    #[test]
    fn end_of_identifier_boundary_still_highlights() {
        let text = "class C { void m() { int x = 1; x = x + 2; f(x); } }";
        // Cursor immediately *after* the declaration `x` (char 26): `token_at_offset` yields
        // both neighbors and the `IDENT` side wins.
        assert_eq!(at_pos(text, 0, 26), at(text, "x"));
    }

    #[test]
    fn multiple_declarators_each_name_is_write() {
        // Flat `FIELD_DECL`: both `p` and `q` are direct `IDENT` children of the same node, and
        // the initializer `q = p` references the field `p`.
        let text = "class C { int p = 1, q = p; }";
        assert_eq!(at(text, "q"), [(0, 21, 22, W)]);
        assert_eq!(at(text, "p"), [(0, 14, 15, W), (0, 25, 26, R)]);

        // Same shape for a flat `LOCAL_VAR_DECL`.
        let text = "class C { void m() { int p = 1, q = p; } }";
        assert_eq!(at(text, "q"), [(0, 32, 33, W)]);
        assert_eq!(at(text, "p"), [(0, 25, 26, W), (0, 36, 37, R)]);
    }

    #[test]
    fn compound_assignment_and_inc_dec_are_writes() {
        let text = "class C { void m(int x, boolean b) { x += 2; x++; --x; int y = -x; } }";
        let kinds: Vec<_> = at(text, "x").into_iter().map(|(_, _, _, k)| k).collect();
        // parameter declaration, `+=` target, `x++`, `--x` are writes; the `-x` operand is a read.
        assert_eq!(kinds, [W, W, W, W, R]);

        let text = "class C { void m(boolean flag) { boolean c = !flag; } }";
        let kinds: Vec<_> = at(text, "flag").into_iter().map(|(_, _, _, k)| k).collect();
        // `!flag` does not mutate its operand.
        assert_eq!(kinds, [W, R]);
    }

    #[test]
    fn field_and_shadowing_local_are_distinct() {
        // `name` is a field and, inside `m`, a shadowing local. Name resolution keeps them apart:
        // each highlights only itself, and the member access `o.name` (a bare token, not a name
        // reference) is pulled into neither set.
        let text = "class C { int name; void m(C o) { int name = o.name; } }";
        // The first `name` is the field; the shadowing local and `o.name` are not its uses.
        assert_eq!(at(text, "name"), [(0, 14, 18, W)]);
        // The local declaration (`name = o`) likewise highlights only itself.
        assert_eq!(at(text, "name = o"), [(0, 38, 42, W)]);
    }

    #[test]
    fn type_name_highlights_its_declaration_and_uses() {
        // The class `Foo`, its use as a field type, and the `new Foo()` constructor type are one
        // symbol — all references to the same declaration.
        let text = "class Foo { Foo f = new Foo(); }";
        assert_eq!(
            at(text, "Foo"),
            [(0, 6, 9, W), (0, 12, 15, R), (0, 24, 27, R)]
        );
    }

    #[test]
    fn simple_name_rhs_resolves_past_a_field_access_target() {
        // In `o.f = f`, the assignment target is the member access `o.f` (not a simple name), and
        // the simple-name RHS `f` resolves to the parameter, not the field. Highlighting the
        // parameter covers its declaration and the RHS use; the field `f` has no file-local use.
        let text = "class C { int f; void m(C o, int f) { o.f = f; } }";
        assert_eq!(at(text, "f) {"), [(0, 33, 34, W), (0, 44, 45, R)]); // the parameter `f`
        assert_eq!(at(text, "f;"), [(0, 14, 15, W)]); // the field `f`
    }

    #[test]
    fn for_each_binding_is_write() {
        let text = "class C { void m(int[] xs) { for (int item : xs) use(item); } }";
        assert_eq!(at(text, "item"), [(0, 38, 42, W), (0, 53, 57, R)]);
    }

    #[test]
    fn external_type_falls_back_to_lexical() {
        // `String` has no file-local declaration, so resolution finds no binding and every same-text
        // `IDENT` is highlighted instead — a read in each type position.
        let text = "class C { String a; String b; }";
        assert_eq!(at(text, "String"), [(0, 10, 16, R), (0, 20, 26, R)]);
    }

    #[test]
    fn never_panics_on_broken_or_out_of_range() {
        for text in ["", "class", "class C {", "/* unterminated", "@", "a ="] {
            let idx = LineIndex::new(text);
            let parse = jals_syntax::parse(text);
            for (line, character) in [(0, 0), (999, 999), (0, 999)] {
                document_highlight(&parse, text, &idx, Position { line, character });
            }
        }
    }
}
