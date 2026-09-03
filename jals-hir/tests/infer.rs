//! Type-inference tests: the inferred type of definitions and expressions.
//!
//! Targeted assertions pin individual rules (literals, numeric promotion, `var`, …); the snapshot
//! tests render every value definition and every expression of a fixture, one per line, so the
//! whole bottom-up result is visible at a glance.

use core::fmt::Write;

use expect_test::{Expect, expect};
use jals_hir::{FileAnalysis, FileId, FileSemantics, Namespace, ProjectIndex, Ty, TypedFile};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{self, AstNode};

/// Parses `src`, keeping its `SOURCE_FILE` node alive (rowan nodes are ref-counted).
fn parse(src: &str) -> SyntaxNode {
    jals_exec::block_on_inline(jals_syntax::Parse::parse(src)).syntax()
}

/// A single-file project, holding what a [`TypedFile`] borrows.
///
/// The analysis and the index are owned here because a binding — and the type witness it hands
/// out — borrow both; a helper returning one directly would be returning a borrow of its own
/// locals.
struct Fixture {
    node: SyntaxNode,
    analysis: FileAnalysis,
    index: ProjectIndex,
}

impl Fixture {
    /// Analyse `src` as file 0 of a project containing only it (so reference type names can
    /// resolve to project items).
    fn new(src: &str) -> Self {
        let node = parse(src);
        let analysis = jals_exec::block_on_inline(FileAnalysis::of(&node));
        let index =
            jals_exec::block_on_inline(ProjectIndex::builder(&[(FileId(0), node.clone())]).build());
        Self {
            node,
            analysis,
            index,
        }
    }

    /// This file bound to its project. The caller keeps the binding: the type witness borrows its
    /// memo cell, so the binding has to outlive it.
    const fn semantics(&self) -> FileSemantics<'_> {
        self.analysis.in_project(&self.index, FileId(0))
    }
}

/// The inferred type of the first definition named `name`.
fn def_ty(src: &str, name: &str) -> String {
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    let def = typed
        .analysis()
        .defs()
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no definition named `{name}`"));
    typed.type_of_def(def.id).to_string()
}

/// The recorded type of the expression node `n` — the `type_of_expr` lookup keyed by `n`'s span.
fn type_at<'t>(typed: TypedFile<'t>, n: &SyntaxNode) -> Option<&'t Ty> {
    let r = n.text_range();
    typed.type_of_expr(usize::from(r.start())..usize::from(r.end()))
}

/// The inferred type of the (first) expression whose source text is exactly `text`.
fn expr_ty(src: &str, text: &str) -> String {
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    let expr = fixture
        .node
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|e| e.syntax().text().to_string().trim() == text)
        .unwrap_or_else(|| panic!("no expression `{text}`"));
    type_at(typed, expr.syntax()).unwrap().to_string()
}

