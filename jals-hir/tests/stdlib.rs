//! Tests for the embedded `java.lang` stubs, indexed via [`ProjectIndex::build_with_stdlib`].
//!
//! They pin the Step-1 contract: core JDK types become real (stub-origin) project items, so a
//! reference to one resolves and its members infer — while the default [`ProjectIndex::build`] is
//! unchanged (those types stay `external`), and a stub is never offered as a navigation target.

use jals_hir::{
    FileId, ItemOrigin, ProjectIndex, Resolved, Ty, TypeInference, TypeResolution, infer,
    resolve_node,
};
use jals_syntax::SyntaxNode;

/// Parses each source, keeping its `SOURCE_FILE` node alive (rowan nodes are ref-counted).
fn nodes(sources: &[&str]) -> Vec<(FileId, SyntaxNode)> {
    sources
        .iter()
        .enumerate()
        .map(|(i, s)| (FileId(i as u32), jals_syntax::parse(s).syntax()))
        .collect()
}

/// Analyses a single-file project *with the stdlib stubs*, returning the pieces a test queries.
fn analyse_with_stdlib(src: &str) -> (SyntaxNode, Resolved, TypeInference, ProjectIndex) {
    let node = jals_syntax::parse(src).syntax();
    let resolved = resolve_node(&node);
    let index = ProjectIndex::build_with_stdlib(&[(FileId(0), node.clone())]);
    let ti = infer(&node, &resolved, &index, FileId(0));
    (node, resolved, ti, index)
}

/// The inferred type of the first definition named `name`.
fn def_ty(ti: &TypeInference, resolved: &Resolved, name: &str) -> Ty {
    let def = resolved
        .defs
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no definition named `{name}`"));
    ti.type_of_def(def.id).clone()
}

#[test]
fn string_resolves_to_a_stdlib_project_item() {
    let src = "class C { void m() { String s = null; } }";
    let (_node, resolved, ti, index) = analyse_with_stdlib(src);

    let ty = def_ty(&ti, &resolved, "s");
    assert_eq!(ty.to_string(), "String");
    let id = ty
        .project_id()
        .expect("with the stubs indexed, `String` is a project (not external) type");
    assert_eq!(index.item(id).origin, ItemOrigin::Stdlib);
    assert_eq!(index.item(id).fqn.to_string(), "java.lang.String");
}

#[test]
fn string_length_infers_int() {
    // The member call sees through `String` to the stub's `int length()`.
    let src = "class C { void m() { String s = null; var n = s.length(); } }";
    let (_node, resolved, ti, _index) = analyse_with_stdlib(src);
    assert_eq!(def_ty(&ti, &resolved, "n").to_string(), "int");
}

#[test]
fn object_is_a_supertype_of_string() {
    let (_node, _resolved, _ti, index) = analyse_with_stdlib("class C { }");
    let string_id = index
        .resolve_type_name(FileId(0), "String", None)
        .project_id()
        .expect("String is indexed");
    let object_id = index
        .resolve_type_name(FileId(0), "Object", None)
        .project_id()
        .expect("Object is indexed");
    assert!(index.is_subtype(string_id, object_id));
    // CharSequence (a stub interface) is a supertype too.
    let cs_id = index
        .resolve_type_name(FileId(0), "CharSequence", None)
        .project_id()
        .expect("CharSequence is indexed");
    assert!(index.is_subtype(string_id, cs_id));
}

#[test]
fn stdlib_symbol_goto_is_none() {
    // A stub type has no host-openable file, so go-to-definition on it yields nothing.
    let src = "class C { String f; }";
    let (_node, resolved, _ti, index) = analyse_with_stdlib(src);
    let offset = src.find("String f").expect("type reference present");
    assert_eq!(index.definition_at(FileId(0), &resolved, offset), None);
}

#[test]
fn default_build_keeps_string_external() {
    // Regression guard: without the stubs, `String` is external exactly as before.
    let src = "class C { String f; }";
    let index = ProjectIndex::build(&nodes(&[src]));
    assert_eq!(
        index.resolve_type_name(FileId(0), "String", None),
        TypeResolution::External,
    );
}

#[test]
fn build_with_stdlib_never_panics_and_project_items_are_in_bounds() {
    let sources = [
        "",
        "class C { String s; void m() { var n = s.length(); } }",
        "package a; import java.util.List; class D extends Object { }",
        "🦀 class Broken { int (}",
    ];
    let nodes = nodes(&sources);
    let index = ProjectIndex::build_with_stdlib(&nodes);
    // Every *project* item's name range stays within its source; stub items live at reserved high
    // file ids and are excluded from this host-source bounds check.
    for item in index.items().filter(|it| it.origin == ItemOrigin::Project) {
        let src = sources[item.file.0 as usize];
        assert!(
            item.name_range.end <= src.len(),
            "project item {} out of bounds",
            item.fqn
        );
    }
}
