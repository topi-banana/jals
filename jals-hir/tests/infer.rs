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
    let index = ProjectIndex::builder(&[(FileId(0), node.clone())]).build();
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
fn new_and_cast_carry_type_arguments() {
    // An external generic type: both `ArrayList` and `String` are unindexed, rendered by spelling.
    let ext = "class C { void m() { var r = new ArrayList<String>(); } }";
    assert_eq!(expr_ty(ext, "new ArrayList<String>()"), "ArrayList<String>");

    // A project generic type keeps its argument too; `Box` and `Helper` both resolve to project items.
    let proj =
        "class Box<T> { } class C { void m() { var r = new Box<Helper>(); } } class Helper { }";
    assert_eq!(expr_ty(proj, "new Box<Helper>()"), "Box<Helper>");

    // A cast target's type arguments are carried through.
    let cast = "class C { void m(Object o) { var r = (List<String>) o; } }";
    assert_eq!(expr_ty(cast, "(List<String>) o"), "List<String>");

    // A bare wildcard argument (`<?>`) is a token, not a nameable type node, so it is not carried —
    // the type degrades to its raw spelling rather than failing. Wildcards are modelled in a later
    // phase (generic subtyping).
    let wild = "class C { void m(Object o) { var r = (List<?>) o; } }";
    assert_eq!(expr_ty(wild, "(List<?>) o"), "List");
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

// --- Member access (fields and method calls) ------------------------------------------------------

#[test]
fn field_access_resolves_to_the_field_type() {
    let src = "class Box { int size; String label; } class C { void m(Box b) { var a = b.size; var s = b.label; } }";
    assert_eq!(expr_ty(src, "b.size"), "int");
    assert_eq!(expr_ty(src, "b.label"), "String");
}

#[test]
fn field_access_carries_type_arguments() {
    // A concrete argument flows through the member's declared type: `xs : List<String>`.
    let src = "class Box { List<String> xs; } class C { void m(Box b) { var r = b.xs; } }";
    assert_eq!(expr_ty(src, "b.xs"), "List<String>");

    // A type-variable argument is carried by spelling (`E`); binding it to the receiver's actual
    // argument is the substitution phase, not yet done — so it shows as the declared `List<E>`.
    let generic = "class Box<E> { List<E> xs; } class C { void m(Box b) { var r = b.xs; } }";
    assert_eq!(expr_ty(generic, "b.xs"), "List<E>");
}

#[test]
fn method_call_resolves_to_the_return_type() {
    let src = "class Box { int area() { return 0; } Box grow() { return this; } } class C { void m(Box b) { var n = b.area(); var g = b.grow(); } }";
    assert_eq!(expr_ty(src, "b.area()"), "int");
    assert_eq!(expr_ty(src, "b.grow()"), "Box");
}

#[test]
fn bare_method_call_resolves_on_the_enclosing_type() {
    let src = "class C { int compute() { return 0; } void m() { var r = compute(); } }";
    assert_eq!(expr_ty(src, "compute()"), "int");
}

#[test]
fn generic_member_access_substitutes_type_arguments() {
    // A direct type-variable member binds to the receiver's argument: `Box<String>.get() : String`.
    let direct = "class Box<E> { E get() { return null; } E item; } \
                  class C { void m(Box<String> b) { var g = b.get(); var f = b.item; } }";
    assert_eq!(expr_ty(direct, "b.get()"), "String");
    assert_eq!(expr_ty(direct, "b.item"), "String");

    // Substitution recurses into a nested generic: a `List<E>` field becomes `List<String>`.
    let nested = "class Box<E> { List<E> xs; } \
                  class C { void m(Box<String> b) { var r = b.xs; } }";
    assert_eq!(expr_ty(nested, "b.xs"), "List<String>");

    // A raw receiver leaves the type variable un-substituted (it survives by name).
    let raw = "class Box<E> { E get() { return null; } } \
               class C { void m(Box b) { var r = b.get(); } }";
    assert_eq!(expr_ty(raw, "b.get()"), "E");
}

#[test]
fn inherited_generic_member_substitutes_through_the_chain() {
    // A concrete supertype argument binds the inherited member: `Sub extends Base<String>`.
    let concrete = "class Base<T> { T get() { return null; } } \
                    class Sub extends Base<String> { } \
                    class C { void m(Sub s) { var r = s.get(); } }";
    assert_eq!(expr_ty(concrete, "s.get()"), "String");

    // The receiver's own argument threads through to the supertype: `Sub<U> extends Base<U>`.
    let threaded = "class Base<T> { T get() { return null; } } \
                    class Sub<U> extends Base<U> { } \
                    class C { void m(Sub<String> s) { var r = s.get(); } }";
    assert_eq!(expr_ty(threaded, "s.get()"), "String");
}

#[test]
fn inherited_member_is_accessible() {
    let src = "class Base { int shared() { return 0; } } class Sub extends Base { } class C { void m(Sub s) { var r = s.shared(); } }";
    assert_eq!(expr_ty(src, "s.shared()"), "int");
}

#[test]
fn member_access_chains_through_inferred_types() {
    let src = "class Inner { int leaf; } class Outer { Inner inner() { return null; } } class C { void m(Outer o) { var r = o.inner().leaf; } }";
    assert_eq!(expr_ty(src, "o.inner().leaf"), "int");
}

#[test]
fn var_local_takes_a_member_type() {
    let src = "class Box { long id; } class C { void m(Box b) { var v = b.id; } }";
    assert_eq!(def_ty(src, "v"), "long");
}

#[test]
fn an_external_receivers_members_are_unknown() {
    // `xs` is `java.util.List` (external, unindexed): its members are not resolved.
    let access = "class C { void m(java.util.List xs) { var r = xs.size; } }";
    assert_eq!(expr_ty(access, "xs.size"), "?");
}

#[test]
fn a_missing_member_on_a_project_type_is_unknown() {
    let src = "class Box { int size; } class C { void m(Box b) { var r = b.nope; } }";
    assert_eq!(expr_ty(src, "b.nope"), "?");
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
