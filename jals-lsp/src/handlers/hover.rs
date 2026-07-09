//! Hover (`textDocument/hover`): show the inferred type of the expression under the cursor.
//!
//! Type inference resolves reference type names against the project, so the cross-file path lives on
//! the workspace ([`Workspace::hover`](crate::state::Workspace::hover)), which holds the index. This
//! module holds the **file-local** fallback ([`hover_local`]) for a document outside any indexed
//! project, plus [`type_hover`] — the shared formatting both paths use to render a [`Ty`] as a
//! Markdown hover.

use async_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};
use jals_hir::Ty;
use jals_syntax::Parse;

use crate::line_index::LineIndex;

/// Renders an inferred type as a hover, or `None` for [`Ty::Unknown`] (nothing useful to show).
pub(crate) fn type_hover(ty: &Ty) -> Option<Hover> {
    if matches!(ty, Ty::Unknown) {
        return None;
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("```java\n{ty}\n```"),
        }),
        range: None,
    })
}

/// The hover for the expression under `position`, inferred over this one file. Reference type names
/// can only resolve externally (by spelling) here — [`jals_hir::infer_node`] has no project index —
/// but structural inference (primitives, arrays, `var`, numeric promotion) is unaffected.
pub(crate) fn hover_local(
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
    position: Position,
) -> Option<Hover> {
    let root = parse.syntax();
    let resolved = jals_hir::resolve_node(&root);
    let inference = jals_hir::infer_node(&root, &resolved);
    let offset = u32::from(line_index.offset(text, position)) as usize;
    type_hover(inference.type_at(offset)?)
}

#[cfg(test)]
mod tests {
    use text_size::TextSize;

    use super::*;

    /// The hover text at the start of the first occurrence of `needle`, if any.
    fn at(text: &str, needle: &str) -> Option<String> {
        let line_index = LineIndex::new(text);
        let offset = text.find(needle).expect("needle not found");
        let pos = line_index.position(text, TextSize::from(offset as u32));
        let parse = jals_syntax::parse(text);
        hover_local(&parse, text, &line_index, pos).map(|h| match h.contents {
            HoverContents::Markup(m) => m.value,
            _ => panic!("expected markup hover"),
        })
    }

    #[test]
    fn hovers_a_literal() {
        assert_eq!(
            at("class C { void m() { var x = 1; } }", "1"),
            Some("```java\nint\n```".to_string())
        );
    }

    #[test]
    fn hovers_a_var_local_from_its_initializer() {
        // The `s` use shows the type inferred for the `var` binding.
        let text = "class C { void m() { var s = \"hi\"; use(s); } }";
        let use_s = text.rfind('s').unwrap();
        let line_index = LineIndex::new(text);
        let pos = line_index.position(text, TextSize::from(use_s as u32));
        let parse = jals_syntax::parse(text);
        let hover = hover_local(&parse, text, &line_index, pos).expect("has a type");
        let HoverContents::Markup(m) = hover.contents else {
            panic!("expected markup");
        };
        assert_eq!(m.value, "```java\nString\n```");
    }

    #[test]
    fn no_hover_on_an_unknown_form() {
        // A method call has no inferred type yet (member resolution is a later phase).
        assert_eq!(at("class C { void m() { var r = f(); } }", "f()"), None);
    }

    #[test]
    fn never_panics_on_broken_or_out_of_range() {
        for text in ["", "class", "class C {", "@", "a ="] {
            let line_index = LineIndex::new(text);
            let parse = jals_syntax::parse(text);
            for (line, character) in [(0, 0), (999, 999), (0, 999)] {
                hover_local(&parse, text, &line_index, Position { line, character });
            }
        }
    }
}
