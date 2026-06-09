//! Builds an LSP document-symbol tree from the typed AST.

use async_lsp::lsp_types::{DocumentSymbol, SymbolKind};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{AstNode, ClassBody, Decl, EnumDecl, Member, SourceFile};

use crate::line_index::LineIndex;

/// Build the document-symbol tree for `text`.
pub(crate) fn document_symbols(text: &str, line_index: &LineIndex) -> Vec<DocumentSymbol> {
    let Some(file) = SourceFile::cast(jals_syntax::parse(text).syntax()) else {
        return Vec::new();
    };
    file.decls()
        .map(|decl| symbol_for_decl(&decl, text, line_index))
        .collect()
}

fn symbol_for_decl(decl: &Decl, text: &str, idx: &LineIndex) -> DocumentSymbol {
    match decl {
        Decl::Class(d) => type_symbol(d.syntax(), d.name(), SymbolKind::CLASS, d.body(), text, idx),
        Decl::Interface(d) => type_symbol(
            d.syntax(),
            d.name(),
            SymbolKind::INTERFACE,
            d.body(),
            text,
            idx,
        ),
        Decl::Record(d) => type_symbol(
            d.syntax(),
            d.name(),
            SymbolKind::STRUCT,
            d.body(),
            text,
            idx,
        ),
        Decl::AnnotationType(d) => type_symbol(
            d.syntax(),
            d.name(),
            SymbolKind::INTERFACE,
            d.body(),
            text,
            idx,
        ),
        Decl::Enum(d) => enum_symbol(d, text, idx),
    }
}

fn symbol_for_member(member: &Member, text: &str, idx: &LineIndex) -> Option<DocumentSymbol> {
    let sym = match member {
        Member::Field(d) => leaf(d.syntax(), d.name(), SymbolKind::FIELD, text, idx),
        Member::Method(d) => leaf(d.syntax(), d.name(), SymbolKind::METHOD, text, idx),
        Member::Constructor(d) => leaf(d.syntax(), d.name(), SymbolKind::CONSTRUCTOR, text, idx),
        // Unnamed static/instance initializer block: skip.
        Member::Initializer(_) => return None,
        Member::Class(d) => {
            type_symbol(d.syntax(), d.name(), SymbolKind::CLASS, d.body(), text, idx)
        }
        Member::Interface(d) => type_symbol(
            d.syntax(),
            d.name(),
            SymbolKind::INTERFACE,
            d.body(),
            text,
            idx,
        ),
        Member::Record(d) => type_symbol(
            d.syntax(),
            d.name(),
            SymbolKind::STRUCT,
            d.body(),
            text,
            idx,
        ),
        Member::AnnotationType(d) => type_symbol(
            d.syntax(),
            d.name(),
            SymbolKind::INTERFACE,
            d.body(),
            text,
            idx,
        ),
        Member::Enum(d) => enum_symbol(d, text, idx),
    };
    Some(sym)
}

/// A type-like symbol (class/interface/record/annotation) whose children are its members.
fn type_symbol(
    node: &SyntaxNode,
    name: Option<String>,
    kind: SymbolKind,
    body: Option<ClassBody>,
    text: &str,
    idx: &LineIndex,
) -> DocumentSymbol {
    let children = body.map(|b| {
        b.members()
            .filter_map(|m| symbol_for_member(&m, text, idx))
            .collect()
    });
    make(node, name, kind, children, text, idx)
}

/// An enum symbol, whose children are its constants followed by its members.
fn enum_symbol(d: &EnumDecl, text: &str, idx: &LineIndex) -> DocumentSymbol {
    let children = d.body().map(|b| {
        let constants = b
            .constants()
            .map(|c| leaf(c.syntax(), c.name(), SymbolKind::ENUM_MEMBER, text, idx));
        let members = b.members().filter_map(|m| symbol_for_member(&m, text, idx));
        constants.chain(members).collect()
    });
    make(d.syntax(), d.name(), SymbolKind::ENUM, children, text, idx)
}

/// A symbol with no children.
fn leaf(
    node: &SyntaxNode,
    name: Option<String>,
    kind: SymbolKind,
    text: &str,
    idx: &LineIndex,
) -> DocumentSymbol {
    make(node, name, kind, None, text, idx)
}

fn make(
    node: &SyntaxNode,
    name: Option<String>,
    kind: SymbolKind,
    children: Option<Vec<DocumentSymbol>>,
    text: &str,
    idx: &LineIndex,
) -> DocumentSymbol {
    let range = idx.range(text, node.text_range());
    #[allow(deprecated)]
    DocumentSymbol {
        name: name.unwrap_or_else(|| "<anonymous>".to_string()),
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        // The AST has no separate name-token accessor, so the whole node is the selection
        // range. This still satisfies LSP's "contained by range" requirement.
        selection_range: range,
        children: children.filter(|c| !c.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols(text: &str) -> Vec<DocumentSymbol> {
        document_symbols(text, &LineIndex::new(text))
    }

    #[test]
    fn class_with_members() {
        let syms = symbols("class C { int x; void m() {} class Inner {} }");
        assert_eq!(syms.len(), 1);
        let c = &syms[0];
        assert_eq!(c.name, "C");
        assert_eq!(c.kind, SymbolKind::CLASS);
        let children = c.children.as_ref().unwrap();
        assert_eq!(children.len(), 3);
        assert_eq!(
            (children[0].name.as_str(), children[0].kind),
            ("x", SymbolKind::FIELD)
        );
        assert_eq!(
            (children[1].name.as_str(), children[1].kind),
            ("m", SymbolKind::METHOD)
        );
        assert_eq!(
            (children[2].name.as_str(), children[2].kind),
            ("Inner", SymbolKind::CLASS)
        );
    }

    #[test]
    fn enum_constants_then_methods() {
        let syms = symbols("enum E { A, B; void m() {} }");
        let e = &syms[0];
        assert_eq!(e.kind, SymbolKind::ENUM);
        let children = e.children.as_ref().unwrap();
        assert_eq!(children.len(), 3);
        assert_eq!(
            (children[0].name.as_str(), children[0].kind),
            ("A", SymbolKind::ENUM_MEMBER)
        );
        assert_eq!(
            (children[1].name.as_str(), children[1].kind),
            ("B", SymbolKind::ENUM_MEMBER)
        );
        assert_eq!(
            (children[2].name.as_str(), children[2].kind),
            ("m", SymbolKind::METHOD)
        );
    }

    #[test]
    fn incomplete_decls_do_not_panic() {
        // Missing names / bodies must not panic — accessors return None and we fall back.
        let _ = symbols("class {}");
        let _ = symbols("class");
        let _ = symbols("");
    }
}
