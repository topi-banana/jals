//! Tests for the checked-exception analysis: a checked exception a
//! method / constructor can raise that is neither declared in its `throws` clause nor caught by an
//! enclosing `try` / `catch`. All cases build a single-file project index with the stdlib stubs (the
//! `Throwable` hierarchy the classifier needs).

use jals_hir::{FileAnalysis, FileId, ProjectIndex, UnreportedException};

/// The simple names of the exceptions reported unreported in `src`, index built over the whole file.
fn reported(src: &str) -> Vec<String> {
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(src)).syntax();
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), root.clone())])
            .with_stdlib()
            .build(),
    );
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    jals_exec::block_on_inline(
        analysis
            .in_project(&index, FileId(0))
            .unreported_exceptions(),
    )
    .into_iter()
    .map(|e| e.name)
    .collect()
}

/// A user-defined checked exception plus a class `C` holding `body` as the single method `f`.
fn with_checked(body: &str) -> String {
    format!("class MyEx extends Exception {{}} class C {{ void f() {{ {body} }} }}")
}

#[test]
fn throwing_an_undeclared_checked_exception_is_reported() {
    assert_eq!(reported(&with_checked("throw new MyEx();")), ["MyEx"]);
}

#[test]
fn declaring_the_exception_silences_it() {
    let src =
        "class MyEx extends Exception {} class C { void f() throws MyEx { throw new MyEx(); } }";
    assert!(reported(src).is_empty());
}

#[test]
fn a_supertype_in_the_throws_clause_covers_it() {
    let src = "class MyEx extends Exception {} class C { void f() throws Exception { throw new MyEx(); } }";
    assert!(reported(src).is_empty());
}

#[test]
fn catching_it_exactly_silences_it() {
    assert!(reported(&with_checked("try { throw new MyEx(); } catch (MyEx e) {}")).is_empty());
}

#[test]
fn catching_a_supertype_silences_it() {
    assert!(
        reported(&with_checked(
            "try { throw new MyEx(); } catch (Exception e) {}"
        ))
        .is_empty()
    );
    assert!(
        reported(&with_checked(
            "try { throw new MyEx(); } catch (Throwable t) {}"
        ))
        .is_empty()
    );
}

#[test]
fn a_non_covering_catch_still_reports() {
    // `RuntimeException` is not a supertype of the checked `MyEx`, so it does not catch it.
    assert_eq!(
        reported(&with_checked(
            "try { throw new MyEx(); } catch (RuntimeException e) {}"
        )),
        ["MyEx"]
    );
}

#[test]
fn a_multi_catch_arm_that_covers_it_silences_it() {
    assert!(
        reported(&with_checked(
            "try { throw new MyEx(); } catch (RuntimeException | MyEx e) {}"
        ))
        .is_empty()
    );
}

#[test]
fn an_outer_try_catches_a_nested_throw() {
    assert!(
        reported(&with_checked(
            "try { try { throw new MyEx(); } finally {} } catch (MyEx e) {}"
        ))
        .is_empty()
    );
}

#[test]
fn a_throw_in_a_finally_is_not_caught_by_that_try() {
    // The `finally` block is not protected by its own `try`'s catches.
    assert_eq!(
        reported(&with_checked(
            "try {} catch (MyEx e) {} finally { throw new MyEx(); }"
        )),
        ["MyEx"]
    );
}

#[test]
fn rethrowing_from_a_catch_is_reported() {
    // The rethrow is in the catch block, not the guarded region, so it escapes `f`.
    assert_eq!(
        reported(&with_checked(
            "try { throw new MyEx(); } catch (MyEx e) { throw e; }"
        )),
        ["MyEx"]
    );
}

#[test]
fn an_unchecked_throw_is_never_reported() {
    assert!(reported(&with_checked("throw new IllegalStateException();")).is_empty());
    assert!(reported(&with_checked("throw new NullPointerException();")).is_empty());
    assert!(reported(&with_checked("throw new RuntimeException();")).is_empty());
}

