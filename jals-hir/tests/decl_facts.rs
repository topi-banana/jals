//! The syntax behind a fact: `Def::is_static`, and the two accessors that hand a consumer back the
//! node the resolver saw when it recorded a definition or a reference.
//!
//! These are the facts a lint rule used to re-derive by walking the whole file a second time, so
//! what is pinned here is the *agreement* — that asking `jals-hir` gives the same answer the
//! ancestor check gave, in the shapes where the two could come apart.

use core::fmt::Write;

use jals_hir::{DefKind, FileAnalysis, FileId, Namespace, ProjectIndex};
use jals_syntax::SyntaxKind;

fn analyse(src: &str) -> FileAnalysis {
    jals_exec::block_on_inline(FileAnalysis::parse(src))
}

/// Every field in the file as `name: <is_static>` lines, in declaration order.
fn field_statics(src: &str) -> String {
    let analysis = analyse(src);
    let mut s = String::new();
    for def in analysis.defs().iter().filter(|d| d.kind == DefKind::Field) {
        writeln!(s, "{}: {}", def.name, def.is_static).unwrap();
    }
    s
}

#[test]
fn a_written_static_field_is_static_and_an_instance_field_is_not() {
    assert_eq!(
        field_statics("class C { static int a; int b; private static final int c = 1; }"),
        "a: true\nb: false\nc: true\n"
    );
}

/// JLS §9.3: every field in an interface body is implicitly `public static final`, with none of
/// those tokens spelled.
#[test]
fn an_interface_field_is_static_without_the_keyword() {
    assert_eq!(
        field_statics("interface I { int SIDES = 3; }"),
        "SIDES: true\n"
    );
    assert_eq!(
        field_statics("@interface A { }\ninterface J { String NAME = \"x\"; }"),
        "NAME: true\n"
    );
}

/// The implicit set belongs to the **innermost** enclosing type, not to any interface ancestor: a
/// type nested in an interface declares ordinary instance state.
#[test]
fn a_type_nested_in_an_interface_declares_instance_fields() {
    assert_eq!(
        field_statics("interface I { class C { int x; } }"),
        "x: false\n"
    );
    assert_eq!(
        field_statics("interface I { record R(int x) { } }"),
        "x: false\n"
    );
    assert_eq!(
        field_statics("interface I { enum E { A; int x; } }"),
        "x: false\n"
    );
    // ...and the interface's own field in the same file is still static.
    assert_eq!(
        field_statics("interface I { int K = 1; class C { int x; } }"),
        "K: true\nx: false\n"
    );
}

/// An interface *method* is not implicitly `static` — only a field is (JLS §9.4 makes an
/// unqualified interface method `public abstract`, not `static`).
#[test]
fn an_interface_method_is_not_static() {
    let analysis = analyse("interface I { void m(); static void s() { } }");
    let method = |name: &str| {
        analysis
            .defs()
            .iter()
            .find(|d| d.name == name && d.kind == DefKind::Method)
            .unwrap_or_else(|| panic!("no method `{name}`"))
    };
    assert!(!method("m").is_static);
    assert!(method("s").is_static);
}

/// A record component is registered as a field, writes no `static`, and sits in a record — so it
/// is an instance field without needing a case of its own.
#[test]
fn a_record_component_is_not_static() {
    assert_eq!(
        field_statics("record P(int x, int y) { }"),
        "x: false\ny: false\n"
    );
}

/// `Def::is_static`'s doc claims the file-local fold and
/// [`MemberModifiers::is_static`](jals_hir::MemberModifiers::is_static) agree. They are two
/// implementations of JLS §9.3 in two layers' vocabulary, so the claim is only worth writing if
/// something checks it — this is that check.
#[test]
fn the_file_local_fold_agrees_with_the_project_index() {
    let src = "interface Shape { int SIDES = 3; double area(); static Shape unit() { return null; } }\n\
               class Box { static int s; int i; }";
    let node = jals_exec::block_on_inline(jals_syntax::Parse::parse(src)).syntax();
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&node));
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&[(FileId(0), node)]).build());

    let project_static = |ty: &str, member: &str, namespace| {
        let owner = index
            .resolve_type_name(FileId(0), ty, None)
            .project_id()
            .unwrap_or_else(|| panic!("no type `{ty}`"));
        let id = index
            .resolve_member(owner, member, namespace)
            .unwrap_or_else(|| panic!("no member `{ty}.{member}`"));
        index.member(id).modifiers.is_static
    };
    let local_static = |member: &str| {
        analysis
            .defs()
            .iter()
            .find(|d| d.name == member)
            .unwrap_or_else(|| panic!("no definition `{member}`"))
            .is_static
    };

    for (ty, member, namespace) in [
        // The one that only the fold gets right.
        ("Shape", "SIDES", Namespace::Value),
        ("Box", "s", Namespace::Value),
        ("Box", "i", Namespace::Value),
        // And the methods, where neither layer folds anything in.
        ("Shape", "area", Namespace::Method),
        ("Shape", "unit", Namespace::Method),
    ] {
        assert_eq!(
            local_static(member),
            project_static(ty, member, namespace),
            "`{ty}.{member}` reads differently file-locally and project-wide"
        );
    }
}

