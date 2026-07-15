//! Rename (`textDocument/rename` + `textDocument/prepareRename`).
//!
//! File-local: resolve the binding under the cursor and rewrite its declaration and every
//! reference to it within the open document. A file in an indexed project renames project types
//! across the whole project instead (see [`Workspace::rename`](crate::state::Workspace::rename));
//! this is the fallback for a document that belongs to no project.
//!
//! Renaming is deliberately conservative — it only offers symbols it can rewrite *soundly*:
//!
//! - Locals, parameters, type parameters, and pattern-bound names never escape their file, so
//!   rewriting the open document covers every use.
//! - Members (a field, method, constructor, or enum constant) are withheld: their cross-file
//!   references are not indexed yet, so a rename could silently break uses in other files. They
//!   become renamable once a cross-file member reference index exists.
//!
//! The new name is validated against the lexer ([`Rename::is_valid_identifier`]): it must tokenize as a
//! single `IDENT`, so a reserved word (`int`, `true`) or a malformed name is rejected before any
//! edit is produced.

use std::collections::HashMap;

use async_lsp::lsp_types::{Position, Range, TextEdit, Url, WorkspaceEdit};
use jals_hir::{DefId, DefKind, Resolved};
use jals_syntax::{Parse, SyntaxKind, SyntaxToken};

use crate::line_index::LineIndex;

/// Rename (`textDocument/rename` + `textDocument/prepareRename`): the file-local pass.
pub(crate) struct Rename;

impl Rename {
    /// Whether `name` is a single legal Java identifier: it tokenizes to exactly one `IDENT` token
    /// spanning the whole string. A reserved word lexes to its keyword kind (`int` → `INT_KW`), and
    /// anything with whitespace, punctuation, or a leading digit yields a non-`IDENT` token or more
    /// than one token — all rejected. (A context-sensitive keyword such as `var` lexes as `IDENT` and
    /// is accepted; its use is position-restricted, which a rename does not police.)
    pub(crate) fn is_valid_identifier(name: &str) -> bool {
        let mut tokens = jals_syntax::Lexer::tokenize(name).into_iter();
        matches!(
            (tokens.next(), tokens.next()),
            (Some(token), None) if token.kind == SyntaxKind::IDENT && token.text == name
        )
    }

    /// Whether a binding of this kind may be renamed from a single file's resolution alone. Locals and
    /// other file-scoped bindings always qualify; project types do too (the workspace widens their
    /// rewrite project-wide). Members are withheld — their uses can span files we do not rewrite here.
    pub(crate) const fn is_renamable_kind(kind: DefKind) -> bool {
        use jals_hir::DefKind::{
            AnnotationType, CatchParam, Class, Enum, Interface, LambdaParam, Local, Param,
            PatternVar, Record, Resource, TypeParam,
        };
        matches!(
            kind,
            Local
                | Param
                | LambdaParam
                | TypeParam
                | CatchParam
                | Resource
                | PatternVar
                | Class
                | Interface
                | Enum
                | Record
                | AnnotationType
        )
    }

    /// The renamable binding under `position`: its def id, the identifier token naming it, and the
    /// file's resolution. `None` when the cursor is on no renamable binding (an external name, a
    /// keyword/literal, or a withheld member). Shared by [`Self::prepare_rename_local`] and [`Self::rename_local`].
    fn renamable_binding_at(
        parse: &Parse,
        text: &str,
        line_index: &LineIndex,
        position: Position,
    ) -> Option<(DefId, SyntaxToken, Resolved)> {
        let root = parse.syntax();
        let ident = super::Cursor::ident_at(&root, line_index.offset(text, position))?;
        let resolved = jals_hir::Resolved::resolve_node(&root);
        let id = resolved.symbol_at(usize::from(ident.text_range().start()))?;
        Self::is_renamable_kind(resolved.def(id).kind).then_some((id, ident, resolved))
    }

    /// The range of the renamable identifier under `position`, or `None` when the cursor is on no
    /// renamable binding (an external name, a keyword/literal, or a withheld member). Drives
    /// `prepareRename`, which the editor uses to validate a rename before prompting for a new name.
    pub(crate) fn prepare_rename_local(
        parse: &Parse,
        text: &str,
        line_index: &LineIndex,
        position: Position,
    ) -> Option<Range> {
        let (_, ident, _) = Self::renamable_binding_at(parse, text, line_index, position)?;
        Some(line_index.range(text, ident.text_range()))
    }

