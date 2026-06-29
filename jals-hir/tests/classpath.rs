//! The classpath bridge end-to-end: a `.class` file folded into the index resolves its members and
//! substitutes its generics, exactly like a source type.

use std::path::PathBuf;

use jals_classfile::ClassFile;
use jals_hir::{FileId, Namespace, ProjectIndex, SourceLocations, infer, resolve_node};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{self, AstNode};

/// `Box<T>` (generic, with `T get()` / `void set(T)`), compiled from `tests/fixtures/Box.java`.
fn box_classfile() -> ClassFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Box.class");
    ClassFile::read(&std::fs::read(path).expect("read Box.class")).expect("parse Box.class")
}

/// The `Box.java` source `Box.class` was compiled from — used as a library-sources overlay.
const BOX_SOURCE: &str = include_str!("fixtures/Box.java");

fn parse(src: &str) -> SyntaxNode {
    jals_syntax::parse(src).syntax()
}

/// The inferred type of the expression whose source text is exactly `text`, with `classfiles` folded
/// into the index as classpath types.
fn expr_ty(src: &str, text: &str, classfiles: &[ClassFile]) -> String {
    let node = parse(src);
    let resolved = resolve_node(&node);
    let index = ProjectIndex::builder(&[(FileId(0), node.clone())])
        .with_stdlib()
        .with_classpath(&ProjectIndex::lower_classpath(classfiles))
        .build();
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
    let index = ProjectIndex::builder(&[(FileId(0), node.clone())])
        .with_stdlib()
        .with_classpath(&ProjectIndex::lower_classpath(std::slice::from_ref(
            &box_classfile(),
        )))
        .build();
    let offset = src.find("Box").expect("Box in source");
    assert!(
        index.definition_at(FileId(0), &resolved, offset).is_none(),
        "go-to-def into a classpath type should be suppressed"
    );
}

#[test]
fn classpath_type_navigates_to_library_source() {
    // With the library *sources* (`Box.java`) folded in as an overlay, go-to-definition on a
    // classpath type lands on its real source declaration instead of being suppressed.
    let src = "class Test { Box<String> field; }";
    let node = parse(src);
    let resolved = resolve_node(&node);

    let lib = FileId(100);
    let sources = ProjectIndex::index_source_locations(&[(lib, parse(BOX_SOURCE))]);
    let classpath = ProjectIndex::lower_classpath(std::slice::from_ref(&box_classfile()));
    let index = ProjectIndex::builder(&[(FileId(0), node.clone())])
        .with_stdlib()
        .with_classpath(&classpath)
        .with_source_locations(&sources)
        .build();

    let offset = src.find("Box").expect("Box in source");
    let (file, range) = index
        .definition_at(FileId(0), &resolved, offset)
        .expect("a classpath type with sources is a navigation target");
    assert_eq!(file, lib, "navigates into the library source file");
    // The target is the `Box` name token of the `class Box` declaration (not the word "Box" in the
    // file's leading comment).
    let want = BOX_SOURCE.find("class Box").expect("Box decl in source") + "class ".len();
    assert_eq!(range, want..want + 3);
}

#[test]
fn source_dep_type_is_typed_from_source_and_navigates() {
    // A `git`/`path` dependency: `Box.java` is folded in as a `Source`-origin type with NO `.class`
    // backing it, so the source is both the typing authority and the navigation target.
    let node = parse(SRC);
    let resolved = resolve_node(&node);

    let lib = FileId(100);
    let lib_box = parse(BOX_SOURCE);
    let index = ProjectIndex::builder(&[(FileId(0), node.clone())])
        .with_stdlib()
        .with_source_deps(&[(lib, lib_box)])
        .with_classpath(&ProjectIndex::lower_classpath(&[]))
        .with_source_locations(&SourceLocations::default())
        .build();

    // Typing flows through the library source: `Box<String>.get()` substitutes `T` ↦ `String`.
    let ti = infer(&node, &resolved, &index, FileId(0));
    let expr = node
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|e| e.syntax().text().to_string().trim() == "b.get()")
        .expect("b.get() expression");
    let r = expr.syntax().text_range();
    assert_eq!(
        ti.type_of_expr(usize::from(r.start())..usize::from(r.end()))
            .map(|t| t.to_string())
            .as_deref(),
        Some("String")
    );

    // Go-to-definition on the `Box` type reference lands on the `class Box` declaration in the
    // library source — directly via the item's own file/range, no overlay needed.
    let offset = SRC.find("Box").expect("Box in source");
    let (file, range) = index
        .definition_at(FileId(0), &resolved, offset)
        .expect("a source-dep type is a navigation target");
    assert_eq!(file, lib);
    let want = BOX_SOURCE.find("class Box").expect("Box decl") + "class ".len();
    assert_eq!(range, want..want + 3);

    // The member `get` likewise carries its real source location in its own `file`/`name_range`
    // (a source-dep member needs no `source_location` overlay).
    let box_id = index
        .resolve_type_name(FileId(0), "Box", None)
        .project_id()
        .expect("Box resolves to the source-dep item");
    let get = index.member(
        index
            .resolve_member(box_id, "get", Namespace::Method)
            .unwrap(),
    );
    assert_eq!(get.file, lib);
    assert_eq!(get.source_location, None);
    let want = BOX_SOURCE.find("get(").expect("get decl");
    assert_eq!(get.name_range, want..want + 3);
}

#[test]
fn classpath_member_navigates_to_library_source() {
    // The same overlay gives a classpath *member* a real source location: `Box.get` points at its
    // `get` declaration in `Box.java`.
    let src = "class Test { Box<String> field; }";
    let node = parse(src);

    let lib = FileId(100);
    let sources = ProjectIndex::index_source_locations(&[(lib, parse(BOX_SOURCE))]);
    let classpath = ProjectIndex::lower_classpath(std::slice::from_ref(&box_classfile()));
    let index = ProjectIndex::builder(&[(FileId(0), node)])
        .with_stdlib()
        .with_classpath(&classpath)
        .with_source_locations(&sources)
        .build();

    let box_id = index
        .resolve_type_name(FileId(0), "Box", None)
        .project_id()
        .expect("Box resolves to a classpath item");
    let get_id = index
        .resolve_member(box_id, "get", Namespace::Method)
        .expect("Box.get resolves");
    let (file, range) = index
        .member(get_id)
        .source_location
        .clone()
        .expect("a classpath member with sources has a source location");
    assert_eq!(file, lib);
    let want = BOX_SOURCE.find("get(").expect("get decl in source");
    assert_eq!(range, want..want + 3);
}
