//! Tests for the "cannot find symbol" analysis in the value and method name-spaces: a name no
//! scope, no supertype, and no static import binds.
//!
//! The sibling of `project.rs`'s type-name resolution, and the half where a false positive is
//! expensive — the rule reading it defaults to `error` — so most of what is pinned here is what
//! must **not** be reported.

use std::collections::BTreeSet;

use jals_hir::{FileAnalysis, FileId, Namespace, ProjectIndex};
use jals_syntax::SyntaxNode;
use jals_syntax::cfg::CfgMap;

/// Parses each source, keeping the nodes alive, in input order.
fn nodes(sources: &[&str]) -> Vec<(FileId, SyntaxNode)> {
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

/// The names reported unresolved across a whole project, as `name` for a value and `name()` for a
/// method, in file then offset order.
fn reported(sources: &[&str]) -> Vec<String> {
    let nodes = nodes(sources);
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&nodes).with_stdlib().build());
    let mut out = Vec::new();
    for (file, root) in &nodes {
        let analysis = jals_exec::block_on_inline(FileAnalysis::of(root));
        let names =
            jals_exec::block_on_inline(analysis.in_project(&index, *file).unresolved_names());
        out.extend(names.into_iter().map(|u| match u.namespace {
            Namespace::Method => format!("{}()", u.name),
            _ => u.name,
        }));
    }
    out
}

/// The one-file shorthand.
fn reported_one(src: &str) -> Vec<String> {
    reported(&[src])
}

#[test]
fn an_undeclared_name_is_reported() {
    assert_eq!(
        reported_one("class C { void m() { int a = nope; } }"),
        ["nope"]
    );
}

#[test]
fn an_undeclared_bare_call_is_reported_as_a_method() {
    assert_eq!(reported_one("class C { void m() { nope(); } }"), ["nope()"]);
}

#[test]
fn a_local_used_before_its_declaration_is_reported() {
    // javac's "cannot find symbol" too: JLS §6.3 starts a local's scope at its declarator.
    assert_eq!(
        reported_one("class C { void m() { int a = later; int later = 1; } }"),
        ["later"]
    );
}

#[test]
fn a_field_used_before_its_declaration_is_not() {
    // A field's scope is the whole class body, so the forward reference is legal.
    assert!(reported_one("class C { void m() { int a = later; } int later = 1; }").is_empty());
}

#[test]
fn an_inherited_member_is_not_reported() {
    let sources = &[
        "package p; class Base { protected int f; protected int m() { return 0; } }",
        "package p; class Sub extends Base { void use() { int a = f; int b = m(); } }",
    ];
    assert!(reported(sources).is_empty(), "{:?}", reported(sources));
}

#[test]
fn an_interface_constant_and_default_method_are_not_reported() {
    let sources = &[
        "package p; interface I { int K = 1; default int d() { return 0; } }",
        "package p; class C implements I { void use() { int a = K; int b = d(); } }",
    ];
    assert!(reported(sources).is_empty(), "{:?}", reported(sources));
}

#[test]
fn a_type_that_inherits_from_outside_the_project_stands_down() {
    // `ArrayList` brings members the index cannot enumerate, so no negative answer is available —
    // not even for a name it plainly does not declare.
    let src = "class C extends java.util.ArrayList<String> { void use() { int n = size(); int q = whatever; } }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn an_anonymous_class_is_asked_about_its_own_supertype() {
    // The enclosing type of a name in an anonymous body is the anonymous class, not the class
    // around it — otherwise every member it inherits reads as undefined.
    let sources = &[
        "package p; abstract class Abs { abstract void go(); protected int inherited; }",
        "package p; class Host { void m() { Abs a = new Abs() { void go() { int x = inherited; } }; } }",
    ];
    assert!(reported(sources).is_empty(), "{:?}", reported(sources));
}

#[test]
fn an_anonymous_class_over_an_external_supertype_stands_down() {
    let src = "class Host { void m() { Object o = new java.util.ArrayList<String>() { void f() { int n = size(); } }; } }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn an_enum_constant_body_reaches_the_enum() {
    let src = "enum E { A { void f() { int x = shared; } }; int shared; void f() {} }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn a_local_class_is_asked_about_its_own_supertype() {
    let sources = &[
        "package p; class Base { protected int f; }",
        "package p; class Host { void m() { class L extends Base { void g() { int x = f; } } } }",
    ];
    assert!(reported(sources).is_empty(), "{:?}", reported(sources));
}

#[test]
fn an_argument_to_an_anonymous_class_belongs_to_the_class_around_it() {
    // The argument sits under the same `NEW_EXPR` as the body but outside it, so it is resolved
    // against `Host` — where it really is undefined.
    let src = "class Host { int own; void m() { Object o = new Thread(nope) { }; int ok = own; } }";
    assert_eq!(reported_one(src), ["nope"]);
}

