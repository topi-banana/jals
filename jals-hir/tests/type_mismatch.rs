//! Tests for assignment-context type-mismatch detection: the
//! index-free subset (primitives, `null`, arrays) and the index-aware project subtyping cases.

use jals_classfile::ClassFile;
use jals_hir::{FileAnalysis, FileId, MismatchKind, ProjectIndex, TypeMismatch};
use jals_syntax::SyntaxNode;

/// Mismatches found without a project index (reference types stay external / lenient).
fn free(src: &str) -> Vec<TypeMismatch> {
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(src)).syntax();
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    jals_exec::block_on_inline(analysis.type_mismatches())
}

/// Mismatches found in `sources[file]` with a project index built over every source.
fn indexed(sources: &[&str], file: u32) -> Vec<TypeMismatch> {
    let nodes: Vec<(FileId, SyntaxNode)> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                FileId(u32::try_from(i).unwrap()),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(s)).syntax(),
            )
        })
        .collect();
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&nodes).build());
    let (fid, root) = &nodes[file as usize];
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(root));
    jals_exec::block_on_inline(analysis.in_project(&index, *fid).type_mismatches())
}

/// Mismatches found in `sources[file]` with the embedded stubs indexed as well.
///
/// The sibling of [`indexed`] that the *product* actually resembles: every host builds its index
/// with `ProjectIndex::builder(..).with_stdlib()`, so a guard written through [`indexed`] alone is
/// blind to anything the stubs change — which is how a `java.lang.Object` reachable from every type
/// could silence [`ProjectIndex::method_set_complete`] for the whole workspace without a red test.
fn indexed_with_stdlib(sources: &[&str], file: u32) -> Vec<TypeMismatch> {
    let nodes = parsed(sources);
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&nodes).with_stdlib().build());
    mismatches_of(&nodes, &index, file)
}

/// Mismatches found in `sources[file]` with `java.lang` types indexed from **class files** — so they
/// are [`ItemOrigin::Classpath`](jals_hir::ItemOrigin::Classpath) items rather than stubs.
///
/// This is the only shape that exercises the precise rules. `Ty::demote_stdlib` rewrites a
/// stub-origin type to its lenient by-name form before assignment conversion, so through the stubs
/// `Object` never reaches the project-to-project subtyping arm and `Integer` never reaches the
/// project boxing arm — both answer `true` for a reason that says nothing about either rule. A real
/// JDK on the classpath is not demoted, and both arms decide the answer.
fn indexed_with_classpath(
    sources: &[&str],
    file: u32,
    classfiles: &[ClassFile],
) -> Vec<TypeMismatch> {
    let nodes = parsed(sources);
    let lowered = jals_exec::block_on_inline(ProjectIndex::lower_classpath(classfiles));
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&nodes)
            .with_classpath(&lowered)
            .build(),
    );
    mismatches_of(&nodes, &index, file)
}

/// The `java.lang` fixtures, compiled from `tests/fixtures/JavaLang*.java` (see their provenance
/// headers, which record the `--patch-module` invocation `java.lang` needs).
fn java_lang_fixtures() -> Vec<ClassFile> {
    ["JavaLangObject.class", "JavaLangInteger.class"]
        .iter()
        .map(|name| {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name);
            jals_exec::block_on_inline(ClassFile::read(
                std::fs::read(path)
                    .unwrap_or_else(|e| panic!("read {name}: {e}"))
                    .as_slice(),
            ))
            .unwrap_or_else(|e| panic!("parse {name}: {e:?}"))
        })
        .collect()
}

/// Each source parsed and paired with its [`FileId`], in order.
fn parsed(sources: &[&str]) -> Vec<(FileId, SyntaxNode)> {
    sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                FileId(u32::try_from(i).unwrap()),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(s)).syntax(),
            )
        })
        .collect()
}

/// The mismatches in `nodes[file]` against an already-built `index`.
fn mismatches_of(
    nodes: &[(FileId, SyntaxNode)],
    index: &ProjectIndex,
    file: u32,
) -> Vec<TypeMismatch> {
    let (fid, root) = &nodes[file as usize];
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(root));
    jals_exec::block_on_inline(analysis.in_project(index, *fid).type_mismatches())
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
    let MismatchKind::Assignment { expected, found } = m.kind() else {
        panic!("an initializer mismatch is an assignment-context one");
    };
    assert_eq!(found.to_string(), "null");
    assert_eq!(expected.to_string(), "int");
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

