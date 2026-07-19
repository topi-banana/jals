//! Tests for constant `if` condition detection (`jals_hir::DeadIf::collect`): literal and operator
//! folding, `final` constant-variable propagation, and the conservative bails.

use jals_hir::{DeadIf, Resolved};

fn dead(src: &str) -> Vec<DeadIf> {
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(src)).syntax();
    let resolved = jals_exec::block_on_inline(Resolved::resolve_node(&root));
    jals_exec::block_on_inline(DeadIf::collect(&root, &resolved))
}

/// Wraps a statement body in a method so it parses as a valid local context.
fn in_method(body: &str) -> String {
    format!("class C {{ void m() {{ {body} }} }}")
}

/// The single report expected for `body`, with its constant value.
fn one(body: &str) -> DeadIf {
    let src = in_method(body);
    let found = dead(&src);
    assert_eq!(found.len(), 1, "expected one report in `{body}`: {found:?}");
    found.into_iter().next().unwrap()
}

fn none(body: &str) {
    let src = in_method(body);
    let found = dead(&src);
    assert!(
        found.is_empty(),
        "expected no report in `{body}`: {found:?}"
    );
}

#[test]
fn boolean_literal_conditions_are_flagged() {
    assert!(one("if (true) {}").value);
    assert!(!one("if (false) { x(); }").value);
}

#[test]
fn always_true_without_else_has_no_dead_range() {
    let d = one("if (true) { a(); }");
    assert!(d.value);
    assert_eq!(d.dead_range, None);
}

#[test]
fn always_false_marks_the_then_branch_dead() {
    let src = in_method("if (false) { x(); }");
    let found = dead(&src);
    let then_start = src.find("{ x(); }").unwrap();
    assert_eq!(
        found[0].dead_range,
        Some(then_start..then_start + "{ x(); }".len())
    );
}

#[test]
fn always_true_marks_the_else_branch_dead() {
    let src = in_method("if (true) { a(); } else { b(); }");
    let found = dead(&src);
    let else_start = src.find("{ b(); }").unwrap();
    assert_eq!(
        found[0].dead_range,
        Some(else_start..else_start + "{ b(); }".len())
    );
    let cond_start = src.find("true").unwrap();
    assert_eq!(found[0].condition_range, cond_start..cond_start + 4);
}

#[test]
fn dead_else_if_chain_is_covered_wholly() {
    let src = in_method("if (true) { a(); } else if (c) { b(); } else { d(); }");
    let found = dead(&src);
    assert_eq!(found.len(), 1);
    // The else branch is itself an `if` statement; the dead range covers all of it.
    let chain = "if (c) { b(); } else { d(); }";
    let start = src.find(chain).unwrap();
    assert_eq!(found[0].dead_range, Some(start..start + chain.len()));
}

#[test]
fn single_statement_branch_is_the_dead_range() {
    let src = in_method("if (false) x();");
    let found = dead(&src);
    let start = src.find("x();").unwrap();
    assert_eq!(found[0].dead_range, Some(start..start + "x();".len()));
}

#[test]
fn parens_and_negation_fold() {
    assert!(one("if ((true)) {}").value);
    assert!(one("if (!false) {}").value);
    assert!(one("if (!(1 > 2)) {}").value);
    let d = one("if ((true)) {}");
    // The condition range is the whole parenthesized expression.
    let src = in_method("if ((true)) {}");
    let start = src.find("(true)").unwrap();
    assert_eq!(d.condition_range, start..start + "(true)".len());
}

#[test]
fn short_circuit_folds_by_three_valued_logic() {
    assert!(!one("if (x && false) {}").value);
    assert!(!one("if (false && f()) {}").value);
    assert!(one("if (x || true) {}").value);
    assert!(one("if (true || f()) {}").value);
    assert!(one("if (true && true) {}").value);
    assert!(!one("if (false || false) {}").value);
    none("if (x && true) {}");
    none("if (x || false) {}");
}

