//! Type-inference tests: the inferred type of definitions and expressions.
//!
//! Targeted assertions pin individual rules (literals, numeric promotion, `var`, …); the snapshot
//! tests render every value definition and every expression of a fixture, one per line, so the
//! whole bottom-up result is visible at a glance.

use expect_test::{Expect, expect};
use jals_hir::{
    FileId, Namespace, ProjectIndex, Resolved, Ty, TypeInference, infer, infer_node, resolve_node,
};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{self, AstNode};

/// Parses `src`, keeping its `SOURCE_FILE` node alive (rowan nodes are ref-counted).
fn parse(src: &str) -> SyntaxNode {
    jals_syntax::parse(src).syntax()
}

/// Infers a single-file project (so reference type names can resolve to project items).
fn analyse(src: &str) -> (SyntaxNode, Resolved, TypeInference) {
    let node = parse(src);
    let resolved = resolve_node(&node);
    let index = ProjectIndex::build(&[(FileId(0), node.clone())]);
    let ti = infer(&node, &resolved, &index, FileId(0));
    (node, resolved, ti)
}

/// The inferred type of the first definition named `name`.
fn def_ty(src: &str, name: &str) -> String {
    let (_, resolved, ti) = analyse(src);
    let def = resolved
        .defs
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no definition named `{name}`"));
    ti.type_of_def(def.id).to_string()
}

/// The inferred type of the (first) expression whose source text is exactly `text`.
fn expr_ty(src: &str, text: &str) -> String {
    let (node, _, ti) = analyse(src);
    let expr = node
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|e| e.syntax().text().to_string().trim() == text)
        .unwrap_or_else(|| panic!("no expression `{text}`"));
    let r = expr.syntax().text_range();
    ti.type_of_expr(usize::from(r.start())..usize::from(r.end()))
        .unwrap()
        .to_string()
}

// --- Literals -------------------------------------------------------------------------------------

#[test]
fn literals_have_their_primitive_type() {
    let src = "class C { void m() { var a = 1; var b = 1L; var c = 1.5; var d = 1.5f; var e = 'x'; var f = true; var g = \"s\"; var h = null; } }";
    assert_eq!(def_ty(src, "a"), "int");
    assert_eq!(def_ty(src, "b"), "long");
    assert_eq!(def_ty(src, "c"), "double");
    assert_eq!(def_ty(src, "d"), "float");
    assert_eq!(def_ty(src, "e"), "char");
    assert_eq!(def_ty(src, "f"), "boolean");
    assert_eq!(def_ty(src, "g"), "String");
    assert_eq!(def_ty(src, "h"), "null");
}

// --- Operators ------------------------------------------------------------------------------------

#[test]
fn arithmetic_promotes_sub_int_operands_to_int() {
    // The classic surprise: byte + byte is int, not byte.
    let src = "class C { void m() { byte x = 1; var r = x + x; } }";
    assert_eq!(expr_ty(src, "x + x"), "int");
}

#[test]
fn arithmetic_widens_to_the_larger_operand() {
    let src = "class C { void m() { int i = 1; double d = 1.0; var r = i + d; } }";
    assert_eq!(expr_ty(src, "i + d"), "double");
}

#[test]
fn string_plus_anything_is_string() {
    let src = "class C { void m() { var r = \"n=\" + 1; } }";
    assert_eq!(expr_ty(src, "\"n=\" + 1"), "String");
}

#[test]
fn comparisons_and_logical_are_boolean() {
    let src = "class C { void m() { int a = 1; var lt = a < a; var ge = a >= a; var eq = a == a; var sh = a >> a; } }";
    assert_eq!(expr_ty(src, "a < a"), "boolean");
    assert_eq!(expr_ty(src, "a >= a"), "boolean");
    assert_eq!(expr_ty(src, "a == a"), "boolean");
    // A shift, by contrast, is numeric (the promoted left operand).
    assert_eq!(expr_ty(src, "a >> a"), "int");
}

#[test]
fn negation_is_boolean() {
    let src = "class C { void m() { boolean b = true; var r = !b; } }";
    assert_eq!(expr_ty(src, "!b"), "boolean");
}

// --- Names, casts, new, arrays --------------------------------------------------------------------

#[test]
fn name_reference_has_its_declarations_type() {
    let src = "class C { void m(long p) { var r = p; } }";
    assert_eq!(expr_ty(src, "p"), "long");
}

#[test]
fn cast_has_the_cast_target_type() {
    let src = "class C { void m(Object o) { var r = (int) o; } }";
    assert_eq!(expr_ty(src, "(int) o"), "int");
}