    /// A [`WorkspaceEdit`] renaming the binding under `position` to `new_name` within this one file, or
    /// `None` if the cursor is on no renamable binding. The caller validates `new_name` first (see
    /// [`Self::is_valid_identifier`]).
    pub(crate) fn rename_local(
        parse: &Parse,
        text: &str,
        line_index: &LineIndex,
        uri: &Url,
        position: Position,
        new_name: &str,
    ) -> Option<WorkspaceEdit> {
        Self::renamable_binding_at(parse, text, line_index, position)?;
        let offset = u32::from(line_index.offset(text, position)) as usize;
        let project = super::OneFileQueries::new(parse);
        let edits: Vec<TextEdit> = project
            .queries()
            .references(offset, true, [project.file()])
            .into_iter()
            .map(|target| TextEdit {
                range: line_index.byte_range(text, &target.range),
                new_text: new_name.to_owned(),
            })
            .collect();
        if edits.is_empty() {
            return None;
        }
        let mut changes = HashMap::new();
        changes.insert(uri.clone(), edits);
        Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use text_size::TextSize;

    use super::*;

    #[test]
    fn valid_identifier_accepts_names_and_rejects_keywords_and_junk() {
        for ok in ["foo", "x1", "_x", "camelCase", "var"] {
            assert!(
                Rename::is_valid_identifier(ok),
                "{ok} should be a valid identifier"
            );
        }
        for bad in [
            "int", "class", "true", "false", "null", "1a", "a b", "", "x.y", "a-b", "+",
        ] {
            assert!(
                !Rename::is_valid_identifier(bad),
                "{bad} should be rejected"
            );
        }
    }

    /// The `WorkspaceEdit` for renaming at the start of the first `needle`, as `(start, end,
    /// new_text)` tuples decoded from the single file's edits, in document order.
    fn rename_at(text: &str, needle: &str, new_name: &str) -> Option<Vec<(u32, u32, String)>> {
        let uri = Url::parse("file:///A.java").unwrap();
        let idx = LineIndex::new(text);
        let offset = text.find(needle).expect("needle not found");
        let pos = idx.position(text, TextSize::new(offset as u32));
        let parse = jals_syntax::Parse::parse(text);
        let edit = Rename::rename_local(&parse, text, &idx, &uri, pos, new_name)?;
        let mut edits: Vec<TextEdit> = edit.changes.unwrap().remove(&uri).unwrap();
        edits.sort_by_key(|e| (e.range.start.line, e.range.start.character));
        Some(
            edits
                .into_iter()
                .map(|e| (e.range.start.character, e.range.end.character, e.new_text))
                .collect(),
        )
    }

    #[test]
    fn renames_a_local_declaration_and_its_uses() {
        let text = "class C { void m() { int x = 1; x = x + 2; f(x); } }";
        let edits = rename_at(text, "x = 1", "y").expect("local x is renamable");
        assert_eq!(
            edits,
            [
                (25, 26, "y".to_owned()), // declaration
                (32, 33, "y".to_owned()), // assignment target
                (36, 37, "y".to_owned()), // rhs
                (45, 46, "y".to_owned()), // argument
            ]
        );
    }

    #[test]
    fn renames_from_a_use_too() {
        let text = "class C { void m() { int x = 1; f(x); } }";
        let edits = rename_at(text, "x);", "renamed").expect("renamable from a use");
        assert_eq!(
            edits,
            [
                (25, 26, "renamed".to_owned()),
                (34, 35, "renamed".to_owned()),
            ]
        );
    }

    #[test]
    fn member_and_external_are_not_renamable() {
        // A field is a member: its cross-file uses are not indexed here, so it is withheld.
        assert!(rename_at("class C { int f; }", "f;", "g").is_none());
        // An external type has no file-local binding to rewrite.
        assert!(rename_at("class C { String s; }", "String", "Str").is_none());
        // A keyword / literal is not an identifier and anchors nothing.
        assert!(rename_at("class C { void m() {} }", "void", "x").is_none());
    }

    #[test]
    fn prepare_reports_the_identifier_range_for_a_renamable_binding() {
        let text = "class C { void m() { int x = 1; f(x); } }";
        let idx = LineIndex::new(text);
        let parse = jals_syntax::Parse::parse(text);
        let pos = idx.position(text, TextSize::new(text.find("x = 1").unwrap() as u32));
        let range = Rename::prepare_rename_local(&parse, text, &idx, pos).expect("x is renamable");
        assert_eq!((range.start.character, range.end.character), (25, 26));

        // A member yields no prepare range.
        let pos = idx.position(text, TextSize::new(text.find("m()").unwrap() as u32));
        assert!(Rename::prepare_rename_local(&parse, text, &idx, pos).is_none());
    }

    #[test]
    fn never_panics_on_broken_or_out_of_range() {
        let uri = Url::parse("file:///A.java").unwrap();
        for text in ["", "class", "class C {", "@", "a ="] {
            let idx = LineIndex::new(text);
            let parse = jals_syntax::Parse::parse(text);
            for (line, character) in [(0, 0), (999, 999), (0, 999)] {
                let pos = Position { line, character };
                Rename::prepare_rename_local(&parse, text, &idx, pos);
                Rename::rename_local(&parse, text, &idx, &uri, pos, "x");
            }
        }
    }
}