#[test]
fn an_outer_field_reached_from_a_nested_class_is_not_reported() {
    let src =
        "class Outer { int f; static int sf; class Inner { void m() { int a = f; int b = sf; } } }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn an_ambiguous_name_qualifier_is_not_reported() {
    // JLS §6.5.2: the left-hand name of a qualified name denotes a variable, a type, or a package,
    // and the value-namespace lookup that records it can only miss on the latter two.
    let src = "class C { static int sf; void m() { System.out.println(1); int a = C.sf; Class<?> k = C.class; Object o = java.util.Collections.emptyList(); Runnable r = C::st; } static void st() {} }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn a_case_label_constant_is_not_reported() {
    // JLS §14.11 lets an `enum` constant be written unqualified there; it binds against the
    // selector's type, which no name lookup reaches.
    let sources = &[
        "package p; enum Color { RED, GREEN }",
        "package p; class C { void m(Color c) { switch (c) { case RED -> {} default -> {} } switch (c) { case GREEN: break; default: break; } } }",
    ];
    assert!(reported(sources).is_empty(), "{:?}", reported(sources));
}

#[test]
fn a_single_static_import_binds_the_name_it_writes() {
    let src =
        "import static java.util.Arrays.asList;\nclass C { void m() { Object o = asList(1, 2); } }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn an_on_demand_static_import_of_a_partial_owner_stands_down() {
    // `java.lang.Math` is a deliberately partial stub, so nothing can be concluded about a bare
    // name in a file that imports its members on demand — including one it does not declare.
    let src = "import static java.lang.Math.*;\nclass C { void m() { double x = max(1.0, 2.0); int q = whatever; } }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn an_object_method_is_never_reported() {
    // Every type inherits it, and the stub `Object` is partial.
    let src = "class C { void m() { int h = hashCode(); String s = toString(); } }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn an_annotation_element_name_is_not_reported() {
    let sources = &[
        "package p; @interface Anno { int elem(); }",
        "package p; class C { @Anno(elem = 1) void m() {} }",
    ];
    assert!(reported(sources).is_empty(), "{:?}", reported(sources));
}

#[test]
fn a_member_access_right_hand_name_is_not_reported() {
    // It is not recorded as a reference at all — it needs a type, and structurally it is a bare
    // token rather than a name-reference node.
    let src = "class C { void m(String s) { int n = s.length(); int[] a = new int[1]; int l = a.length; } }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn a_lambda_parameter_and_a_record_component_are_not_reported() {
    let src = "record R(int comp) { int get() { return comp; } } class C { void m() { Runnable r = () -> { int y = 0; int z = y; }; } }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn a_label_is_not_reported() {
    let src = "class C { void m() { outer: for (int i = 0; i < 3; i++) { if (i == 1) { continue outer; } break outer; } } }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn a_compact_source_file_resolves_against_its_implicit_class() {
    // JEP 512: no type declaration surrounds the reference, and the implicit class inherits only
    // `Object` — so the file-local answer is final for everything else.
    let src = "void main() { int x = greet(); int y = nope; }\nint greet() { return 1; }";
    assert_eq!(reported_one(src), ["nope"]);
}

#[test]
fn a_compact_source_file_still_excepts_object_methods() {
    let src = "void main() { String s = toString(); }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

#[test]
fn a_records_compact_constructor_binds_its_implicit_parameters() {
    // `R { x = ...; }` names the implicit constructor parameter, which no declaration in the body
    // introduces — the one shape where a component-named binding is neither a local nor a field
    // reached by simple name.
    let src = "record R(int x, String s) { R { x = x + 1; s = s.trim(); } }";
    assert!(reported_one(src).is_empty(), "{:?}", reported_one(src));
}

/// The names reported unresolved in one file analysed under `features`.
///
/// The `cfg` map reaches the index as well as the file's own resolution — through
/// `with_disabled` here, and through `extract_file_with_cfg` in a host that assembles its index
/// incrementally. Either way a disabled member is not indexed, which is what makes the verdict
/// below a statement about *this* feature set.
fn reported_with_features(src: &str, features: &[&str]) -> Vec<String> {
    let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse(src));
    let root = parse.syntax();
    let features: BTreeSet<String> = features.iter().map(ToString::to_string).collect();
    let cfg = CfgMap::compute(&parse, &features);
    let disabled = [(FileId(0), cfg.clone())];
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), root.clone())])
            .with_stdlib()
            .with_disabled(&disabled)
            .build(),
    );
    let analysis = jals_exec::block_on_inline(FileAnalysis::of_with_cfg(&root, &cfg));
    jals_exec::block_on_inline(analysis.in_project(&index, FileId(0)).unresolved_names())
        .into_iter()
        .map(|u| u.name)
        .collect()
}

#[test]
fn a_live_reference_to_a_cfg_disabled_declaration_is_reported() {
    // The verdict is **per feature set**, deliberately: the compile frontend blanks the disabled
    // host, so under this selection the build really does not have the field — which is the same
    // thing javac would say about the lowered tree. Under the other selection it is declared and
    // nothing is reported.
    let src = "class C {\n    #[cfg(feature = \"x\")]\n    int fieldX;\n    void m() { int a = fieldX; }\n}";
    assert_eq!(reported_with_features(src, &[]), ["fieldX"]);
    assert!(
        reported_with_features(src, &["x"]).is_empty(),
        "{:?}",
        reported_with_features(src, &["x"])
    );
}

#[test]
fn a_reference_inside_a_cfg_disabled_host_is_not_reported() {
    // The resolver records no reference inside a disabled host at all, so a name that only the
    // *other* feature set declares cannot be reported against this one.
    let src = "class C {\n    #[cfg(feature = \"x\")]\n    void gone() { int a = onlyUnderX; }\n}";
    assert!(
        reported_with_features(src, &[]).is_empty(),
        "{:?}",
        reported_with_features(src, &[])
    );
}
