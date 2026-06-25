//! Completion (`textDocument/completion`).
//!
//! Member-access completion: with the cursor after `receiver.` (or a partially-typed `receiver.fo`),
//! offer the fields and methods reachable on the receiver's type. The semantic work —
//! anchoring on the dot, inferring the receiver, enumerating members — is
//! [`jals_hir::member_completions`], which needs the project member model. The cross-file path lives
//! on the workspace ([`Workspace::completions`](crate::state::Workspace::completions)), which holds
//! the index; this module holds the **file-local** fallback ([`completions_local`], which builds a
//! single-file index so the document's own types complete) and [`completions_to_lsp`] — the mapping
//! from `jals-hir`'s pure shape to the LSP payload that both paths share.

use async_lsp::lsp_types::{CompletionItem, CompletionItemKind, Position};
use jals_hir::{Completion, DefKind, FileId, ProjectIndex};
use jals_syntax::Parse;

use crate::line_index::LineIndex;

/// Maps `jals-hir`'s pure member completions to LSP `CompletionItem`s: the member name is the label,
/// its kind drives the item icon, and its rendered type / signature is the detail shown beside it.
pub(crate) fn completions_to_lsp(completions: Vec<Completion>) -> Vec<CompletionItem> {
    completions
        .into_iter()
        .map(|completion| CompletionItem {
            label: completion.label,
            kind: Some(item_kind(completion.kind)),
            detail: Some(completion.detail),
            ..CompletionItem::default()
        })
        .collect()
}

/// The LSP completion-item kind for a member's [`DefKind`]. Member completion only yields fields and
/// methods; any other kind (it should not occur) falls back to a plain field icon.
fn item_kind(kind: DefKind) -> CompletionItemKind {
    match kind {
        DefKind::Method => CompletionItemKind::METHOD,
        _ => CompletionItemKind::FIELD,
    }
}

/// The member completions at `position`, computed over this one file by building a single-file
/// project index (so the document's own types' members are visible). The fallback for a file outside
/// any indexed project; the cross-file path is
/// [`Workspace::completions`](crate::state::Workspace::completions).
pub(crate) fn completions_local(
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
    position: Position,
) -> Vec<CompletionItem> {
    let root = parse.syntax();
    let offset = u32::from(line_index.offset(text, position)) as usize;
    let index = ProjectIndex::build(&[(FileId(0), root.clone())]);
    let resolved = jals_hir::resolve_node(&root);
    let completions = jals_hir::member_completions(&root, &resolved, &index, FileId(0), offset);
    completions_to_lsp(completions)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Completion labels (with kinds) at the cursor placed just after the first `cursor` substring.
    fn complete_at(text: &str, cursor: &str) -> Vec<(String, CompletionItemKind)> {
        let line_index = LineIndex::new(text);
        let offset = text.find(cursor).expect("cursor substring") + cursor.len();
        let pos = line_index.position(text, text_size::TextSize::from(offset as u32));
        let parse = jals_syntax::parse(text);
        let mut items: Vec<(String, CompletionItemKind)> =
            completions_local(&parse, text, &line_index, pos)
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
                ("area".to_string(), CompletionItemKind::METHOD),
                ("m".to_string(), CompletionItemKind::METHOD),
                ("size".to_string(), CompletionItemKind::FIELD),
            ]
        );
    }

    #[test]
    fn no_members_for_an_external_receiver() {
        let text = "class C { void m(String s) { s. } }";
        assert!(complete_at(text, "s.").is_empty());
    }
}
