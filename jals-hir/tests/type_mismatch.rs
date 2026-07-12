//! Tests for assignment-context type-mismatch detection (`jals_hir::TypeInference::type_mismatches`): the
//! index-free subset (primitives, `null`, arrays) and the index-aware project subtyping cases.

use jals_hir::{FileId, ProjectIndex, Resolved, TypeInference, TypeMismatch};
use jals_syntax::SyntaxNode;

/// Mismatches found without a project index (reference types stay external / lenient).
fn free(src: &str) -> Vec<TypeMismatch> {
    let root = jals_syntax::Parse::parse(src).syntax();
    let resolved = Resolved::resolve_node(&root);
    TypeInference::type_mismatches(&root, &resolved, None)
}

/// Mismatches found in `sources[file]` with a project index built over every source.
fn indexed(sources: &[&str], file: u32) -> Vec<TypeMismatch> {
    let nodes: Vec<(FileId, SyntaxNode)> = sources
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
    let (fid, root) = &nodes[file as usize];
    let resolved = Resolved::resolve_node(root);
    TypeInference::type_mismatches(root, &resolved, Some((&index, *fid)))
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
fn multi_declarator_each_initializer_is_checked() {
    // Each declarator is paired with its own initializer.
    assert_eq!(free(&in_method("int a = 1, b = 2.0;")).len(), 1); // only `b`
    assert_eq!(free(&in_method("int a = 1.0, b = 2;")).len(), 1); // only `a`
    assert_eq!(free(&in_method("int a = 1.0, b = 2.0;")).len(), 2); // both
    assert_eq!(free(&in_method("int a, b = 2.0;")).len(), 1); // `a` has no initializer
    assert!(free(&in_method("int a = 1, b = 2;")).is_empty()); // both fine
}

#[test]
fn return_mismatch_is_flagged() {
    assert_eq!(free("class C { int m() { return 1.0; } }").len(), 1);
    assert!(free("class C { int m() { return 1; } }").is_empty());
    // Constant narrowing applies to a `return` too (JLS §5.2).
    assert!(free("class C { byte m() { return 1; } }").is_empty());
    // A bare `return;` has no value to check.
    assert!(free("class C { void m() { return; } }").is_empty());
}

#[test]
fn return_inside_a_lambda_is_not_attributed_to_the_method() {
    // The `return 1.0` belongs to the lambda (target-typed), not to the `void` method.
    assert!(free("class C { void m() { run(() -> { return 1.0; }); } }").is_empty());
}

#[test]
fn return_subtyping_needs_the_index() {
    let mismatch = "class Base {} class Sub extends Base {} \
                    class C { Sub make() { return new Base(); } }";
    assert!(free(mismatch).is_empty()); // index-free: external & lenient
    assert_eq!(indexed(&[mismatch], 0).len(), 1); // returning a `Base` where `Sub` is required

    let upcast = "class Base {} class Sub extends Base {} \
                  class C { Base make() { return new Sub(); } }";
    assert!(indexed(&[upcast], 0).is_empty());
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

// ===== method argument checking (index-only) =====

#[test]
fn argument_type_is_checked_against_the_parameter() {
    let bad = "class C { void f(int x) {} void g() { f(1.0); } }";
    assert_eq!(indexed(&[bad], 0).len(), 1); // double argument to an int parameter
    // A widening / exact argument is fine.
    assert!(indexed(&["class C { void f(long x) {} void g() { f(1); } }"], 0).is_empty());
    assert!(indexed(&["class C { void f(int x) {} void g() { f(1); } }"], 0).is_empty());
}

#[test]
fn argument_checking_needs_the_index() {
    // The parameter types live in the project member model; the index-free path cannot see them.
    let src = "class C { void f(int x) {} void g() { f(1.0); } }";
    assert!(free(src).is_empty());
}

#[test]
fn argument_invocation_does_not_allow_constant_narrowing() {
    // Unlike `byte b = 1;` (assignment), `f(1)` for a `byte` parameter is a compile error (JLS §5.3
    // has no constant narrowing), so it *is* flagged.
    let src = "class C { void f(byte b) {} void g() { f(1); } }";
    assert_eq!(indexed(&[src], 0).len(), 1);
}

#[test]
fn argument_project_subtyping() {
    let bad = "class Base {} class Sub extends Base {} \
               class C { void f(Sub s) {} void g() { f(new Base()); } }";
    assert_eq!(indexed(&[bad], 0).len(), 1);
    let ok = "class Base {} class Sub extends Base {} \
              class C { void f(Base b) {} void g() { f(new Sub()); } }";
    assert!(indexed(&[ok], 0).is_empty());
}

#[test]
fn an_applicable_overload_silences_the_call() {
    // `f(String)` leniently accepts a `double` (external boxing target), so the call binds — nothing
    // is flagged even though `f(int)` rejects it.
    let src = "class C { void f(int x) {} void f(String s) {} void g() { f(1.0); } }";
    assert!(indexed(&[src], 0).is_empty());
    // An exactly-applicable overload also silences it.
    let ok = "class C { void f(int x) {} void f(boolean b) {} void g() { f(1); f(true); } }";
    assert!(indexed(&[ok], 0).is_empty());
}

#[test]
fn an_override_is_still_checked() {
    // The same signature in a subclass collapses to one candidate, so the call is checked.
    let src = "class B { void f(int x) {} } class S extends B { void f(int x) {} } \
               class C { void g(S s) { s.f(1.0); } }";
    assert_eq!(indexed(&[src], 0).len(), 1);
}

#[test]
fn varargs_methods_are_skipped() {
    let src = "class C { void v(int... xs) {} void g() { v(1.0); } }";
    assert!(indexed(&[src], 0).is_empty());
}

#[test]
fn arity_mismatch_is_not_a_type_error() {
    // Wrong number of arguments is a separate error class; this rule reports only type mismatches.
    let src = "class C { void f(int x) {} void g() { f(); f(1, 2); } }";
    assert!(indexed(&[src], 0).is_empty());
}

// ===== type-based overload resolution (B4) =====

#[test]
fn no_applicable_overload_is_reported_once() {
    // Both overloads definitively reject `double`, so the call matches none.
    let src = "class C { void f(int x) {} void f(boolean b) {} void g() { f(1.0); } }";
    let found = indexed(&[src], 0);
    assert_eq!(found.len(), 1);
    assert!(found[0].message().contains("no overload") && found[0].message().contains("`f`"));
}

#[test]
fn no_applicable_overload_with_project_parameter_types() {
    let bad = "class A {} class B {} \
               class C { void f(A a) {} void f(B b) {} void g() { f(1.0); } }";
    assert_eq!(indexed(&[bad], 0).len(), 1);
    // An exactly-matching project argument binds one overload — nothing flagged.
    let ok = "class A {} class B {} \
              class C { void f(A a) {} void f(B b) {} void g() { f(new A()); } }";
    assert!(indexed(&[ok], 0).is_empty());
}

#[test]
fn overload_reporting_is_guarded_by_method_set_completeness() {
    // `C extends Foo` where `Foo` is external: `Foo` may declare `f(double)`, so a "no overload"
    // conclusion is unsafe and suppressed.
    let external =
        "class C extends Foo { void f(int x) {} void f(boolean b) {} void g() { f(1.0); } }";
    assert!(indexed(&[external], 0).is_empty());
    // The same source with `Foo` defined in the project makes the set complete, so it is reported.
    let complete = "class Foo {} \
                    class C extends Foo { void f(int x) {} void f(boolean b) {} void g() { f(1.0); } }";
    assert_eq!(indexed(&[complete], 0).len(), 1);
}

#[test]
fn object_method_names_are_not_reported() {
    // `equals` is an `Object` method, so the call may bind to `Object.equals(Object)` — not flagged.
    let src = "class C { void equals(int x) {} void g() { equals(1.0); } }";
    assert!(indexed(&[src], 0).is_empty());
}
