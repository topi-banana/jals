//! The classpath bridge end-to-end: a `.class` file folded into the index resolves its members and
//! substitutes its generics, exactly like a source type.

use std::path::PathBuf;

use jals_classfile::ClassFile;
use jals_hir::{FileId, ProjectIndex, infer, resolve_node};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{self, AstNode};

/// `Box<T>` (generic, with `T get()` / `void set(T)`), compiled from `tests/fixtures/Box.java`.
fn box_classfile() -> ClassFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Box.class");
    ClassFile::read(&std::fs::read(path).expect("read Box.class")).expect("parse Box.class")
}

fn parse(src: &str) -> SyntaxNode {
    jals_syntax::parse(src).syntax()
}

/// The inferred type of the expression whose source text is exactly `text`, with `classfiles` folded
/// into the index as classpath types.
fn expr_ty(src: &str, text: &str, classfiles: &[ClassFile]) -> String {
    let node = parse(src);
    let resolved = resolve_node(&node);
    let index = ProjectIndex::build_with_classpath(&[(FileId(0), node.clone())], classfiles);
    let ti = infer(&node, &resolved, &index, FileId(0));
    let expr = node
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|e| e.syntax().text().to_string().trim() == text)
        .unwrap_or_else(|| panic!("no expression `{text}`"));
    let r = expr.syntax().text_range();
    ti.type_of_expr(usize::from(r.start())..usize::from(r.end()))
        .map(ToString::to_string)
        .unwrap_or_else(|| "<none>".to_owned())
}

const SRC: &str = "class Test { void m(Box<String> b) { var x = b.get(); } }";

#[test]
fn classpath_generic_member_is_substituted() {
    // `Box<String>.get()` returns `T` ↦ `String` through a loaded classpath type.
    assert_eq!(
        expr_ty(SRC, "b.get()", std::slice::from_ref(&box_classfile())),
        "String"
    );
}

#[test]
fn without_the_classfile_the_member_is_unknown() {
    // Same source, but `Box` is not on the classpath: it stays external, so the member type is not
    // known (and certainly not `String`). This is what the bridge improves on.
    assert_ne!(expr_ty(SRC, "b.get()", &[]), "String");
}

#[test]
fn classpath_type_is_not_a_navigation_target() {
    // A classpath type has no host-openable source, so go-to-definition is suppressed (like a stub).
    let src = "class Test { Box<String> field; }";
    let node = parse(src);
    let resolved = resolve_node(&node);
    let index = ProjectIndex::build_with_classpath(
        &[(FileId(0), node.clone())],
        std::slice::from_ref(&box_classfile()),
    );
    let offset = src.find("Box").expect("Box in source");
    assert!(
        index.definition_at(FileId(0), &resolved, offset).is_none(),
        "go-to-def into a classpath type should be suppressed"
    );
}