/// An external target is lenient about the *hierarchy* it cannot see, not about boxing.
///
/// Boxing produces one wrapper per primitive (JLS §5.1.7), and a name that is not that wrapper is not
/// a boxing target however little is known about it. Admitting any external name made `String s = 1;`
/// pass — and, worse, made every primitive argument applicable to every reference parameter, which
/// overload selection then resolved by declaration order.
#[test]
fn an_external_target_is_lenient_about_hierarchy_not_about_boxing() {
    // `int` does not box to a `String`, however external `String` is.
    assert_eq!(
        indexed(&["class C { void m() { String s = 1; } }"], 0).len(),
        1
    );
    // It does box to its own wrapper and to what every wrapper is.
    for target in ["Integer", "Object", "Number", "Comparable"] {
        let src = format!("class C {{ void m() {{ {target} n = 1; }} }}");
        assert!(
            indexed(&[&src], 0).is_empty(),
            "`int` boxes to {target}, so nothing should be flagged"
        );
    }
    // An unindexed type's *hierarchy* is still unknown, so a reference-to-reference assignment stays
    // lenient.
    assert!(indexed(&["class C { void m() { Runnable r = \"s\"; } }"], 0).is_empty());
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
    // An exactly-applicable overload silences the call even where a sibling rejects the argument.
    let ok = "class C { void f(int x) {} void f(boolean b) {} void g() { f(1); f(true); } }";
    assert!(indexed(&[ok], 0).is_empty());
    // A widening one does too.
    let widened = "class C { void f(long x) {} void f(String s) {} void g() { f(1); } }";
    assert!(indexed(&[widened], 0).is_empty());
    // But a `double` fits neither an `int` nor a `String`, so the call is flagged. `f(String)` used
    // to accept it — every primitive was assignable to every external name — which silenced a call
    // no JVM would ever link.
    let bad = "class C { void f(int x) {} void f(String s) {} void g() { f(1.0); } }";
    assert_eq!(indexed(&[bad], 0).len(), 1);
}

#[test]
fn an_override_is_still_checked() {
    // The same signature in a subclass collapses to one candidate, so the call is checked.
    let src = "class B { void f(int x) {} } class S extends B { void f(int x) {} } \
               class C { void g(S s) { s.f(1.0); } }";
    assert_eq!(indexed(&[src], 0).len(), 1);
}