#[test]
fn integer_comparisons_fold() {
    assert!(one("if (1 == 1) {}").value);
    assert!(!one("if (1 != 1) {}").value);
    assert!(one("if (1 < 2) {}").value);
    assert!(one("if (2 <= 2) {}").value);
    assert!(!one("if (1 > 2) {}").value);
    assert!(one("if (2 >= 2) {}").value); // `>=` is the two-token `GT EQ`
    assert!(one("if (-1 < 0) {}").value);
    assert!(one("if (true == true) {}").value);
    assert!(!one("if (true != true) {}").value);
}

#[test]
fn java_literal_shapes_fold_with_java_semantics() {
    assert!(one("if (0xFF == 255) {}").value);
    assert!(one("if (0xFFFFFFFF == -1) {}").value); // 32-bit sign extension, as in Java
    assert!(one("if (1_000 == 1000) {}").value);
    assert!(one("if (1L == 1) {}").value);
    assert!(one("if (0b1010 == 10) {}").value);
    assert!(one("if (010 == 8) {}").value);
}

#[test]
fn final_local_propagates() {
    assert!(!one("final boolean DEBUG = false; if (DEBUG) { log(); }").value);
    assert!(one("final int LIMIT = 0; if (LIMIT >= 0) {}").value);
    assert!(one("final var x = true; if (x) {}").value);
}

#[test]
fn final_field_propagates() {
    let found = dead("class C { static final boolean DEBUG = false; void m() { if (DEBUG) {} } }");
    assert_eq!(found.len(), 1);
    assert!(!found[0].value);
    let found = dead("class C { final boolean on = true; void m() { if (on) {} } }");
    assert_eq!(found.len(), 1);
    assert!(found[0].value);
}

#[test]
fn final_chains_propagate() {
    assert!(!one("final boolean a = true; final boolean b = !a; if (b) {}").value);
}

#[test]
fn multi_declarator_binds_the_right_initializer() {
    assert!(!one("final boolean a = true, b = false; if (b) {}").value);
    none("final int a, b = 1; if (a > 0) {}");
}

#[test]
fn non_constant_names_do_not_fold() {
    none("boolean debug = false; if (debug) {}"); // not final
    none("final boolean debug = f(); if (debug) {}"); // initializer does not fold
    none("final boolean debug; if (debug) {}"); // no initializer
    none("if (undefined) {}"); // unresolved
    let found = dead("class C { void m(boolean flag) { if (flag) {} } }");
    assert!(found.is_empty()); // parameter
    let found = dead("class C { boolean debug = false; void m() { if (this.debug) {} } }");
    assert!(found.is_empty()); // member access never folds
}

#[test]
fn shadowing_resolves_to_the_inner_binding() {
    // The non-final local shadows the constant field, so nothing folds.
    let found = dead(
        "class C { static final boolean X = true; void m(boolean p) { boolean X = p; if (X) {} } }",
    );
    assert!(found.is_empty());
}

#[test]
fn side_effects_and_unsupported_forms_bail() {
    none("boolean b; if (b = true) {}"); // assignment
    none("if (f()) {}");
    none("if (1 == true) {}"); // ill-typed mixed comparison
    none("if (x ? true : true) {}"); // ternaries are out of scope
    none("if (1 + 1 == 2) {}"); // arithmetic is out of scope
    none("if (true & true) {}"); // non-short-circuit `&` is out of scope
}

#[test]
fn cyclic_initializers_terminate_without_reports() {
    let found = dead(
        "class C { static final boolean A = B; static final boolean B = A; void m() { if (A) {} } }",
    );
    assert!(found.is_empty());
}

#[test]
fn only_if_statements_are_examined() {
    none("while (true) { work(); }");
    none("do { work(); } while (false);");
    none("for (;;) {}");
    none("int x = true ? 1 : 2;");
}

#[test]
fn nested_constant_ifs_are_each_reported() {
    let found = dead(&in_method("if (true) { if (false) { x(); } }"));
    assert_eq!(found.len(), 2);
    assert!(found[0].value);
    assert!(!found[1].value);
}

#[test]
fn broken_sources_never_panic() {
    for src in [
        "class C { void m() { if ( } }",
        "class C { void m() { if () {} } }",
        "class C { void m() { if (true } }",
        "class C { void m() { if (true) } }",
        "if (true",
        "",
    ] {
        let _ = dead(src);
    }
}
