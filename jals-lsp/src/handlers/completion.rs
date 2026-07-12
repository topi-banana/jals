//! Completion (`textDocument/completion`).
//!
//! Two contexts, dispatched on [`jals_hir::ProjectIndex::at_member_access`]:
//!
//! - **Member access** (`receiver.` / a partial `receiver.fo`): the fields and methods reachable on
//!   the receiver's type ([`jals_hir::ProjectIndex::member_completions`]). No keywords.
//! - **Bare identifier** (anywhere else): the in-scope bindings and project type names
//!   ([`jals_hir::ProjectIndex::scope_completions`]), plus the Java keywords (added here, the only non-semantic
//!   part).
//!
//! Both semantic halves need the project member model. The cross-file path lives on the workspace
//! ([`Workspace::completions`](crate::state::Workspace::completions)), which holds the index; this
//! module holds the shared dispatch ([`Completions::completions`]), the **file-local** fallback
//! ([`Completions::completions_local`], which builds a single-file index so the document's own types
//! complete), and [`Completions::completions_to_lsp`] — the mapping from `jals-hir`'s pure shape to
//! the LSP payload.

use async_lsp::lsp_types::{CompletionItem, CompletionItemKind, Position};
use jals_hir::{Completion, DefKind, FileId, ProjectIndex, Resolved};
use jals_syntax::{Parse, SyntaxNode};

use crate::line_index::LineIndex;

/// The Java reserved words, literals, and restricted keywords offered at a bare identifier position.
/// A flat list — the editor filters by the typed prefix; positions are not gated.
const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
    "true",
    "false",
    "null",
    "var",
    "yield",
    "record",
    "sealed",
    "permits",
];

/// Completion (`textDocument/completion`).
pub(crate) struct Completions;

impl Completions {
    /// Maps `jals-hir`'s pure completions to LSP `CompletionItem`s: the name is the label, its kind
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

    /// The LSP completion-item kind for a completion's [`DefKind`] — the icon the editor shows.
    const fn item_kind(kind: DefKind) -> CompletionItemKind {
        use DefKind::{
            AnnotationType, CatchParam, Class, Constructor, Enum, EnumConstant, Field, Interface,
            LambdaParam, Local, Method, Param, PatternVar, Record, Resource, TypeParam,
        };
        match kind {
            Method | Constructor => CompletionItemKind::METHOD,
            Field => CompletionItemKind::FIELD,
            EnumConstant => CompletionItemKind::ENUM_MEMBER,
            Local | Param | LambdaParam | CatchParam | Resource | PatternVar => {
                CompletionItemKind::VARIABLE
            }
            TypeParam => CompletionItemKind::TYPE_PARAMETER,
            Class | Record => CompletionItemKind::CLASS,
            Interface | AnnotationType => CompletionItemKind::INTERFACE,
            Enum => CompletionItemKind::ENUM,
        }
    }

    /// The completions at byte `offset`, dispatched on context: members after a `.`, otherwise the
    /// in-scope bindings and project types plus the Java keywords. The shared core of the workspace and
    /// file-local entry points.
    pub(crate) fn completions(
        root: &SyntaxNode,
        resolved: &Resolved,
        index: &ProjectIndex,
        file: FileId,
        offset: usize,
    ) -> Vec<CompletionItem> {
        if jals_hir::ProjectIndex::at_member_access(root, offset) {
            Self::completions_to_lsp(index.member_completions(root, resolved, file, offset))
        } else {
            let mut items =
                Self::completions_to_lsp(index.scope_completions(root, resolved, file, offset));
            items.extend(Self::keyword_items());
            items
        }
    }

    /// The Java keywords as LSP completion items.
    fn keyword_items() -> impl Iterator<Item = CompletionItem> {
        JAVA_KEYWORDS.iter().map(|keyword| CompletionItem {
            label: (*keyword).to_owned(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..CompletionItem::default()
        })
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
        let root = parse.syntax();
        let offset = u32::from(line_index.offset(text, position)) as usize;
        let index = ProjectIndex::builder(&[(FileId(0), root.clone())])
            .with_stdlib()
            .build();
        let resolved = jals_hir::Resolved::resolve_node(&root);
        Self::completions(&root, &resolved, &index, FileId(0), offset)
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
