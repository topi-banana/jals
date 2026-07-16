//! Protocol-neutral document outline (document symbols) over the typed AST.
//!
//! The full declaration/member case table lives here once; the LSP and Monaco hosts map each
//! [`OutlineNode`] to their protocol's symbol shape (kind vocabulary and coordinates) only.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_hir::DefKind;
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{AstNode, ClassBody, Decl, EnumDecl, Member, SourceFile};

/// One node of the document outline: a declaration's name, kind, byte ranges, and nested members.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineNode {
    /// The declared name, or `"<anonymous>"` when error recovery left the declaration nameless.
    pub name: String,
    /// The declaration's kind in HIR vocabulary — hosts map it to their protocol's symbol kind.
    pub kind: DefKind,
    /// The whole declaration's byte range.
    pub range: Range<usize>,
    /// The byte range a client selects when revealing the symbol. The typed AST has no separate
    /// name-token accessor, so this is the whole node — still contained by `range`.
    pub selection_range: Range<usize>,
    /// Nested declarations. An enum's constants come before its members; an unnamed
    /// static/instance initializer block contributes nothing.
    pub children: Vec<Self>,
}

/// Builds the document outline from a parsed file's syntax tree.
pub struct Outline;

impl Outline {
    /// The outline of `root`'s top-level declarations (empty for a non-source-file root).
    pub fn of(root: &SyntaxNode) -> Vec<OutlineNode> {
        let Some(file) = SourceFile::cast(root.clone()) else {
            return Vec::new();
        };
        file.decls().map(|decl| Self::for_decl(&decl)).collect()
    }

    /// The outline node for a top-level declaration.
    fn for_decl(decl: &Decl) -> OutlineNode {
        match decl {
            Decl::Class(d) => Self::for_type(d.syntax(), d.name(), DefKind::Class, d.body()),
            Decl::Interface(d) => {
                Self::for_type(d.syntax(), d.name(), DefKind::Interface, d.body())
            }
            Decl::Record(d) => Self::for_type(d.syntax(), d.name(), DefKind::Record, d.body()),
            Decl::AnnotationType(d) => {
                Self::for_type(d.syntax(), d.name(), DefKind::AnnotationType, d.body())
            }
            Decl::Enum(d) => Self::for_enum(d),
            // Top-level field / method of a compact source file (JEP 512).
            Decl::Field(d) => Self::leaf(d.syntax(), d.name(), DefKind::Field),
            Decl::Method(d) => Self::leaf(d.syntax(), d.name(), DefKind::Method),
        }
    }

    /// The outline node for a type member, or `None` for an unnamed initializer block.
    fn for_member(member: &Member) -> Option<OutlineNode> {
        let node = match member {
            Member::Field(d) => Self::leaf(d.syntax(), d.name(), DefKind::Field),
            Member::Method(d) => Self::leaf(d.syntax(), d.name(), DefKind::Method),
            Member::Constructor(d) => Self::leaf(d.syntax(), d.name(), DefKind::Constructor),
            // Unnamed static/instance initializer block: skip.
            Member::Initializer(_) => return None,
            Member::Class(d) => Self::for_type(d.syntax(), d.name(), DefKind::Class, d.body()),
            Member::Interface(d) => {
                Self::for_type(d.syntax(), d.name(), DefKind::Interface, d.body())
            }
            Member::Record(d) => Self::for_type(d.syntax(), d.name(), DefKind::Record, d.body()),
            Member::AnnotationType(d) => {
                Self::for_type(d.syntax(), d.name(), DefKind::AnnotationType, d.body())
            }
            Member::Enum(d) => Self::for_enum(d),
        };
        Some(node)
    }

    /// A type-like node (class/interface/record/annotation) whose children are its members.
    fn for_type(
        node: &SyntaxNode,
        name: Option<String>,
        kind: DefKind,
        body: Option<ClassBody>,
    ) -> OutlineNode {
        let children = body
            .map(|b| b.members().filter_map(|m| Self::for_member(&m)).collect())
            .unwrap_or_default();
        Self::node(node, name, kind, children)
    }

