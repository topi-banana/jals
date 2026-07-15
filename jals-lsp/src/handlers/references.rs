//! Find references (`textDocument/references`).
//!
//! [`jals_editor::ProjectQueries`] owns symbol anchoring, declaration policy, and project scanning.
//! This adapter maps its byte ranges to LSP locations; the fallback supplies a stdlib-aware one-file
//! project through the same interface.

use async_lsp::lsp_types::{Location, Position, Url};
use jals_syntax::Parse;

use crate::line_index::LineIndex;

/// Find references (`textDocument/references`): the file-local pass.
pub(crate) struct References;

impl References {
    /// Every reference to the symbol under `position`, within this one file, as `Location`s under
    /// `uri`. The declaration is included when `include_declaration` is set. Empty if the cursor is not
    /// on a resolvable identifier — an unresolved or external name has no file-local binding to gather.
    pub(crate) fn references(
        parse: &Parse,
        text: &str,
        line_index: &LineIndex,
        uri: &Url,
        position: Position,
        include_declaration: bool,
    ) -> Vec<Location> {
        let offset = u32::from(line_index.offset(text, position)) as usize;
        let project = super::OneFileQueries::new(parse);
        project
            .queries()
            .references(offset, include_declaration, [project.file()])
            .into_iter()
            .map(|target| Location {
                uri: uri.clone(),
                range: line_index.byte_range(text, &target.range),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use text_size::TextSize;

    use super::*;

    /// References from the cursor at the start of the first occurrence of `needle`, each decoded to
    /// `(line, start_char, end_char)`.
    fn at(text: &str, needle: &str, include_declaration: bool) -> Vec<(u32, u32, u32)> {
        let uri = Url::parse("file:///A.java").unwrap();
        let idx = LineIndex::new(text);
        let offset = text.find(needle).expect("needle not found in text");
        let pos = idx.position(text, TextSize::new(offset as u32));
        let parse = jals_syntax::Parse::parse(text);
        References::references(&parse, text, &idx, &uri, pos, include_declaration)
            .into_iter()
            .map(|l| {
                (
                    l.range.start.line,
                    l.range.start.character,
                    l.range.end.character,
                )
            })
            .collect()
    }

    #[test]
    fn gathers_uses_and_optionally_the_declaration() {
        let text = "class C { void m() { int x = 1; x = x + 2; f(x); } }";
        // Cursor on the declaration `x`. Without it: the three uses, in document order.
        assert_eq!(
            at(text, "x = 1", false),
            [(0, 32, 33), (0, 36, 37), (0, 45, 46)]
        );
        // With the declaration: the binding `int x` is included, in document order.
        assert_eq!(
            at(text, "x = 1", true),
            [(0, 25, 26), (0, 32, 33), (0, 36, 37), (0, 45, 46)]
        );
    }

    #[test]
    fn from_a_use_finds_the_same_binding() {
        // Cursor on the use `x` in `f(x)` resolves to the same local as its declaration.
        let text = "class C { void m() { int x = 1; f(x); } }";
        assert_eq!(at(text, "x);", true), [(0, 25, 26), (0, 34, 35)]);
    }

    #[test]
    fn returns_uri_of_the_document() {
        let text = "class C { void m() { int x = 1; f(x); } }";
        let uri = Url::parse("file:///A.java").unwrap();
        let idx = LineIndex::new(text);
        let parse = jals_syntax::Parse::parse(text);
        let pos = idx.position(text, TextSize::new(text.find("x = 1").unwrap() as u32));
        let locations = References::references(&parse, text, &idx, &uri, pos, true);
        assert!(!locations.is_empty());
        assert!(locations.iter().all(|l| l.uri == uri));
    }

    #[test]
    fn unresolved_name_yields_nothing_but_stdlib_uses_are_findable() {
        // An undeclared name has no binding to gather. The one-file fallback includes stdlib, so a
        // source-less stdlib type still returns its use (but no declaration target).
        let text = "class C { void m() { use(nope); } }";
        assert!(at(text, "nope", true).is_empty());
        assert_eq!(at("class C { String s; }", "String", true), [(0, 10, 16)]);
    }

    #[test]
    fn never_panics_on_broken_or_out_of_range() {
        let uri = Url::parse("file:///A.java").unwrap();
        for text in ["", "class", "class C {", "@", "a ="] {
            let idx = LineIndex::new(text);
            let parse = jals_syntax::Parse::parse(text);
            for (line, character) in [(0, 0), (999, 999), (0, 999)] {
                References::references(
                    &parse,
                    text,
                    &idx,
                    &uri,
                    Position { line, character },
                    true,
                );
            }
        }
    }
}
