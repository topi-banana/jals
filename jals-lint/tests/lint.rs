use expect_test::{Expect, expect};
use jals_lint::{Config, LintOutput, Severity, lint_source};

/// Render the diagnostics (then parser errors) of a default-config lint run as one line each:
/// `rule:start..end: message`.
fn render(out: &LintOutput) -> String {
    let mut s = String::new();
    for d in out.diagnostics.iter().chain(&out.parse_errors) {
        s.push_str(&format!(
            "{}:{}..{}: {}\n",
            d.rule, d.range.start, d.range.end, d.message
        ));
    }
    s
}

fn lint(src: &str) -> String {
    render(&lint_source(src, &Config::default()))
}

fn check(src: &str, expected: Expect) {
    expected.assert_eq(&lint(src));
}

// ===== wildcard-import =====

#[test]
fn wildcard_import_flagged() {
    check(
        "import java.util.*;",
        expect![[r#"
            wildcard-import:0..19: avoid wildcard imports; import the specific types you use
        "#]],
    );
}

#[test]
fn specific_import_ok() {
    check("import java.util.List;", expect![""]);
}

// ===== empty-catch =====

#[test]
fn empty_catch_flagged() {
    check(
        "class Foo { void m() { try { x(); } catch (Exception e) {} } }",
        expect![[r#"
            empty-catch:35..58: empty catch block swallows the exception; handle it or add a comment explaining why
        "#]],
    );
}

#[test]
fn commented_catch_ok() {
    check(
        "class Foo { void m() { try { x(); } catch (Exception e) { /* ignored */ } } }",
        expect![""],
    );
}

#[test]
fn non_empty_catch_ok() {
    check(
        "class Foo { void m() { try { x(); } catch (Exception e) { log(e); } } }",
        expect![""],
    );
}

// ===== missing-braces =====

#[test]
fn missing_braces_if_flagged() {
    check(
        "class Foo { void m() { if (a) b(); } }",
        expect![[r#"
            missing-braces:29..34: `if` body should be wrapped in braces
        "#]],
    );
}

#[test]
fn braced_if_ok() {
    check("class Foo { void m() { if (a) { b(); } } }", expect![""]);
}

#[test]
fn else_if_chain_ok() {
    check(
        "class Foo { void m() { if (a) { b(); } else if (c) { d(); } } }",
        expect![""],
    );
}

#[test]
fn missing_braces_loops_flagged() {
    check(
        "class Foo { void m() { while (a) b(); for (int i = 0; a; i++) c(); } }",
        expect![[r#"
            missing-braces:32..37: `while` body should be wrapped in braces
            missing-braces:61..66: `for` body should be wrapped in braces
        "#]],
    );
}

// ===== naming-convention =====

#[test]
fn naming_type_and_method_flagged() {
    check(
        "class foo { void Bar() {} }",
        expect![[r#"
            naming-convention:6..9: type name `foo` should be UpperCamelCase
            naming-convention:17..20: method name `Bar` should be lowerCamelCase
        "#]],
    );
}

#[test]
fn naming_constant_flagged() {
    check(
        "class Foo { static final int maxValue = 1; }",
        expect![[r#"
            naming-convention:29..37: constant name `maxValue` should be UPPER_SNAKE_CASE
        "#]],
    );
}

#[test]
fn naming_field_flagged() {
    check(
        "class Foo { int my_field; }",
        expect![[r#"
            naming-convention:16..24: field name `my_field` should be lowerCamelCase
        "#]],
    );
}

#[test]
fn naming_clean_ok() {
    check(
        "class Foo { static final int MAX_VALUE = 1; int count; void doThing(int itemId) { use(itemId); } }",
        expect![""],
    );
}

// ===== unused-local =====

#[test]
fn unused_local_flagged() {
    check(
        "class Foo { void m() { int x = 1; } }",
        expect![[r#"
        unused-local:27..28: unused local variable `x`
    "#]],
    );
}

#[test]
fn used_local_ok() {
    check(
        "class Foo { int m() { int x = 1; return x; } }",
        expect![""],
    );
}

#[test]
fn unnamed_local_ok() {
    // `var _ = ...` binds nothing, so there is nothing to flag.
    check("class Foo { void m() { var _ = compute(); } }", expect![""]);
}

#[test]
fn multi_declarator_only_unused_flagged() {
    check(
        "class Foo { int m() { int a = 1, b = 2; return a; } }",
        expect![[r#"
            unused-local:33..34: unused local variable `b`
        "#]],
    );
}

#[test]
fn unused_parameter_of_bodied_method_flagged() {
    check(
        "class Foo { void m(int p) {} }",
        expect![[r#"
        unused-local:23..24: unused parameter `p`
    "#]],
    );
}

#[test]
fn abstract_parameter_not_flagged() {
    // An interface method has no body; its parameter can never be used, so it is not flagged.
    check("interface Foo { void m(int p); }", expect![""]);
}

#[test]
fn lambda_parameter_not_flagged() {
    // Unused lambda parameters are routinely intentional and are left alone.
    check("class Foo { void m() { run(x -> 1); } }", expect![""]);
}

// ===== configuration =====

#[test]
fn allow_suppresses_a_rule() {
    let mut config = Config::default();
    config
        .rules
        .insert("wildcard-import".to_string(), Severity::Allow);
    let out = lint_source("import java.util.*;", &config);
    assert!(
        out.diagnostics.is_empty(),
        "rule set to allow should not fire"
    );
}

#[test]
fn severity_is_resolved_from_config() {
    let mut config = Config::default();
    config
        .rules
        .insert("wildcard-import".to_string(), Severity::Error);
    let out = lint_source("import java.util.*;", &config);
    assert_eq!(out.diagnostics.len(), 1);
    assert_eq!(out.diagnostics[0].severity, Severity::Error);
}

// ===== type-mismatch =====

#[test]
fn type_mismatch_narrowing_flagged() {
    // A field initializer (fields are not subject to `unused-local`, isolating this rule).
    check(
        "class C { int x = 1.0; }",
        expect![[r#"
            type-mismatch:17..21: incompatible types: `double` cannot be assigned to `int`
        "#]],
    );
}

#[test]
fn type_mismatch_constant_narrowing_ok() {
    // `byte b = 1;` is legal constant narrowing — must not be flagged.
    check("class C { byte b = 1; }", expect![""]);
}
