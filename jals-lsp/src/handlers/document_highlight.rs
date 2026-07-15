//! Occurrence highlighting (`textDocument/documentHighlight`).
//!
//! Semantic first: with the cursor on an identifier that resolves to a file-local binding (a
//! local, parameter, field, method, type parameter, or sibling type), every occurrence of *that
//! binding* — its declaration and each reference to it — is highlighted, and nothing else.
//! Shadowing and name-spaces are respected, so a local does not light up a same-named field, and a
//! member access (`obj.field`) of the same spelling is left alone.
//!
//! Cross-file types: when the identifier has no file-local binding but a project index is supplied
//! and the cursor is on a type name that resolves through it (an imported or same-package sibling
//! declared in another file), every type reference *in this file* that resolves to the same
//! declaration is highlighted precisely — so a same-spelled variable or member access is left alone,
//! which the lexical fallback could not manage.
//!
//! Lexical fallback: failing both — an external type (`String`), an inherited member, an undeclared
//! name, or any reference with no index to consult — name resolution has nothing to anchor to, so
//! every `IDENT` token with the same text is highlighted instead. That over-matches across unrelated
//! roles, but it keeps an external name highlightable; it is what the syntax layer alone can deliver.
//!
//! Each occurrence is classified from its syntactic context alone: declaration/binding names,
//! simple-name assignment targets (including compound operators like `+=`, which the LSP's
//! single-kind field forces to one side — Write, per rust-analyzer convention), and `++`/`--`
//! operands are [`DocumentHighlightKind::WRITE`]; every other occurrence is
//! [`DocumentHighlightKind::READ`]. Only plain `IDENT` tokens trigger: keywords (including
//! contextual ones like `var`, remapped at parse time), literals, trivia, and `_` yield no
//! highlights.

use async_lsp::lsp_types::{DocumentHighlight, DocumentHighlightKind, Position};
use jals_editor::{Highlight, HighlightKind, ProjectQueries, QueryFile};
use jals_hir::{FileId, ProjectIndex, Resolved};
use jals_syntax::Parse;

use crate::line_index::LineIndex;

/// Occurrence highlighting (`textDocument/documentHighlight`).
pub(crate) struct DocumentHighlights;

impl DocumentHighlights {
    /// All occurrences of the symbol under `position`, in document order; empty if the cursor is not
    /// on an identifier. With `project = Some((index, file))`, a type name with no file-local binding is
    /// resolved cross-file so its references in this file highlight precisely; `None` keeps the
    /// file-local behavior (a lexical fallback for such a name).
    pub(crate) fn document_highlight(
        parse: &Parse,
        text: &str,
        line_index: &LineIndex,
        position: Position,
        project: Option<(&ProjectIndex, FileId)>,
    ) -> Vec<DocumentHighlight> {
        let offset = u32::from(line_index.offset(text, position)) as usize;
        let highlights = if let Some((index, file)) = project {
            let root = parse.syntax();
            let resolved = Resolved::resolve_node(&root);
            ProjectQueries::new(index, QueryFile::new(file, root, &resolved)).highlights(offset)
        } else {
            super::OneFileQueries::new(parse)
                .queries()
                .highlights(offset)
        };
        Self::highlights_to_lsp(highlights, text, line_index)
    }

    pub(crate) fn highlights_to_lsp(
        highlights: Vec<Highlight>,
        text: &str,
        line_index: &LineIndex,
    ) -> Vec<DocumentHighlight> {
        highlights
            .into_iter()
            .map(|highlight| DocumentHighlight {
                range: line_index.byte_range(text, &highlight.range),
                kind: Some(match highlight.kind {
                    HighlightKind::Read => DocumentHighlightKind::READ,
                    HighlightKind::Write => DocumentHighlightKind::WRITE,
                }),
            })
            .collect()
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
        DocumentHighlights::document_highlight(
            &jals_syntax::Parse::parse(text),
            text,
            &idx,
            Position { line, character },
            None,
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
            let parse = jals_syntax::Parse::parse(text);
            for (line, character) in [(0, 0), (999, 999), (0, 999)] {
                DocumentHighlights::document_highlight(
                    &parse,
                    text,
                    &idx,
                    Position { line, character },
                    None,
                );
            }
        }
    }

    #[test]
    fn cross_file_type_highlights_precisely_with_an_index() {
        use jals_hir::{FileId, ProjectIndex};

        // `Foo` is declared in another file and used twice here; a local variable is also named
        // `Foo` (an unusual but legal spelling clash). The index path highlights only the two type
        // references, never the variable — where the lexical fallback would catch all three.
        let other = "package a; class Foo { }";
        let main = "package a; class C { Foo a; Foo b; void m() { int Foo = 0; } }";
        let nodes = [
            (FileId(0), jals_syntax::Parse::parse(other).syntax()),
            (FileId(1), jals_syntax::Parse::parse(main).syntax()),
        ];
        let index = ProjectIndex::builder(&nodes).build();
        let idx = LineIndex::new(main);
        let parse = jals_syntax::Parse::parse(main);

        // Cursor on the first type use of `Foo`.
        let col = main.find("Foo").unwrap() as u32;
        let highlights = DocumentHighlights::document_highlight(
            &parse,
            main,
            &idx,
            Position::new(0, col),
            Some((&index, FileId(1))),
        );
        let cols: Vec<u32> = highlights.iter().map(|h| h.range.start.character).collect();
        // The two type references (`Foo a`, `Foo b`), not the `int Foo` local.
        assert_eq!(cols.len(), 2);
        let foo_a = main.find("Foo a").unwrap() as u32;
        let foo_b = main.find("Foo b").unwrap() as u32;
        assert_eq!(cols, [foo_a, foo_b]);

        // The file-local path (no index) over-matches: all three `Foo` tokens.
        let lexical =
            DocumentHighlights::document_highlight(&parse, main, &idx, Position::new(0, col), None);
        assert_eq!(lexical.len(), 3);
    }
}
