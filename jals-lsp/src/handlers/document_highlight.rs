//! Lexical occurrence highlighting (`textDocument/documentHighlight`).
//!
//! Purely lexical: with the cursor on an identifier, every `IDENT` token in the document with
//! the same text is highlighted — there is no name resolution, so occurrences in unrelated
//! roles (a field access, a type name, a label) light up too. That over-matching is intended;
//! it is what "lexical" promises and what the syntax layer can deliver.
//!
//! Each occurrence is classified from its syntactic context alone: declaration/binding names,
//! simple-name assignment targets (including compound operators like `+=`, which the LSP's
//! single-kind field forces to one side — Write, per rust-analyzer convention), and `++`/`--`
//! operands are [`DocumentHighlightKind::WRITE`]; every other occurrence is
//! [`DocumentHighlightKind::READ`]. Only plain `IDENT` tokens trigger: keywords (including
//! contextual ones like `var`, remapped at parse time), literals, trivia, and `_` yield no
//! highlights.

use async_lsp::lsp_types::{DocumentHighlight, DocumentHighlightKind, Position};
use jals_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use crate::line_index::LineIndex;

/// All same-text occurrences of the identifier under `position`, in document order; empty if
/// the cursor is not on an identifier.
pub(crate) fn document_highlight(
    text: &str,
    line_index: &LineIndex,
    position: Position,
) -> Vec<DocumentHighlight> {
    let root = jals_syntax::parse(text).syntax();
    // `offset` is clamped into `[0, len]`, so `token_at_offset`'s precondition holds. At a
    // boundary between two tokens it yields both; preferring the `IDENT` side keeps a cursor
    // at the end of a word highlighting it (standard editor UX).
    let offset = line_index.offset(text, position);
    let Some(target) = root
        .token_at_offset(offset)
        .find(|token| token.kind() == SyntaxKind::IDENT)
    else {
        return Vec::new();
    };
    // Preorder traversal, so the results are already in document order.
    root.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::IDENT && token.text() == target.text())
        .map(|token| DocumentHighlight {
            range: line_index.range(text, token.text_range()),
            kind: Some(classify(&token)),
        })
        .collect()
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
        document_highlight(text, &idx, Position { line, character })
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
        // Flat `FIELD_DECL`: both `p` and `q` are direct `IDENT` children of the same node.
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
        // parameter, `+=` target, `x++`, `--x` are writes; the `-x` operand is a read.
        assert_eq!(kinds, [W, W, W, W, R]);

        let text = "class C { void m(boolean flag) { boolean c = !flag; } }";
        let kinds: Vec<_> = at(text, "flag").into_iter().map(|(_, _, _, k)| k).collect();
        // `!flag` does not mutate its operand.
        assert_eq!(kinds, [W, R]);
    }

    #[test]
    fn field_access_matches_lexically() {
        // No name resolution: the field `name`, the local `name`, and `o.name` all match.
        let text = "class C { int name; void m(C o) { int name = o.name; } }";
        assert_eq!(
            at(text, "name"),
            [(0, 14, 18, W), (0, 38, 42, W), (0, 47, 51, R)]
        );
    }

    #[test]
    fn type_and_variable_with_same_text_all_match() {
        // Lexical over-matching is intended: type positions match the class name too.
        let text = "class Foo { Foo f = new Foo(); }";
        assert_eq!(
            at(text, "Foo"),
            [(0, 6, 9, W), (0, 12, 15, R), (0, 24, 27, R)]
        );
    }

    #[test]
    fn assignment_to_field_access_lhs_is_read() {
        // Only *simple-name* targets are writes: `o.f` keeps `f` a read even as the LHS, and
        // the RHS `f` is a non-target child of the assignment.
        let text = "class C { int f; void m(C o, int f) { o.f = f; } }";
        assert_eq!(
            at(text, "f"),
            [
                (0, 14, 15, W),
                (0, 33, 34, W),
                (0, 40, 41, R),
                (0, 44, 45, R)
            ]
        );
    }

    #[test]
    fn for_each_binding_is_write() {
        let text = "class C { void m(int[] xs) { for (int item : xs) use(item); } }";
        assert_eq!(at(text, "item"), [(0, 38, 42, W), (0, 53, 57, R)]);
    }

    #[test]
    fn never_panics_on_broken_or_out_of_range() {
        for text in ["", "class", "class C {", "/* unterminated", "@", "a ="] {
            let idx = LineIndex::new(text);
            for (line, character) in [(0, 0), (999, 999), (0, 999)] {
                document_highlight(text, &idx, Position { line, character });
            }
        }
    }
}
