//! Tests for assignment-context type-mismatch detection (`jals_hir::type_mismatches`): the
//! index-free subset (primitives, `null`, arrays) and the index-aware project subtyping cases.

use jals_hir::{FileId, ProjectIndex, TypeMismatch, resolve_node, type_mismatches};
use jals_syntax::SyntaxNode;

/// Mismatches found without a project index (reference types stay external / lenient).
fn free(src: &str) -> Vec<TypeMismatch> {
    let root = jals_syntax::parse(src).syntax();
    let resolved = resolve_node(&root);
    type_mismatches(&root, &resolved, None)
}

/// Mismatches found in `sources[file]` with a project index built over every source.
fn indexed(sources: &[&str], file: u32) -> Vec<TypeMismatch> {
    let nodes: Vec<(FileId, SyntaxNode)> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| (FileId(i as u32), jals_syntax::parse(s).syntax()))
        .collect();
    let index = ProjectIndex::build(&nodes);
    let (fid, root) = &nodes[file as usize];
    let resolved = resolve_node(root);
    type_mismatches(root, &resolved, Some((&index, *fid)))
}

/// Wraps a statement body in a method so it parses as a valid local context.
fn in_method(body: &str) -> String {
    format!("class C {{ void m() {{ {body} }} }}")
}

#[test]
fn primitive_narrowing_is_flagged() {
    for body in [
        "int x = 1.0;",   // double -> int
        "int x = 1L;",    // long -> int
        "float f = 1.0;", // double -> float
        "long l = 1.0;",  // double -> long
    ] {
        let found = free(&in_method(body));
        assert_eq!(found.len(), 1, "expected one mismatch in `{body}`");
    }
}

#[test]
fn boolean_and_null_mismatches_are_flagged() {
    assert_eq!(free(&in_method("boolean b = 1;")).len(), 1);
    assert_eq!(free(&in_method("int x = true;")).len(), 1);
    assert_eq!(free(&in_method("int x = null;")).len(), 1);

    let m = &free(&in_method("int x = null;"))[0];
    assert_eq!(m.expected.to_string(), "int");
    assert_eq!(m.found.to_string(), "null");
    assert!(m.message().contains("`null`"));
    assert!(m.message().contains("`int`"));
}

#[test]
fn array_element_mismatch_is_flagged() {
    assert_eq!(free(&in_method("int[] a = new long[0];")).len(), 1);
}

#[test]
fn widening_and_var_are_not_flagged() {
    for body in [
        "long x = 1;",    // int -> long widening
        "double d = 1;",  // int -> double widening
        "int x = 'a';",   // char -> int widening
        "float f = 1L;",  // long -> float widening
        "var s = \"x\";", // var: no written type to disagree with
        "int x = 1;",     // identity
    ] {
        assert!(
            free(&in_method(body)).is_empty(),
            "unexpected mismatch in `{body}`"
        );
    }
}

#[test]
fn constant_narrowing_to_small_integer_is_not_flagged() {
    // Legal under JLS §5.2 constant narrowing — must not be a false positive.
    for body in ["byte b = 1;", "short s = 2;", "char c = 65;"] {
        assert!(
            free(&in_method(body)).is_empty(),
            "constant narrowing `{body}` must be allowed"
        );
    }
}

#[test]
fn fields_are_checked_too() {
    assert_eq!(free("class C { int x = 1.0; }").len(), 1);
    assert!(free("class C { long x = 1; }").is_empty());
}

#[test]
fn simple_assignment_is_checked_but_compound_is_not() {
    assert_eq!(free(&in_method("int x = 0; x = 1.0;")).len(), 1);
    // Compound assignment carries an implicit narrowing cast and is legal.
    assert!(free(&in_method("int x = 0; x += 1.0;")).is_empty());
}

#[test]
fn multi_declarator_is_skipped() {
    // The name<->initializer pairing is ambiguous from the flat CST, so it is not checked.
    assert!(free(&in_method("int a = 1, b = 2.0;")).is_empty());
}

#[test]
fn project_subtyping_mismatch_needs_the_index() {
    let src = "class Base {} class Sub extends Base {} \
               class C { void m() { Sub s = new Base(); } }";
    // Index-free: `Base`/`Sub` are both external and lenient, so nothing is reported.
    assert!(free(src).is_empty());
    // Index-aware: assigning a `Base` value to a `Sub` slot is a real mismatch.
    assert_eq!(indexed(&[src], 0).len(), 1);
}

#[test]
fn upcast_and_unrelated_project_types() {
    // Upcast `Base b = new Sub()` is fine.
    let ok = "class Base {} class Sub extends Base {} \
              class C { void m() { Base b = new Sub(); } }";
    assert!(indexed(&[ok], 0).is_empty());

    // Unrelated project types do not assign.
    let bad = "class Foo {} class Bar {} \
               class C { void m() { Foo f = new Bar(); } }";
    assert_eq!(indexed(&[bad], 0).len(), 1);
}

#[test]
fn external_target_stays_lenient_even_with_an_index() {
    // `String` is java.lang (external): boxing-style leniency means no mismatch is reported yet.
    let src = "class C { void m() { String s = 1; } }";
    assert!(indexed(&[src], 0).is_empty());
}
