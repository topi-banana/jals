use std::fmt::Write;

use expect_test::{Expect, expect};
use jals_config::lint::Config;
use jals_config::{Feature, FeatureSet, Severity};
use jals_lint::LintOutput;

/// Render the diagnostics (then parser errors) of a default-config lint run as one line each:
/// `rule:start..end: message`.
fn render(out: &LintOutput) -> String {
    let mut s = String::new();
    for d in out.diagnostics.iter().chain(&out.parse_errors) {
        writeln!(
            s,
            "{}:{}..{}: {}",
            d.rule, d.range.start, d.range.end, d.message
        )
        .unwrap();
    }
    s
}

fn lint(src: &str) -> String {
    render(&LintOutput::lint_source(src, &Config::default()))
}

#[allow(clippy::needless_pass_by_value)]
fn check(src: &str, expected: Expect) {
    expected.assert_eq(&lint(src));
}

// ===== wildcard-import =====

#[test]
fn wildcard_import_flagged() {
    check(
        "import java.util.*;",
        expect![[r"
            wildcard-import:0..19: avoid wildcard imports; import the specific types you use
        "]],
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
        expect![[r"
            empty-catch:35..58: empty catch block swallows the exception; handle it or add a comment explaining why
        "]],
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
        expect![[r"
            missing-braces:29..34: `if` body should be wrapped in braces
        "]],
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
        expect![[r"
            missing-braces:32..37: `while` body should be wrapped in braces
            missing-braces:61..66: `for` body should be wrapped in braces
        "]],
    );
}

// ===== constant-condition =====

#[test]
fn constant_condition_flagged() {
    check(
        "class Foo { void m() { if (true) { a(); } else { b(); } } }",
        expect![[r"
            constant-condition:27..31: `if` condition is always true
        "]],
    );
    check(
        "class Foo { void m() { if (1 > 2) { a(); } } }",
        expect![[r"
            constant-condition:27..32: `if` condition is always false
        "]],
    );
}

#[test]
fn constant_condition_folds_final_locals() {
    check(
        "class Foo { void m() { final boolean debug = false; if (debug) { log(); } } }",
        expect![[r"
            constant-condition:56..61: `if` condition is always false
        "]],
    );
}

#[test]
fn variable_condition_ok() {
    check(
        "class Foo { void m(boolean a) { if (a) { b(); } } }",
        expect![""],
    );
}

#[test]
fn idiomatic_infinite_loops_ok() {
    check(
        "class Foo { void m() { while (true) { work(); } } }",
        expect![""],
    );
}

// ===== naming-convention =====

#[test]
fn naming_type_and_method_flagged() {
    check(
        "class foo { void Bar() {} }",
        expect![[r"
            naming-convention:6..9: type name `foo` should be UpperCamelCase
            naming-convention:17..20: method name `Bar` should be lowerCamelCase
        "]],
    );
}

#[test]
fn naming_constant_flagged() {
    check(
        "class Foo { static final int maxValue = 1; }",
        expect![[r"
            naming-convention:29..37: constant name `maxValue` should be UPPER_SNAKE_CASE
        "]],
    );
}

#[test]
fn naming_field_flagged() {
    check(
        "class Foo { int my_field; }",
        expect![[r"
            naming-convention:16..24: field name `my_field` should be lowerCamelCase
        "]],
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
        expect![[r"
        unused-local:27..28: unused local variable `x`
    "]],
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
        expect![[r"
            unused-local:33..34: unused local variable `b`
        "]],
    );
}

#[test]
fn unused_parameter_of_bodied_method_flagged() {
    check(
        "class Foo { void m(int p) {} }",
        expect![[r"
        unused-local:23..24: unused parameter `p`
    "]],
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
        .insert("wildcard-import".to_owned(), Severity::Allow);
    let out = LintOutput::lint_source("import java.util.*;", &config);
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
        .insert("wildcard-import".to_owned(), Severity::Error);
    let out = LintOutput::lint_source("import java.util.*;", &config);
    assert_eq!(out.diagnostics.len(), 1);
    assert_eq!(out.diagnostics[0].severity, Severity::Error);
}

// ===== type-mismatch =====

#[test]
fn type_mismatch_narrowing_flagged() {
    // A field initializer (fields are not subject to `unused-local`, isolating this rule).
    check(
        "class C { int x = 1.0; }",
        expect![[r"
            type-mismatch:17..21: incompatible types: `double` cannot be assigned to `int`
        "]],
    );
}

#[test]
fn type_mismatch_constant_narrowing_ok() {
    // `byte b = 1;` is legal constant narrowing — must not be flagged.
    check("class C { byte b = 1; }", expect![""]);
}

#[test]
fn type_mismatch_return_flagged() {
    // The method has no locals, so only `type-mismatch` fires.
    check(
        "class C { int m() { return 1.0; } }",
        expect![[r"
            type-mismatch:26..30: incompatible types: `double` cannot be assigned to `int`
        "]],
    );
}

// ===== compact-source-file =====

/// Lint `src` with the project's feature set resolved from the given `[package] features` list
/// (the host injects this from the manifest), rendered like [`lint`]. An empty list models a
/// manifest that declares no features, which leaves every gate off.
fn lint_with_features(src: &str, features: &[Feature]) -> String {
    let config = Config {
        features: FeatureSet::resolve(features),
        ..Default::default()
    };
    render(&LintOutput::lint_source(src, &config))
}

#[test]
fn compact_source_file_top_level_main_flagged_on_java24() {
    // A top-level `main` (JEP 512) is only a preview feature before Java 25.
    expect![[r#"
        compact-source-file:0..14: top-level declarations like `main` are a preview feature before `java25`; to use them, add `"java25"` or `"compact-source-files"` to `[package] features`
    "#]]
    .assert_eq(&lint_with_features("void main() {}", &[Feature::Java24]));
}

#[test]
fn compact_source_file_top_level_field_flagged_on_java24() {
    // Any top-level member — not just `main` — is an implicit-class declaration.
    expect![[r#"
        compact-source-file:0..14: top-level declarations like `main` are a preview feature before `java25`; to use them, add `"java25"` or `"compact-source-files"` to `[package] features`
    "#]]
    .assert_eq(&lint_with_features("int count = 0;", &[Feature::Java24]));
}

#[test]
fn compact_source_file_allowed_on_java25() {
    assert_eq!(lint_with_features("void main() {}", &[Feature::Java25]), "");
}

#[test]
fn compact_source_file_allowed_with_individual_feature() {
    // The single-feature opt-in works without moving to the java25 preset.
    assert_eq!(
        lint_with_features(
            "void main() {}",
            &[Feature::Java24, Feature::CompactSourceFiles]
        ),
        ""
    );
}

#[test]
fn compact_source_file_not_gated_without_features() {
    // No declared features (the common case): the syntax is not flagged.
    assert_eq!(lint_with_features("void main() {}", &[]), "");
}

#[test]
fn compact_source_file_class_member_main_ok_on_java24() {
    // A `main` inside a class is ordinary Java, never a compact source file.
    assert_eq!(
        lint_with_features("class C { void main() {} }", &[Feature::Java24]),
        ""
    );
}

#[test]
fn compact_source_file_respects_allow_config() {
    let mut config = Config {
        features: FeatureSet::resolve(&[Feature::Java24]),
        ..Default::default()
    };
    config
        .rules
        .insert("compact-source-file".to_owned(), Severity::Allow);
    let out = LintOutput::lint_source("void main() {}", &config);
    assert!(
        out.diagnostics
            .iter()
            .all(|d| d.rule != "compact-source-file"),
        "expected the rule to be suppressed: {:?}",
        out.diagnostics
    );
}

// ===== module-import =====

#[test]
fn module_import_flagged_on_java24() {
    // `import module M;` (JEP 511) is only a preview feature before Java 25.
    expect![[r#"
        module-import:0..24: module import declarations (`import module …;`) are a preview feature before `java25`; to use them, add `"java25"` or `"module-imports"` to `[package] features`
    "#]]
    .assert_eq(&lint_with_features(
        "import module java.base;",
        &[Feature::Java24],
    ));
}

#[test]
fn module_import_allowed_on_java25() {
    assert_eq!(
        lint_with_features("import module java.base;", &[Feature::Java25]),
        ""
    );
}

#[test]
fn module_import_not_gated_without_features() {
    // No declared features (the common case): the syntax is not flagged.
    assert_eq!(lint_with_features("import module java.base;", &[]), "");
}

#[test]
fn ordinary_import_not_flagged_on_java24() {
    // An ordinary type import — including one of a package/type literally named `module` — is not
    // a module import declaration (`is_module()` stays false), so it is never flagged.
    assert_eq!(
        lint_with_features("import java.util.List;", &[Feature::Java24]),
        ""
    );
    assert_eq!(
        lint_with_features("import module.foo.Bar;", &[Feature::Java24]),
        ""
    );
}

#[test]
fn module_import_respects_allow_config() {
    let mut config = Config {
        features: FeatureSet::resolve(&[Feature::Java24]),
        ..Default::default()
    };
    config
        .rules
        .insert("module-import".to_owned(), Severity::Allow);
    let out = LintOutput::lint_source("import module java.base;", &config);
    assert!(
        out.diagnostics.iter().all(|d| d.rule != "module-import"),
        "expected the rule to be suppressed: {:?}",
        out.diagnostics
    );
}