    /// An enum node, whose children are its constants followed by its members.
    fn for_enum(d: &EnumDecl) -> OutlineNode {
        let children = d
            .body()
            .map(|b| {
                let constants = b
                    .constants()
                    .map(|c| Self::leaf(c.syntax(), c.name(), DefKind::EnumConstant));
                let members = b.members().filter_map(|m| Self::for_member(&m));
                constants.chain(members).collect()
            })
            .unwrap_or_default();
        Self::node(d.syntax(), d.name(), DefKind::Enum, children)
    }

    /// A node with no children.
    fn leaf(node: &SyntaxNode, name: Option<String>, kind: DefKind) -> OutlineNode {
        Self::node(node, name, kind, Vec::new())
    }

    /// Assemble an [`OutlineNode`] over `node`'s byte range.
    fn node(
        node: &SyntaxNode,
        name: Option<String>,
        kind: DefKind,
        children: Vec<OutlineNode>,
    ) -> OutlineNode {
        let range = node.text_range();
        let range = usize::from(range.start())..usize::from(range.end());
        OutlineNode {
            name: name.unwrap_or_else(|| "<anonymous>".to_owned()),
            kind,
            range: range.clone(),
            selection_range: range,
            children,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(text: &str) -> Vec<OutlineNode> {
        Outline::of(&jals_syntax::Parse::parse(text).syntax())
    }

    /// `(name, kind)` pairs of a level, for compact assertions.
    fn names(nodes: &[OutlineNode]) -> Vec<(&str, DefKind)> {
        nodes.iter().map(|n| (n.name.as_str(), n.kind)).collect()
    }

    #[test]
    fn class_with_members() {
        let nodes = outline("class C { int x; void m() {} class Inner {} }");
        assert_eq!(names(&nodes), [("C", DefKind::Class)]);
        assert_eq!(
            names(&nodes[0].children),
            [
                ("x", DefKind::Field),
                ("m", DefKind::Method),
                ("Inner", DefKind::Class),
            ]
        );
    }

    #[test]
    fn every_type_flavor_maps_to_its_kind() {
        let nodes =
            outline("class C {}\ninterface I {}\nrecord R() {}\n@interface A {}\nenum E { X }\n");
        assert_eq!(
            names(&nodes),
            [
                ("C", DefKind::Class),
                ("I", DefKind::Interface),
                ("R", DefKind::Record),
                ("A", DefKind::AnnotationType),
                ("E", DefKind::Enum),
            ]
        );
    }

    #[test]
    fn constructor_and_initializer_members() {
        let nodes = outline("class C { C() {} static { } int x; }");
        assert_eq!(
            names(&nodes[0].children),
            [("C", DefKind::Constructor), ("x", DefKind::Field)],
            "the unnamed initializer block is skipped"
        );
    }

    #[test]
    fn enum_constants_then_methods() {
        let nodes = outline("enum E { A, B; void m() {} }");
        assert_eq!(names(&nodes), [("E", DefKind::Enum)]);
        assert_eq!(
            names(&nodes[0].children),
            [
                ("A", DefKind::EnumConstant),
                ("B", DefKind::EnumConstant),
                ("m", DefKind::Method),
            ]
        );
    }

    #[test]
    fn top_level_members_in_compact_source_file() {
        // JEP 512: top-level field / method declarations become outline nodes directly.
        let nodes = outline("int count = 0;\nvoid main() {}\n");
        assert_eq!(
            names(&nodes),
            [("count", DefKind::Field), ("main", DefKind::Method)]
        );
    }

    #[test]
    fn ranges_cover_the_declaration_and_selection_is_contained() {
        let text = "class C { int x; }";
        let nodes = outline(text);
        assert_eq!(&text[nodes[0].range.clone()], "class C { int x; }");
        // A member node's range may include its leading trivia (the CST keeps trivia inside).
        let field = &nodes[0].children[0];
        assert_eq!(text[field.range.clone()].trim_start(), "int x;");
        assert!(field.range.start <= field.selection_range.start);
        assert!(field.selection_range.end <= field.range.end);
    }

    #[test]
    fn incomplete_decls_do_not_panic() {
        // Missing names / bodies must not panic — accessors return None and we fall back.
        let anon = outline("class {}");
        assert!(anon.iter().all(|n| n.name == "<anonymous>"));
        let _ = outline("class");
        let _ = outline("");
    }
}