#[test]
fn new_of_a_project_type_resolves_to_it() {
    let src = "class C { void m() { var r = new Helper(); } } class Helper { }";
    assert_eq!(expr_ty(src, "new Helper()"), "Helper");
    assert_eq!(def_ty(src, "r"), "Helper");
}

#[test]
fn new_array_is_an_array_type() {
    let src = "class C { void m() { var r = new int[3]; } }";
    assert_eq!(expr_ty(src, "new int[3]"), "int[]");
}

#[test]
fn array_field_and_index_peel_one_dimension() {
    let src = "class C { int[] xs; void m() { var r = xs; } }";
    assert_eq!(def_ty(src, "xs"), "int[]");
    let indexed = "class C { void m(int[] xs) { var r = xs[0]; } }";
    assert_eq!(expr_ty(indexed, "xs[0]"), "int");
}

// --- var and forward references -------------------------------------------------------------------

#[test]
fn var_local_takes_its_initializer_type() {
    let src = "class C { void m() { var n = 1 + 2; var s = \"a\" + n; } }";
    assert_eq!(def_ty(src, "n"), "int");
    assert_eq!(def_ty(src, "s"), "String");
}

#[test]
fn field_type_is_visible_to_an_earlier_method() {
    // A method body before the field declaration still sees the field's (explicit) type.
    let src = "class C { void m() { var r = field; } int field; }";
    assert_eq!(expr_ty(src, "field"), "int");
}

// --- Deferred forms -------------------------------------------------------------------------------

#[test]
fn member_dependent_forms_are_unknown_for_now() {
    // Method calls and field access need member resolution (a later phase): no type yet.
    let calls = "class C { void m() { var r = compute(); } int compute() { return 0; } }";
    assert_eq!(expr_ty(calls, "compute()"), "?");
    let access = "class C { void m(java.util.List xs) { var r = xs.size; } }";
    assert_eq!(expr_ty(access, "xs.size"), "?");
}

// --- Project vs. project-free resolution ----------------------------------------------------------

#[test]
fn project_free_inference_names_reference_types_externally() {
    // infer_node has no index, so a sibling type is known only by spelling — but structural
    // inference (the `int`, the `var`) still works.
    let node = parse("class C { void m() { Helper h = make(); var n = 1; } } class Helper { }");
    let resolved = resolve_node(&node);
    let ti = infer_node(&node, &resolved);
    let helper = resolved.defs.iter().find(|d| d.name == "h").unwrap();
    let n = resolved.defs.iter().find(|d| d.name == "n").unwrap();
    assert_eq!(ti.type_of_def(helper.id).to_string(), "Helper");
    assert_eq!(ti.type_of_def(n.id).to_string(), "int");
}

// --- Snapshots ------------------------------------------------------------------------------------

fn render(src: &str) -> String {
    let (node, resolved, ti) = analyse(src);
    let mut out = String::from("defs:\n");
    for d in &resolved.defs {
        if d.kind.namespace() != Namespace::Value {
            continue;
        }
        out.push_str(&format!(
            "  {:?} {}: {}\n",
            d.kind,
            d.name,
            ti.type_of_def(d.id)
        ));
    }
    out.push_str("exprs:\n");
    for e in node.descendants().filter_map(ast::Expr::cast) {
        let r = e.syntax().text_range();
        let ty = ti
            .type_of_expr(usize::from(r.start())..usize::from(r.end()))
            .cloned()
            .unwrap_or(Ty::Unknown);
        let text = e.syntax().text().to_string().trim().replace('\n', " ");
        out.push_str(&format!("  {text}: {ty}\n"));
    }
    out
}

fn check(src: &str, expected: Expect) {
    expected.assert_eq(&render(src));
}

#[test]
fn snapshot_mixed_expression() {
    check(
        "class C { void m(int a, double b) { var r = a * b + 1; } }",
        expect![[r#"
            defs:
              Param a: int
              Param b: double
              Local r: double
            exprs:
              a * b + 1: double
              a * b: double
              a: int
              b: double
              1: int
        "#]],
    );
}

#[test]
fn snapshot_new_and_array() {
    check(
        "class C { void m() { Helper h = new Helper(); var xs = new int[2]; } } class Helper { }",
        expect![[r#"
            defs:
              Local h: Helper
              Local xs: int[]
            exprs:
              new Helper(): Helper
              new int[2]: int[]
              2: int
        "#]],
    );
}
