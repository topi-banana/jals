//! Snapshot tests for file-local name resolution: each reference rendered as one line,
//! `name@start..end -> <target>`, where the target is the resolved definition or `<unresolved>`.

use expect_test::{Expect, expect};
use jals_hir::{Resolution, resolve};

fn render(src: &str) -> String {
    let resolved = resolve(src);
    let mut s = String::new();
    for r in &resolved.references {
        let target = match r.resolution {
            Resolution::Def(id) => {
                let d = resolved.def(id);
                format!(
                    "{:?} `{}`@{}..{}",
                    d.kind, d.name, d.name_range.start, d.name_range.end
                )
            }
            Resolution::Unresolved => "<unresolved>".to_string(),
        };
        s.push_str(&format!(
            "{}@{}..{} -> {}\n",
            r.name, r.range.start, r.range.end, target
        ));
    }
    s
}

fn check(src: &str, expected: Expect) {
    expected.assert_eq(&render(src));
}

#[test]
fn local_and_unresolved_call() {
    check(
        "class C { void m() { int x = 1; use(x); } }",
        expect![[r#"
        use@32..35 -> <unresolved>
        x@36..37 -> Local `x`@25..26
    "#]],
    );
}

#[test]
fn use_before_declaration_is_unresolved() {
    check(
        "class C { void m() { f(x); int x = 1; } }",
        expect![[r#"
        f@21..22 -> <unresolved>
        x@23..24 -> <unresolved>
    "#]],
    );
}

#[test]
fn local_shadows_field() {
    check(
        "class C { int x; void m() { int x = 1; f(x); } }",
        expect![[r#"
            f@39..40 -> <unresolved>
            x@41..42 -> Local `x`@32..33
        "#]],
    );
}

#[test]
fn field_and_method_forward_references_hoist() {
    check(
        "class C { int get() { return helper() + x; } int x; int helper() { return 0; } }",
        expect![[r#"
            helper@29..35 -> Method `helper`@56..62
            x@40..41 -> Field `x`@49..50
        "#]],
    );
}

#[test]
fn namespace_separates_value_and_method() {
    // `run` is both a field (value) and a method; the call resolves to the method, the argument
    // to the field.
    check(
        "class C { int run; int run() { return 0; } void m() { run(run); } }",
        expect![[r#"
            run@54..57 -> Method `run`@23..26
            run@58..61 -> Field `run`@14..17
        "#]],
    );
}

#[test]
fn multi_declarator_initializer_sees_earlier_name() {
    check(
        "class C { void m() { int a = 1, b = a; } }",
        expect![[r#"
        a@36..37 -> Local `a`@25..26
    "#]],
    );
}

#[test]
fn parameter_resolves_in_body() {
    check(
        "class C { void m(int p) { use(p); } }",
        expect![[r#"
        use@26..29 -> <unresolved>
        p@30..31 -> Param `p`@21..22
    "#]],
    );
}

#[test]
fn for_each_variable_and_outer_iterable() {
    // The loop variable `s` resolves inside the body; the iterable `items` resolves to the field.
    check(
        "class C { int items; void m() { for (String s : items) use(s); } }",
        expect![[r#"
            items@48..53 -> Field `items`@14..19
            use@55..58 -> <unresolved>
            s@59..60 -> Local `s`@44..45
        "#]],
    );
}

#[test]
fn catch_binding_resolves_in_block() {
    check(
        "class C { void m() { try { } catch (Exception e) { log(e); } } }",
        expect![[r#"
            log@51..54 -> <unresolved>
            e@55..56 -> CatchParam `e`@46..47
        "#]],
    );
}

#[test]
fn try_with_resources_binding_resolves() {
    check(
        "class C { void m() { try (var r = open()) { use(r); } } }",
        expect![[r#"
            open@34..38 -> <unresolved>
            use@44..47 -> <unresolved>
            r@48..49 -> Resource `r`@30..31
        "#]],
    );
}

#[test]
fn lambda_parameter_resolves_in_body() {
    check(
        "class C { void m() { f(x -> g(x)); } }",
        expect![[r#"
            f@21..22 -> <unresolved>
            g@28..29 -> <unresolved>
            x@30..31 -> LambdaParam `x`@23..24
        "#]],
    );
}

#[test]
fn switch_pattern_variable_resolves_in_arm() {
    check(
        "class C { void m(Object o) { switch (o) { case Integer i -> use(i); default -> {} } } }",
        expect![[r#"
            o@37..38 -> Param `o`@24..25
            use@60..63 -> <unresolved>
            i@64..65 -> PatternVar `i`@55..56
        "#]],
    );
}

#[test]
fn member_access_right_hand_name_is_not_a_reference() {
    // `obj` resolves to the local; the right-hand `field` is not recorded (needs a type).
    check(
        "class C { void m() { var obj = make(); read(obj.field); } }",
        expect![[r#"
        make@31..35 -> <unresolved>
        read@39..43 -> <unresolved>
        obj@44..47 -> Local `obj`@25..28
    "#]],
    );
}

#[test]
fn var_keyword_is_not_a_reference() {
    check(
        "class C { void m() { var x = 1; use(x); } }",
        expect![[r#"
        use@32..35 -> <unresolved>
        x@36..37 -> Local `x`@25..26
    "#]],
    );
}