#[test]
fn a_stdlib_checked_exception_is_reported() {
    let src = "class C { void f() { throw new java.io.IOException(); } }";
    assert_eq!(reported(src), ["IOException"]);
}

#[test]
fn calling_a_method_that_throws_propagates_the_exception() {
    let src = "class MyEx extends Exception {} \
               class C { void a() throws MyEx {} void b() { a(); } }";
    assert_eq!(reported(src), ["MyEx"]);
}

#[test]
fn a_declaring_caller_of_a_throwing_method_is_silent() {
    let src = "class MyEx extends Exception {} \
               class C { void a() throws MyEx {} void b() throws MyEx { a(); } }";
    assert!(reported(src).is_empty());
}

#[test]
fn a_constructor_that_throws_propagates_when_used() {
    let src = "class MyEx extends Exception {} \
               class R { R() throws MyEx {} } \
               class C { void f() { new R(); } }";
    assert_eq!(reported(src), ["MyEx"]);
}

#[test]
fn an_exception_with_an_unindexed_supertype_is_not_classified() {
    // `MyEx`'s chain reaches an un-indexed `Unknown` type, so it cannot be proven checked → skipped.
    let src = "class MyEx extends Unknown {} class C { void f() { throw new MyEx(); } }";
    assert!(reported(src).is_empty());
}

#[test]
fn a_throw_inside_a_lambda_is_not_attributed_to_the_method() {
    // A lambda's thrown exceptions are governed by its target type, not `f`, so they are left alone.
    let src = "class MyEx extends Exception {} \
               interface Task { void run(); } \
               class C { void f() { Task t = () -> { throw new MyEx(); }; } }";
    assert!(reported(src).is_empty());
}

#[test]
fn without_the_stdlib_stubs_nothing_is_reported() {
    // The classifier needs `Throwable` / `RuntimeException` / `Error` to partition the hierarchy.
    // Without them nothing can be *proven* checked, so the analysis reports nothing rather than
    // guessing — the same conservative answer it gives for an unindexable supertype chain.
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(&with_checked(
        "throw new MyEx();",
    )))
    .syntax();
    let index =
        jals_exec::block_on_inline(ProjectIndex::builder(&[(FileId(0), root.clone())]).build());
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    assert!(
        jals_exec::block_on_inline(
            analysis
                .in_project(&index, FileId(0))
                .unreported_exceptions()
        )
        .is_empty()
    );
}

#[test]
fn the_finding_names_the_exception() {
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(&with_checked(
        "throw new MyEx();",
    )))
    .syntax();
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), root.clone())])
            .with_stdlib()
            .build(),
    );
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    let found: Vec<UnreportedException> = jals_exec::block_on_inline(
        analysis
            .in_project(&index, FileId(0))
            .unreported_exceptions(),
    );
    assert_eq!(found.len(), 1);
    // The wording is `jals-lint`'s (`unreported-exception`); what this crate owes is the name.
    assert_eq!(found[0].name, "MyEx");
}

#[test]
fn a_reflective_checked_exception_is_reported() {
    // `ReflectiveOperationException` and its subclasses are checked, so a file that names one is
    // required to handle it. Classifying them needs the stubs to model the hierarchy, not just the
    // leaves: a `catch` of one subclass does not admit the supertype a callee declares.
    let src = "class C {
        void g() throws ReflectiveOperationException {}
        void f() { try { g(); } catch (ClassNotFoundException e) {} }
        void h() { throw new NoSuchMethodException(); }
    }";
    let mut found = reported(src);
    found.sort();
    assert_eq!(
        found,
        ["NoSuchMethodException", "ReflectiveOperationException"]
    );
}

#[test]
fn catching_the_reflective_supertype_admits_a_subclass() {
    // Why the common supertype is spelled out rather than flattened onto `Exception`: one
    // `catch (ReflectiveOperationException e)` is what the whole family binds through.
    let src = "class C { void f() { try { throw new ClassNotFoundException(); } catch (ReflectiveOperationException e) {} } }";
    assert!(reported(src).is_empty(), "{:?}", reported(src));
}
