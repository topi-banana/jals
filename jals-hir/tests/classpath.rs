//! The classpath bridge end-to-end: a `.class` file folded into the index resolves its members and
//! substitutes its generics, exactly like a source type.

use std::path::PathBuf;

use jals_classfile::ClassFile;
use jals_hir::{FileAnalysis, FileId, ItemOrigin, Namespace, ProjectIndex, SourceLocations};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{self, AstNode};

/// `Box<T>` (generic, with `T get()` / `void set(T)`), compiled from `tests/fixtures/Box.java`.
fn box_classfile() -> ClassFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Box.class");
    jals_exec::block_on_inline(ClassFile::read(
        std::fs::read(path).expect("read Box.class").as_slice(),
    ))
    .expect("parse Box.class")
}

/// The `Box.java` source `Box.class` was compiled from — used as a library-sources overlay.
const BOX_SOURCE: &str = include_str!("fixtures/Box.java");

fn parse(src: &str) -> SyntaxNode {
    jals_exec::block_on_inline(jals_syntax::Parse::parse(src)).syntax()
}

/// The inferred type of the expression whose source text is exactly `text`, with `classfiles` folded
/// into the index as classpath types.
fn expr_ty(src: &str, text: &str, classfiles: &[ClassFile]) -> String {
    let node = parse(src);
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&node));
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), node.clone())])
            .with_stdlib()
            .with_classpath(&jals_exec::block_on_inline(ProjectIndex::lower_classpath(
                classfiles,
            )))
            .build(),
    );
    let semantics = analysis.in_project(&index, FileId(0));
    let ti = jals_exec::block_on_inline(semantics.typed());
    let expr = node
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|e| e.syntax().text().to_string().trim() == text)
        .unwrap_or_else(|| panic!("no expression `{text}`"));
    let r = expr.syntax().text_range();
    ti.type_of_expr(usize::from(r.start())..usize::from(r.end()))
        .map_or_else(|| "<none>".to_owned(), ToString::to_string)
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
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&node));
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), node)])
            .with_stdlib()
            .with_classpath(&jals_exec::block_on_inline(ProjectIndex::lower_classpath(
                std::slice::from_ref(&box_classfile()),
            )))
            .build(),
    );
    let offset = src.find("Box").expect("Box in source");
    assert!(
        analysis
            .in_project(&index, FileId(0))
            .definition_at(offset)
            .is_none(),
        "go-to-def into a classpath type should be suppressed"
    );
}

#[test]
fn classpath_type_navigates_to_library_source() {
    // With the library *sources* (`Box.java`) folded in as an overlay, go-to-definition on a
    // classpath type lands on its real source declaration instead of being suppressed.
    let src = "class Test { Box<String> field; }";
    let node = parse(src);
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&node));

    let lib = FileId(100);
    let sources = jals_exec::block_on_inline(ProjectIndex::index_source_locations(&[(
        lib,
        parse(BOX_SOURCE),
    )]));
    let classpath = jals_exec::block_on_inline(ProjectIndex::lower_classpath(
        std::slice::from_ref(&box_classfile()),
    ));
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), node)])
            .with_stdlib()
            .with_classpath(&classpath)
            .with_source_locations(&sources)
            .build(),
    );

    let offset = src.find("Box").expect("Box in source");
    let (file, range) = analysis
        .in_project(&index, FileId(0))
        .definition_at(offset)
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
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&node));

    let lib = FileId(100);
    let lib_box = parse(BOX_SOURCE);
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), node.clone())])
            .with_stdlib()
            .with_source_deps(&[(lib, lib_box)])
            .with_classpath(&jals_exec::block_on_inline(ProjectIndex::lower_classpath(
                &[],
            )))
            .with_source_locations(&SourceLocations::default())
            .build(),
    );

    // Typing flows through the library source: `Box<String>.get()` substitutes `T` ↦ `String`.
    let semantics = analysis.in_project(&index, FileId(0));
    let ti = jals_exec::block_on_inline(semantics.typed());
    let expr = node
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|e| e.syntax().text().to_string().trim() == "b.get()")
        .expect("b.get() expression");
    let r = expr.syntax().text_range();
    assert_eq!(
        ti.type_of_expr(usize::from(r.start())..usize::from(r.end()))
            .map(ToString::to_string)
            .as_deref(),
        Some("String")
    );

    // Go-to-definition on the `Box` type reference lands on the `class Box` declaration in the
    // library source — directly via the item's own file/range, no overlay needed.
    let offset = SRC.find("Box").expect("Box in source");
    let (file, range) = analysis
        .in_project(&index, FileId(0))
        .definition_at(offset)
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
    let sources = jals_exec::block_on_inline(ProjectIndex::index_source_locations(&[(
        lib,
        parse(BOX_SOURCE),
    )]));
    let classpath = jals_exec::block_on_inline(ProjectIndex::lower_classpath(
        std::slice::from_ref(&box_classfile()),
    ));
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), node)])
            .with_stdlib()
            .with_classpath(&classpath)
            .with_source_locations(&sources)
            .build(),
    );

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

/// `java.lang.Object` from a class file, for the priority tests below.
fn java_lang_object() -> ClassFile {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/JavaLangObject.class");
    jals_exec::block_on_inline(ClassFile::read(
        std::fs::read(path)
            .expect("read JavaLangObject.class")
            .as_slice(),
    ))
    .expect("parse JavaLangObject.class")
}

/// An index over `src` with both the stubs and `classfiles` folded in.
fn index_with_stdlib_and_classpath(src: &str, classfiles: &[ClassFile]) -> ProjectIndex {
    let node = parse(src);
    let lowered = jals_exec::block_on_inline(ProjectIndex::lower_classpath(classfiles));
    jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), node)])
            .with_stdlib()
            .with_classpath(&lowered)
            .build(),
    )
}

/// A real `.class` outranks the embedded stub of the same fully-qualified name.
///
/// The stubs are ~58 signature-only types kept for a host with no classpath at all; where a real
/// one exists it is strictly better, and `ItemOrigin::Classpath` already documents that its member
/// set is *complete* while a stub's is deliberately partial. With the stubs winning, indexing a
/// real JDK resolved `java.lang.Object` to the stub and the classpath was decoded for nothing.
#[test]
fn a_classpath_type_outranks_a_stub_of_the_same_name() {
    let index =
        index_with_stdlib_and_classpath("class C {}", std::slice::from_ref(&java_lang_object()));
    let object = index
        .item_by_fqn("java.lang.Object")
        .expect("java.lang.Object is indexed");
    assert_eq!(index.item(object).origin, ItemOrigin::Classpath);
}

/// The project still outranks the classpath: a source type is the one the host can edit, and a jar
/// holding a stale copy of it must not shadow what is on disk.
#[test]
fn a_project_type_still_outranks_a_classpath_type_of_the_same_name() {
    let index = index_with_stdlib_and_classpath(
        "public class Box<T> { public T get() { return null; } }",
        std::slice::from_ref(&box_classfile()),
    );
    let box_id = index.item_by_fqn("Box").expect("Box is indexed");
    assert_eq!(index.item(box_id).origin, ItemOrigin::Project);
}
