//! Go-to-definition (`textDocument/definition`).
//!
//! Cross-file go-to-definition is driven by the project symbol index and lives on the workspace
//! (it has every file's text to turn a target byte range into a `Location`). This module holds the
//! **file-local** fallback used when a document is not part of an indexed project: it resolves the
//! reference under the cursor to a binding in the same file.

use async_lsp::lsp_types::{Position, Range};
use jals_syntax::Parse;

use crate::line_index::LineIndex;

/// The LSP range of the definition the reference under `position` binds to within this one file, if
/// any. Resolution is the file-local pass ([`jals_hir::resolve_node`]); the cursor may sit on a
/// local, parameter, field, method call, or a file-local type name.
pub(crate) fn goto_definition_local(
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
    position: Position,
) -> Option<Range> {
    let resolved = jals_hir::resolve_node(&parse.syntax());
    let offset = u32::from(line_index.offset(text, position)) as usize;
    let def = resolved.definition_at(offset)?;
    Some(line_index.byte_range(text, &def.name_range))
}

#[cfg(test)]
mod tests {
    use text_size::TextSize;

    use super::*;

    /// Go-to-definition from the start of the first occurrence of `needle`, decoded to
    /// `(start_line, start_char, end_line, end_char)`.
    fn at(text: &str, needle: &str) -> Option<(u32, u32, u32, u32)> {
        let line_index = LineIndex::new(text);
        let offset = text.find(needle).expect("needle not found");
        let pos = line_index.position(text, TextSize::from(offset as u32));
        let parse = jals_syntax::parse(text);
        goto_definition_local(&parse, text, &line_index, pos)
            .map(|r| (r.start.line, r.start.character, r.end.line, r.end.character))
    }

    #[test]
    fn local_use_jumps_to_its_declaration() {
        // `class C { void m() { int x = 1; use(x); } }` — `x` in `use(x)` jumps to `int x`.
        let text = "class C { void m() { int x = 1; use(x); } }";
        // Cursor on the use of `x` (the second occurrence).
        let use_x = text.rfind('x').unwrap();
        let line_index = LineIndex::new(text);
        let pos = line_index.position(text, TextSize::from(use_x as u32));
        let parse = jals_syntax::parse(text);
        let range = goto_definition_local(&parse, text, &line_index, pos).expect("resolves");
        // The declaration `x` is at byte 25.
        assert_eq!((range.start.line, range.start.character), (0, 25));
    }

    #[test]
    fn file_local_type_name_jumps_to_its_class() {
        // `Helper` in the field type jumps to the sibling class declaration.
        let text = "class C { Helper h; } class Helper { }";
        assert_eq!(at(text, "Helper"), Some((0, 28, 0, 34)));
    }

    #[test]
    fn unresolved_cursor_yields_none() {
        let text = "class C { void m() { use(nope); } }";
        assert_eq!(at(text, "nope"), None);
    }

    #[test]
    fn never_panics_on_broken_or_out_of_range() {
        for text in ["", "class", "class C {", "@", "a ="] {
            let line_index = LineIndex::new(text);
            let parse = jals_syntax::parse(text);
            for (line, character) in [(0, 0), (999, 999), (0, 999)] {
                goto_definition_local(&parse, text, &line_index, Position { line, character });
            }
        }
    }
}
