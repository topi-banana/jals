//! Tests for `jals_hir::ProjectIndex::signature_help`: locating the call at the cursor, resolving its overloads,
//! and rendering each signature (`name(type1 p1, type2 p2)`) with the active parameter/overload.

use jals_hir::{FileId, ProjectIndex, Resolved, SignatureHelp};
use jals_syntax::SyntaxNode;

/// Build an index over `sources`, place the cursor at the `$0` marker in `sources[file]` (removed
/// before parsing), and run signature help there.
fn help(sources: &[&str], file: usize) -> Option<SignatureHelp> {
    let mut texts: Vec<String> = sources.iter().map(ToString::to_string).collect();
    let offset = texts[file].find("$0").expect("a $0 cursor marker");
    texts[file].replace_range(offset..offset + 2, "");
    let nodes: Vec<(FileId, SyntaxNode)> = texts
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                FileId(u32::try_from(i).unwrap()),
                jals_syntax::Parse::parse(s).syntax(),
            )
        })
        .collect();
    let index = ProjectIndex::builder(&nodes).build();
    let (fid, root) = &nodes[file];
    let resolved = Resolved::resolve_node(root);
    index.signature_help(root, &resolved, *fid, offset)
}

#[test]
fn renders_param_names_and_active_index() {
    let h = help(
        &["class C { int area(int w, int h) {} void g() { area(1, $0); } }"],
        0,
    )
    .expect("the cursor is in a call to `area`");
    assert_eq!(h.signatures.len(), 1);
    assert_eq!(h.signatures[0].label, "area(int w, int h)");
    assert_eq!(h.signatures[0].parameters.len(), 2);
    assert_eq!(h.active_parameter, 1);
    assert_eq!(h.active_signature, 0);
    // The recorded ranges index into the label at the parameters.
    let label = &h.signatures[0].label;
    assert_eq!(&label[h.signatures[0].parameters[0].clone()], "int w");
    assert_eq!(&label[h.signatures[0].parameters[1].clone()], "int h");
}

#[test]
fn empty_args_is_parameter_zero() {
    let h = help(&["class C { void f(int x) {} void g() { f($0); } }"], 0).unwrap();
    assert_eq!(h.active_parameter, 0);
    assert_eq!(h.signatures[0].label, "f(int x)");
}

#[test]
fn qualified_call_on_a_sibling_type() {
    let h = help(
        &[
            "class Box { int area(int w, int h) {} }",
            "class C { void g(Box b) { b.area(1, $0); } }",
        ],
        1,
    )
    .expect("`b.area(..)` resolves on the project type `Box`");
    assert_eq!(h.signatures[0].label, "area(int w, int h)");
    assert_eq!(h.active_parameter, 1);
}

#[test]
fn overloads_pick_active_signature_by_arity() {
    let src = "class C { void f(int a) {} void f(int a, int b) {} void g() { f(1, $0); } }";
    let h = help(&[src], 0).unwrap();
    assert_eq!(h.signatures.len(), 2);
    assert_eq!(h.active_parameter, 1);
    // The cursor is on the second argument, so the highlighted overload must have two parameters.
    assert_eq!(h.signatures[h.active_signature].parameters.len(), 2);
}

#[test]
fn nested_call_targets_the_inner_call() {
    let src = "class C { int inner(int a, int b) {} int outer(int x) {} void g() { outer(inner(1, $0)); } }";
    let h = help(&[src], 0).unwrap();
    assert_eq!(h.signatures[0].label, "inner(int a, int b)");
    assert_eq!(h.active_parameter, 1);
}

#[test]
fn external_receiver_has_no_signature_help() {
    // `s` is a `String` (external) — its members are not indexed, so there is nothing to show.
    let h = help(&["class C { void g(String s) { s.substring($0); } }"], 0);
    assert!(h.is_none());
}

#[test]
fn unnamed_parameter_renders_as_type_only() {
    let h = help(&["class C { void f(int _) {} void g() { f($0); } }"], 0).unwrap();
    assert_eq!(h.signatures[0].label, "f(int)");
}

#[test]
fn does_not_panic_on_broken_calls() {
    // Invariant: never panics, even on incomplete / arbitrary input. May return `None`.
    for src in [
        "class C { void g() { f($0",
        "class C { void g() { f(,$0) } }",
        "$0",
        "f($0)",
    ] {
        let _ = help(&[src], 0);
    }
}