/// One declaration binding two names answers with that one declaration for both — the same reason
/// `is_private` is read off the declaration rather than off each name.
#[test]
fn decl_of_is_the_one_declaration_a_multi_declarator_binds() {
    let analysis = analyse("class C { int a, b; }");
    let fields: Vec<_> = analysis
        .defs()
        .iter()
        .filter(|d| d.kind == DefKind::Field)
        .collect();
    assert_eq!(fields.len(), 2);
    let decls: Vec<_> = fields
        .iter()
        .map(|d| analysis.decl_of(d).expect("a field has a declaration"))
        .collect();
    assert_eq!(decls[0].kind(), SyntaxKind::FIELD_DECL);
    assert_eq!(decls[0].text_range(), decls[1].text_range());
}

/// A component's declaration is the `RECORD_COMPONENT` in the header, which is outside the record
/// body — so the type it is written in is still the record.
#[test]
fn decl_of_a_record_component_is_the_component_inside_its_record() {
    let analysis = analyse("record P(int x) { }");
    let def = analysis
        .defs()
        .iter()
        .find(|d| d.name == "x")
        .expect("the component is a definition");
    let decl = analysis
        .decl_of(def)
        .expect("a component has a declaration");
    assert_eq!(decl.kind(), SyntaxKind::RECORD_COMPONENT);
    assert!(
        decl.ancestors()
            .any(|a| a.kind() == SyntaxKind::RECORD_DECL),
        "the component is written inside its record"
    );
}

/// The one shape where an offset-derived answer and a walk-derived one could diverge: a field
/// declared in an anonymous class body belongs to that body, not to the class around the `new`.
#[test]
fn decl_of_reaches_the_anonymous_body_a_field_is_declared_in() {
    let analysis = analyse("class C { int x; Object o = new Object() { int x; }; }");
    let inner = analysis
        .defs()
        .iter()
        .filter(|d| d.name == "x" && d.kind == DefKind::Field)
        .max_by_key(|d| d.name_range.start)
        .expect("two fields named x");
    let decl = analysis.decl_of(inner).expect("a field has a declaration");
    let body = decl
        .ancestors()
        .find(|a| a.kind() == SyntaxKind::CLASS_BODY)
        .expect("the field sits in a class body");
    assert_eq!(
        body.parent().map(|p| p.kind()),
        Some(SyntaxKind::NEW_EXPR),
        "the innermost body around the inner `x` is the anonymous class's"
    );
}

/// `site_of` answers with the `NAME_REF` the reference names.
///
/// The node's *own* range starts earlier than the reference's, because rowan parks the trivia
/// between two siblings inside the following node — which is why the resolver keyed this on the
/// identifier token and not on the node.
#[test]
fn site_of_is_the_name_ref_the_reference_names() {
    let analysis = analyse("class C { int x; int get() { return x; } }");
    let reference = analysis
        .references()
        .iter()
        .find(|r| r.name == "x" && r.namespace == Namespace::Value)
        .expect("`return x;` is a reference");
    let site = analysis
        .site_of(reference)
        .expect("the reference has a site");
    assert_eq!(site.kind(), SyntaxKind::NAME_REF);
    assert_eq!(site.text().to_string().trim(), "x");
    assert_eq!(usize::from(site.text_range().end()), reference.range.end);
}

/// A type reference is recorded from its `TYPE` node, not from a `NAME_REF`, so it has no site.
/// Pinned because it is the one namespace a caller must filter out rather than treat as a
/// tree/analysis disagreement.
#[test]
fn a_type_reference_has_no_name_ref_site() {
    let analysis = analyse("class C implements I { }");
    let reference = analysis
        .references()
        .iter()
        .find(|r| r.name == "I")
        .expect("`implements I` is a type reference");
    assert_eq!(reference.namespace, Namespace::Type);
    assert!(analysis.site_of(reference).is_none());
}

#[test]
fn every_reference_and_definition_can_be_taken_back_to_the_tree() {
    // Every `DefKind` the resolver registers, including the two whose declaring node is not a
    // declaration at all: a catch parameter is recorded against its `CATCH_CLAUSE` and a switch
    // pattern variable against its `SWITCH_LABEL`.
    let src = "import java.util.List;\n\
               interface I { int K = 1; }\n\
               record P(int x) { int sum() { return x; } }\n\
               class C<T> implements I { \
                 static int s; int a, b; \
                 int m(int p) { int l = p; for (int i : new int[0]) { l += i; } return l + a + s + K; } \
                 void g(Object o) { \
                   try (AutoCloseable c = null) { o.toString(); } \
                   catch (Exception e) { e.toString(); } \
                   if (o instanceof String pv) { pv.length(); } \
                   switch (o) { case Integer si -> si.intValue(); default -> { } } \
                 } \
                 java.util.function.Consumer<String> f = lp -> { int q = 1; System.out.println(q + lp); }; \
               }";
    let analysis = analyse(src);
    for kind in [
        DefKind::CatchParam,
        DefKind::PatternVar,
        DefKind::Resource,
        DefKind::LambdaParam,
        DefKind::TypeParam,
    ] {
        assert!(
            analysis.defs().iter().any(|d| d.kind == kind),
            "the fixture must actually bind a {kind:?} for the sweep to mean anything"
        );
    }
    for def in analysis.defs() {
        assert!(
            analysis.decl_of(def).is_some(),
            "no declaration node for {:?} `{}`",
            def.kind,
            def.name
        );
    }
    for reference in analysis.references() {
        // A type reference names a `TYPE` node rather than a `NAME_REF`; see
        // `a_type_reference_has_no_name_ref_site`.
        if reference.namespace == Namespace::Type {
            continue;
        }
        assert!(
            analysis.site_of(reference).is_some(),
            "no site for {:?} reference `{}` at {}",
            reference.namespace,
            reference.name,
            reference.range.start
        );
    }
}
