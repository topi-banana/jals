//! Completion (`textDocument/completion`).
//!
//! [`jals_editor::ProjectQueries`] owns the member/bare dispatch, semantic candidates, and Java
//! keywords. This adapter maps its protocol-neutral completion categories to LSP kinds. Documents
//! outside a workspace use the same query interface over a stdlib-aware one-file project.

use async_lsp::lsp_types::{CompletionItem, CompletionItemKind, Position};
use jals_editor::{Completion, CompletionKind};
use jals_syntax::Parse;

use crate::line_index::LineIndex;

/// Completion (`textDocument/completion`).
pub(crate) struct Completions;

impl Completions {
    /// Maps shared completions to LSP `CompletionItem`s: the name is the label, its kind
    /// drives the item icon, and its rendered type / signature (when any) is the detail shown beside it.
    pub(crate) fn completions_to_lsp(completions: Vec<Completion>) -> Vec<CompletionItem> {
        completions
            .into_iter()
            .map(|completion| CompletionItem {
                label: completion.label,
                kind: Some(Self::item_kind(completion.kind)),
                detail: (!completion.detail.is_empty()).then_some(completion.detail),
                ..CompletionItem::default()
            })
            .collect()
    }

    /// The LSP completion-item kind for a protocol-neutral completion category.
    const fn item_kind(kind: CompletionKind) -> CompletionItemKind {
        use CompletionKind::{
            Class, Enum, EnumMember, Field, Interface, Keyword, Method, TypeParameter, Variable,
        };
        match kind {
            Method => CompletionItemKind::METHOD,
            Field => CompletionItemKind::FIELD,
            EnumMember => CompletionItemKind::ENUM_MEMBER,
            Variable => CompletionItemKind::VARIABLE,
            TypeParameter => CompletionItemKind::TYPE_PARAMETER,
            Class => CompletionItemKind::CLASS,
            Interface => CompletionItemKind::INTERFACE,
            Enum => CompletionItemKind::ENUM,
            Keyword => CompletionItemKind::KEYWORD,
        }
    }

    /// The completions at `position`, computed over this one file by building a single-file project
    /// index with the `java.lang` stubs folded in (so the document's own types and the core JDK types
    /// are visible). The fallback for a file outside any indexed project; the cross-file path is
    /// [`Workspace::completions`](crate::state::Workspace::completions).
    pub(crate) fn completions_local(
        parse: &Parse,
        text: &str,
        line_index: &LineIndex,
        position: Position,
    ) -> Vec<CompletionItem> {
        let offset = u32::from(line_index.offset(text, position)) as usize;
        let project = super::OneFileQueries::new(parse);
        Self::completions_to_lsp(project.queries().completions(offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Completion labels (with kinds) at the cursor placed just after the first `cursor` substring.
    fn complete_at(text: &str, cursor: &str) -> Vec<(String, CompletionItemKind)> {
        let line_index = LineIndex::new(text);
        let offset = text.find(cursor).expect("cursor substring") + cursor.len();
        let pos = line_index.position(text, text_size::TextSize::from(offset as u32));
        let parse = jals_syntax::Parse::parse(text);
        let mut items: Vec<(String, CompletionItemKind)> =
            Completions::completions_local(&parse, text, &line_index, pos)
                .into_iter()
                .map(|i| (i.label, i.kind.expect("kind set")))
                .collect();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        items
    }

    #[test]
    fn completes_own_members_with_kinds() {
        let text = "class Box { int size; int area() { return 0; } void m(Box b) { b. } }";
        assert_eq!(
            complete_at(text, "b."),
            [
                ("area".to_owned(), CompletionItemKind::METHOD),
                ("m".to_owned(), CompletionItemKind::METHOD),
                ("size".to_owned(), CompletionItemKind::FIELD),
            ]
        );
    }

    #[test]
    fn no_members_for_an_external_receiver() {
        // `Widget` is neither declared here nor a `java.lang` stub, so its type is external — we
        // know no members for it and offer none. (A stubbed receiver like `String` does offer them.)
        let text = "class C { void m(Widget s) { s. } }";
        assert!(complete_at(text, "s.").is_empty());
    }

    #[test]
    fn members_of_a_stubbed_jdk_receiver() {
        // The `java.lang` stubs are folded into the single-file index, so a `String` receiver
        // offers its stubbed methods — the payoff of building with stdlib.
        let text = "class C { void m(String s) { s. } }";
        let labels: Vec<String> = complete_at(text, "s.")
            .into_iter()
            .map(|(l, _)| l)
            .collect();
        assert!(labels.contains(&"length".to_owned()), "got {labels:?}");
        assert!(labels.contains(&"charAt".to_owned()), "got {labels:?}");
    }

    #[test]
    fn scope_offers_locals_and_keywords() {
        // A bare position (after `= `): the local `x` (a variable) and the Java keywords are offered,
        // but no member icons. `return` is a keyword, `x` a variable.
        let text = "class C { void m() { int x = 1; int y = } }";
        let items = complete_at(text, "int y = ");
        assert!(items.contains(&("x".to_owned(), CompletionItemKind::VARIABLE)));
        assert!(items.contains(&("return".to_owned(), CompletionItemKind::KEYWORD)));
        assert!(items.contains(&("class".to_owned(), CompletionItemKind::KEYWORD)));
    }

    #[test]
    fn no_keywords_after_a_dot() {
        // A member-access context offers no keywords (so `class`, `return`, etc. never appear).
        let text = "class Box { int size; void m(Box b) { b. } }";
        let items = complete_at(text, "b.");
        assert!(
            items
                .iter()
                .all(|(_, kind)| *kind != CompletionItemKind::KEYWORD)
        );
        assert!(items.iter().any(|(label, _)| label == "size"));
    }
}