/// The inferred types of every switch *expression* in `src`, in source (pre-order) order — the
/// outer switch first, then any nested ones — so one call checks every switch of a fixture
/// without repeating each switch's full text as an `expr_ty` match.
fn switch_tys(src: &str) -> Vec<String> {
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    fixture
        .node
        .descendants()
        .filter(|n| n.kind() == jals_syntax::SyntaxKind::SWITCH_EXPR)
        .map(|n| {
            type_at(typed, &n)
                .cloned()
                .unwrap_or(Ty::Unknown)
                .to_string()
        })
        .collect()
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

// --- Switch expressions ---------------------------------------------------------------------------

#[test]
fn switch_arrow_arms_of_one_type_infer_that_type() {
    let src = "class C { int m(int x) { return switch (x) { case 1 -> 10; default -> 20; }; } }";
    assert_eq!(switch_tys(src), ["int"]);
}

#[test]
fn switch_arms_of_different_types_are_unknown() {
    // Exact-equality join (like the ternary): a mismatch is `Unknown`, not a common supertype.
    let src =
        "class C { Object m(int x) { return switch (x) { case 1 -> 10; default -> \"s\"; }; } }";
    assert_eq!(switch_tys(src), ["?"]);
}

#[test]
fn switch_arrow_block_yields_are_the_arm_value() {
    let src = "class C { int m(int x) { \
               return switch (x) { case 1 -> { yield 10; } default -> { yield 20; } }; } }";
    assert_eq!(switch_tys(src), ["int"]);
}

#[test]
fn switch_colon_group_yields_are_the_arm_value() {
    let src = "class C { int m(int x) { \
               return switch (x) { case 1: yield 10; default: yield 20; }; } }";
    assert_eq!(switch_tys(src), ["int"]);
}

#[test]
fn switch_throw_arm_produces_no_value() {
    // A `throw` arm never completes normally, so it does not constrain the switch's type.
    let src = "class C { int m(int x) { \
               return switch (x) { case 1 -> 10; default -> throw new RuntimeException(); }; } }";
    assert_eq!(switch_tys(src), ["int"]);
}

#[test]
fn switch_of_a_project_type_resolves_to_it() {
    let src = "class Foo { } class C { Foo m(int x) { \
               return switch (x) { case 1 -> new Foo(); default -> new Foo(); }; } }";
    assert_eq!(switch_tys(src), ["Foo"]);
}

#[test]
fn nested_switch_yields_are_attributed_to_their_own_switch() {
    // The inner switch yields `String`; the outer must not be polluted by it and stays `int`.
    let src = "class C { int m(int x, int y) { \
               return switch (x) { \
                   case 1 -> { \
                       String s = switch (y) { case 2 -> \"a\"; default -> \"b\"; }; \
                       yield 10; \
                   } \
                   default -> 20; \
               }; } }";
    // Pre-order: the outer switch first, then the inner one.
    assert_eq!(switch_tys(src), ["int", "String"]);
}

// --- Snapshots ------------------------------------------------------------------------------------

fn render(src: &str) -> String {
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    let mut out = String::from("defs:\n");
    for d in typed.analysis().defs() {
        if d.kind.namespace() != Namespace::Value {
            continue;
        }
        writeln!(
            out,
            "  {:?} {}: {}",
            d.kind,
            d.name,
            typed.type_of_def(d.id)
        )
        .unwrap();
    }
    out.push_str("exprs:\n");
    for e in fixture.node.descendants().filter_map(ast::Expr::cast) {
        let ty = type_at(typed, e.syntax()).cloned().unwrap_or(Ty::Unknown);
        let text = e.syntax().text().to_string().trim().replace('\n', " ");
        writeln!(out, "  {text}: {ty}").unwrap();
    }
    out
}

#[allow(clippy::needless_pass_by_value)]
fn check(src: &str, expected: Expect) {
    expected.assert_eq(&render(src));
}

#[test]
fn snapshot_mixed_expression() {
    check(
        "class C { void m(int a, double b) { var r = a * b + 1; } }",
        expect![[r"
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
        "]],
    );
}

#[test]
fn snapshot_new_and_array() {
    check(
        "class C { void m() { Helper h = new Helper(); var xs = new int[2]; } } class Helper { }",
        expect![[r"
            defs:
              Local h: Helper
              Local xs: int[]
            exprs:
              new Helper(): Helper
              new int[2]: int[]
              2: int
        "]],
    );
}

#[test]
fn snapshot_switch_expression() {
    check(
        "class C { void m(int x) { var r = switch (x) { case 1 -> 10; default -> 20; }; } }",
        expect![[r"
            defs:
              Param x: int
              Local r: int
            exprs:
              switch (x) { case 1 -> 10; default -> 20; }: int
              x: int
              1: int
              10: int
              20: int
        "]],
    );
}

// --- Member lookup on a receiver that declares no members ------------------------------------

/// JLS §4.4: a type variable's members are its *bound's*.
///
/// `<T extends CharSequence> int len(T t) { return t.length(); }` is ordinary Java. A lookup on the
/// variable itself finds nothing at all, which is the largest share of "did not resolve to an
/// indexed member".
#[test]
fn a_type_variables_members_are_its_bounds() {
    let src = "class Box { \
                 interface Seq { int size(); } \
                 static <T extends Seq> int len(T t) { return t.size(); } \
                 static <U> int any(U u) { return 0; } \
               }";
    assert_eq!(expr_ty(src, "t.size()"), "int");
    // An unbounded variable is `Object`'s member set, and nothing in this project declares `size`
    // there, so the lookup answers nothing rather than the bound's answer.
    let src = "class Box { static <U> int len(U u) { return u.size(); } }";
    assert_eq!(expr_ty(src, "u.size()"), "?");
}

/// JLS §10.7: an array has `Object`'s members, a `length` field, and a `clone()` returning the
/// array type.
///
/// `clone()` is the one no declaration can state — `Object.clone()` returns `Object`, and typing
/// `xs.clone()` that way makes `int[] ys = xs.clone();` a mismatch against a conversion Java does
/// not require.
#[test]
fn an_arrays_clone_returns_the_array_type() {
    let src = "class A { int[] copy(int[] xs) { return xs.clone(); } int n(int[] xs) { return xs.length; } }";
    assert_eq!(expr_ty(src, "xs.clone()"), "int[]");
    assert_eq!(expr_ty(src, "xs.length"), "int");
    let src = "class A { String[][] copy(String[][] xs) { return xs.clone(); } }";
    assert_eq!(expr_ty(src, "xs.clone()"), "String[][]");
}

/// JLS §7.5.3/§7.5.4: a `static` import binds a bare name to a member of the type it names.
///
/// `import static p.Math2.max;` makes `max(1, 2)` a call, and nothing else in the file says so — a
/// bare call is looked up on the enclosing type, which declares no `max`.
#[test]
fn a_static_import_binds_a_bare_name() {
    let owner = "package p; public class Math2 { public static int max(int a, int b) { return a; } \
                 public static final String NAME = \"m\"; }";
    let single = "import static p.Math2.max; \
                  class Use { int m() { return max(1, 2); } }";
    let on_demand = "import static p.Math2.*; \
                     class Use { int m() { return max(1, 2); } String s() { return NAME; } }";
    let unrelated = "class Use { int m() { return max(1, 2); } }";

    for (src, expected) in [
        (single, "int"),
        (on_demand, "int"),
        // No import, so the enclosing type still answers — and it declares no `max`.
        (unrelated, "?"),
    ] {
        let nodes = [
            (FileId(0), parse(owner)),
            (FileId(1), parse(&alloc_src(src))),
        ];
        let index = jals_exec::block_on_inline(ProjectIndex::builder(&nodes).build());
        let analysis = jals_exec::block_on_inline(FileAnalysis::of(&nodes[1].1));
        let semantics = analysis.in_project(&index, FileId(1));
        let typed = jals_exec::block_on_inline(semantics.typed());
        let call = nodes[1]
            .1
            .descendants()
            .filter_map(ast::Expr::cast)
            .find(|e| e.syntax().text().to_string().trim() == "max(1, 2)")
            .expect("the call");
        assert_eq!(
            type_at(typed, call.syntax()).unwrap().to_string(),
            expected,
            "in: {src}"
        );
    }
}

/// A static-imported *field* takes the same route as a method, after the same implicit `this`.
#[test]
fn a_static_import_binds_a_bare_field() {
    let owner = "package p; public class K { public static final String NAME = \"k\"; }";
    let use_src = "import static p.K.NAME; class Use { String s() { return NAME; } }";
    let nodes = [(FileId(0), parse(owner)), (FileId(1), parse(use_src))];
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&nodes).build());
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&nodes[1].1));
    let semantics = analysis.in_project(&index, FileId(1));
    let typed = jals_exec::block_on_inline(semantics.typed());
    let name = nodes[1]
        .1
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|e| e.syntax().text().to_string().trim() == "NAME")
        .expect("the reference");
    assert_eq!(type_at(typed, name.syntax()).unwrap().to_string(), "String");
}

/// Identity, so the loop above reads as three sources rather than three formats.
fn alloc_src(src: &str) -> String {
    src.to_owned()
}

/// JLS §15.9.1: a qualified `new` looks its type up as a member of the qualifier's type.
///
/// `new Inner2().new InnerMost()` in a class that has both an `Inner1.InnerMost` and an
/// `Inner2.InnerMost` resolved to whichever the ordinary scope rules found first — and a lowering
/// then emitted the *other* class's constructor, with the qualifier beneath it, which no verifier
/// accepts.
#[test]
fn a_qualified_new_resolves_against_its_qualifier() {
    let src = "class Q { \
                 class Inner1 { class Nested { } } \
                 class Inner2 { class Nested { } } \
                 Object a() { return new Inner1().new Nested(); } \
                 Object b() { return new Inner2().new Nested(); } \
               }";
    assert_eq!(expr_ty(src, "new Inner1().new Nested()"), "Nested");
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    // Same simple name, two different items: what the qualifier decides.
    let ids: Vec<String> = ["new Inner1().new Nested()", "new Inner2().new Nested()"]
        .into_iter()
        .map(|text| {
            let expr = fixture
                .node
                .descendants()
                .filter_map(ast::Expr::cast)
                .find(|e| e.syntax().text().to_string().trim() == text)
                .expect("the creation");
            let ty = type_at(typed, expr.syntax()).expect("a type");
            let id = ty.project_id().expect("an indexed type");
            fixture.index.item(id).fqn.to_string()
        })
        .collect();
    assert_eq!(ids, ["Q.Inner1.Nested", "Q.Inner2.Nested"]);
}

// --- Target typing (JLS §15.12.2, §15.16, §15.25) ---------------------------------------------

/// A lambda takes its type from the *context*, and an argument is one of them.
///
/// `call(x -> x + 1)` was the largest single blocker: the lambda had no type, so neither did the
/// call, and a backend was told "the type of a value could not be inferred" for the most ordinary
/// shape in modern Java. The overload is selected from the arguments that are pertinent to
/// applicability (a lambda is not one), and the chosen signature then supplies the type.
#[test]
fn an_argument_is_a_target_type() {
    let src = "class C { \
                 interface Fn { int apply(int n); } \
                 static int call(Fn f) { return f.apply(1); } \
                 static int use() { return call(x -> x + 1); } \
               }";
    assert_eq!(expr_ty(src, "x -> x + 1"), "Fn");
    // And the parameter takes its type from the interface, which is what lets the body infer.
    assert_eq!(def_ty(src, "x"), "int");
}

/// A cast is a target type written outright (JLS §15.16), and a conditional passes its own through
/// to both arms (JLS §15.25).
#[test]
fn a_cast_and_a_conditional_carry_a_target_type() {
    let src = "class C { \
                 interface Fn { int apply(int n); } \
                 static Object cast() { return (Fn) x -> x + 1; } \
                 static Fn arms(boolean b) { return b ? x -> x + 1 : y -> y - 1; } \
               }";
    assert_eq!(expr_ty(src, "x -> x + 1"), "Fn");
    assert_eq!(expr_ty(src, "y -> y - 1"), "Fn");
}

/// The overload is selected before the poly argument is typed, which is the order JLS §15.12.2
/// gives — an argument that is not pertinent to applicability cannot be what selects the method
/// that types it.
#[test]
fn a_poly_argument_does_not_select_the_overload() {
    let src = "class C { \
                 interface Fn { int apply(int n); } \
                 static int call(String s, Fn f) { return 0; } \
                 static int call(int n, Fn f) { return 1; } \
                 static int use() { return call(\"a\", x -> x); } \
               }";
    // Selected by the `String` argument, so the lambda's target is that overload's parameter.
    assert_eq!(expr_ty(src, "x -> x"), "Fn");
}

/// A lambda's parameter is the *substituted* type, not the interface's own variable.
///
/// `Function<String, String> f = s -> …` binds `s` to `String`. Left as the interface's `T` it
/// erases to `Object`, so the body could reach no member of a `String` and the synthetic method the
/// backend emits disagreed with its own instructions.
#[test]
fn a_lambda_parameter_takes_the_targets_type_argument() {
    let src = "class C { \
                 interface Fn<T, R> { R apply(T t); } \
                 static void use() { Fn<String, String> f = s -> s; } \
               }";
    assert_eq!(def_ty(src, "s"), "String");
}

/// A call's type is the *selected* overload's return type, not the first member of that name.
///
/// The two answers were computed separately — the value's type by a name lookup in pass 2, the
/// member a backend invokes by overload selection in pass 3 — and a name with two return types is
/// where they disagreed. `int b = call(3, f);` beside a `String call(String)` was typed `String`,
/// which is a store instruction for a type the value does not have.
#[test]
fn a_calls_type_is_the_selected_overloads() {
    let src = "class C { \
                 interface Fn { int apply(int n); } \
                 static String call(String s) { return s; } \
                 static int call(int n, Fn f) { return f.apply(n); } \
                 static void use() { int b = call(3, x -> x * 2); String s = call(\"a\"); } \
               }";
    assert_eq!(expr_ty(src, "call(3, x -> x * 2)"), "int");
    assert_eq!(expr_ty(src, "call(\"a\")"), "String");
}

// --- Enclosing instances and enum constants ---------------------------------------------------

/// `Outer.this` and `Outer.super` name an enclosing instance, not a member.
///
/// The access carries the keyword where a field name would be, so there is no identifier for a
/// member lookup to use and the ordinary path answered `Unknown` — leaving everything the value was
/// then used for untyped with it. `super` resolves to the *superclass* of the named type, by the
/// same rule the bare `super` follows: answering with the named type would bind an overridden member
/// to the override.
#[test]
fn a_qualified_this_names_the_enclosing_instance() {
    let src = "class Base { int v; }
               class Outer extends Base {
                   int field;
                   class Inner {
                       int read() { return Outer.this.field; }
                       Object self() { return Outer.this; }
                       int inherited() { return Outer.super.v; }
                   }
               }";
    assert_eq!(expr_ty(src, "Outer.this"), "Outer");
    assert_eq!(expr_ty(src, "Outer.super"), "Base");
    assert_eq!(expr_ty(src, "Outer.this.field"), "int");
    assert_eq!(expr_ty(src, "Outer.super.v"), "int");
}

/// An `enum` constant writes no type and *is* an instance of the enum that declares it (JLS §8.9.3).
///
/// Nothing else can supply one: a constant is not a `FIELD_DECL` and has no `Type` node beside its
/// name. Without it a bare constant inside its own enum had no type at all — so `red.name()`
/// resolved to nothing, and a call taking one had no argument type to select an overload against.
#[test]
fn an_enum_constant_is_typed_as_its_enum() {
    // `label()` is declared on the enum itself rather than inherited from `java.lang.Enum`, so the
    // claim is about the constant's own type and not about whether the stubs are in reach.
    let src =
        "enum Colour { RED, GREEN; int label() { return 1; } int read() { return RED.label(); } }";
    assert_eq!(def_ty(src, "RED"), "Colour");
    assert_eq!(expr_ty(src, "RED"), "Colour");
    assert_eq!(expr_ty(src, "RED.label()"), "int");
}

/// A **nested** type is spelled with a dot, so a name qualified by one is a field access.
///
/// `Outer.Inner.CONSTANT` reads `Outer.Inner` as a receiver whose own type is unknown — it is not a
/// value at all — and the qualifier lookup read only the simple form. That left every constant of a
/// nested `enum` untyped, which is the ordinary way one is named.
#[test]
fn a_nested_type_qualifies_a_member() {
    let src = "class Outer { enum Kind { ERROR, WARNING } }
               class Use { Object read() { return Outer.Kind.ERROR; } }";
    assert_eq!(expr_ty(src, "Outer.Kind.ERROR"), "Kind");
}