#[test]
fn a_varargs_method_checks_its_trailing_arguments() {
    // A trailing argument is checked against the *element* type, so a `double` fits no `int...` — this
    // used to be silently accepted, because a varargs candidate was dropped before it was checked.
    let bad = "class C { void v(int... xs) {} void g() { v(1.0); } }";
    assert_eq!(indexed(&[bad], 0).len(), 1);
    // Any number of applicable trailing arguments is fine, including none, and so is the array itself.
    let ok = "class C { void v(int... xs) {} void g() { v(); v(1); v(1, 2); v(new int[] {3}); } }";
    assert!(indexed(&[ok], 0).is_empty());
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
    let MismatchKind::NoOverload { name, .. } = found[0].kind() else {
        panic!("an unmatched call is a no-overload mismatch");
    };
    assert_eq!(name, "f");
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

/// The guard on [`ProjectIndex::method_set_complete`] surviving an indexed `java.lang.Object`.
///
/// Every class implicitly extends `Object`, and the stubs are `ItemOrigin::Stdlib` — whose whole
/// purpose in that predicate is to make an overload set *incomplete*. Attaching the implicit edge
/// without exempting it therefore makes the walk reach a stub from **every** type, `check_call`
/// concludes nothing anywhere, and `type-mismatch` goes silent across the workspace with no other
/// test noticing. What makes the exemption sound is that `is_object_method` already forces
/// `false` for every name `Object` declares, so skipping the edge can lose nothing.
#[test]
fn no_overload_is_still_reported_with_stdlib_indexed() {
    // The `complete` case of `overload_reporting_is_guarded_by_method_set_completeness`, re-asked
    // of the index shape the product actually builds.
    let complete = "class Foo {} \
                    class C extends Foo { void f(int x) {} void f(boolean b) {} void g() { f(1.0); } }";
    assert_eq!(indexed_with_stdlib(&[complete], 0).len(), 1);
    // And the external-supertype suppression is not collateral damage of the exemption.
    let external =
        "class C extends Foo { void f(int x) {} void f(boolean b) {} void g() { f(1.0); } }";
    assert!(indexed_with_stdlib(&[external], 0).is_empty());
}

/// A project type is assignable to `java.lang.Object` however `Object` reached the index.
///
/// Through the stubs this passes for a reason that says nothing about subtyping: `demote_stdlib`
/// rewrites the stub to its lenient by-name form, and an external target is assignable from
/// anything. Through a classpath `Object` there is no demotion, so the answer comes from
/// `is_subtype` walking a real supertype chain — which is exactly the chain the implicit
/// `java.lang.Object` edge supplies, and which is empty for `class Foo {}` without it.
#[test]
fn assigning_a_project_type_to_object_is_not_a_mismatch() {
    let src = "class Foo {} class C { void m() { Object o = new Foo(); } }";
    assert!(indexed_with_stdlib(&[src], 0).is_empty());
    assert!(indexed_with_classpath(&[src], 0, &java_lang_fixtures()).is_empty());
}

/// Autoboxing survives a wrapper that reached the index as a real class rather than as a stub.
///
/// A stub `Integer` is rewritten to its lenient by-name form before assignment conversion, so the
/// boxing rule was only ever consulted through the external arm. Scoring against a real JDK indexes
/// `java.lang.Integer` from `ct.sym`, there is no demotion, and reading only the external spelling
/// made `Integer n = 1;` a mismatch — and, worse, silently dropped `f(Integer)` from the applicable
/// overloads for `f(1)`.
#[test]
fn boxing_survives_a_classpath_wrapper() {
    let fixtures = java_lang_fixtures();
    let src = "class C { void m() { Integer n = 1; } }";
    assert!(indexed_with_stdlib(&[src], 0).is_empty());
    assert!(indexed_with_classpath(&[src], 0, &fixtures).is_empty());
    let unboxing = "class C { void m(Integer boxed) { int n = boxed; } }";
    assert!(indexed_with_stdlib(&[unboxing], 0).is_empty());
    assert!(indexed_with_classpath(&[unboxing], 0, &fixtures).is_empty());
}

/// An array is assignable to `java.lang.Object` however `Object` reached the index.
///
/// The sibling of [`assigning_a_project_type_to_object_is_not_a_mismatch`] for the one reference
/// type that has no supertype chain to walk. Through the stubs the target is demoted to a spelling
/// and the lenient external arm answers; through a classpath `Object` it is an indexed item, and
/// the array arm decided it by refusing outright — a reported mismatch on `Object o = args;` in
/// every `main`, and, in argument position, `f(Object)` dropped from the applicable set so overload
/// selection picked the wrong member or none.
#[test]
fn assigning_an_array_to_object_is_not_a_mismatch() {
    let fixtures = java_lang_fixtures();
    let src = "class C { void m() { int[] a = new int[3]; Object o = a; } }";
    assert!(indexed_with_stdlib(&[src], 0).is_empty());
    assert!(indexed_with_classpath(&[src], 0, &fixtures).is_empty());
    let overload = "class C { void f(Object o) {} void f(int n) {} void m(int[] a) { f(a); } }";
    assert!(indexed_with_classpath(&[overload], 0, &fixtures).is_empty());
}

/// A project type merely *named* like a wrapper class is not one.
///
/// Boxing and unboxing are defined on eight classes in `java.lang` (JLS §5.1.7 / §5.1.8), and the
/// rule reached an indexed item through its fully-qualified name — matched on its last segment
/// alone, so `package app; class Number {}` accepted `Number n = 1;`. The damage is not a missing
/// diagnostic: applicability is built on the same conversion, so `f(app.Number)` became applicable
/// to `f(1)` and the lowering emitted `invokevirtual C.f:(Lapp/Number;)V` with an `int` on the
/// stack.
#[test]
fn a_project_type_named_like_a_wrapper_is_not_one() {
    let boxing = "package app; class Number {} class C { void m() { Number n = 1; } }";
    assert_eq!(indexed(&[boxing], 0).len(), 1);
    let unboxing =
        "package app; class Integer {} class C { void m(Integer boxed) { int n = boxed; } }";
    assert_eq!(indexed(&[unboxing], 0).len(), 1);
}
