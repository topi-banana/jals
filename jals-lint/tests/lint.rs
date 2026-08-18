use std::fmt::Write;

use expect_test::{Expect, expect};
use jals_config::lint::Config;
use jals_config::{Feature, FeatureSet, Severity};
use jals_lint::{LintOutput, LintRequest};

/// Render the diagnostics of a default-config lint run as one line each:
/// `rule:start..end: message`.
fn render(out: &LintOutput) -> String {
    let mut s = String::new();
    for d in &out.diagnostics {
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
    render(&jals_exec::block_on_inline(LintOutput::lint_source(
        src,
        &Config::default(),
    )))
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
    // The file uses what it imports: an import nothing spells is `unused-imports`' finding, not
    // this rule's, and the fixture keeps the two apart.
    check(
        "import java.util.List;\nclass Foo { List<String> l = null; }",
        expect![""],
    );
}

#[test]
fn wildcard_group_member_flagged() {
    // A jals grouped import hides its star one level down: the declaration's own name is the
    // shared prefix `java.util`, so the member is what carries the wildcard. `jals-hir` records
    // `java.util.concurrent` as an on-demand import here, so the rule must see it too. The finding
    // spans the member, not the whole declaration — `HashMap` beside it is fine — and starts at
    // the name, not at the space the member's node begins with.
    expect![[r"
        wildcard-import:27..39: avoid wildcard imports; import the specific types you use
    "]]
    .assert_eq(&lint_with_features(
        "import java.util.{HashMap, concurrent.*};\nclass Foo { HashMap<String, String> m = null; }",
        &[Feature::GroupedImports],
    ));
}

#[test]
fn grouped_import_without_a_wildcard_member_ok() {
    assert_eq!(
        lint_with_features(
            "import java.util.{HashMap, regex.Pattern};\n\
             class Foo { HashMap<String, String> m = null; Pattern p = null; }",
            &[Feature::GroupedImports],
        ),
        ""
    );
}

// ===== empty-catch =====

#[test]
fn empty_catch_flagged() {
    // Two independent findings over one clause, and both are true: the block handles nothing, and
    // the parameter it declares is never read. `unused-variables` is what names the fix Java 22
    // added for the second half — write the parameter `_`.
    check(
        "class Foo { void m() { try { x(); } catch (Exception e) {} } }",
        expect![[r"
            empty-catch:35..58: empty catch block swallows the exception; handle it or add a comment explaining why
            unused-variables:53..54: unused exception parameter `e`
        "]],
    );
}

#[test]
fn commented_catch_ok() {
    // The parameter is written `_`, so nothing but this rule has anything to say about the clause.
    check(
        "class Foo { void m() { try { x(); } catch (Exception _) { /* ignored */ } } }",
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

// ===== unused-variables =====

#[test]
fn unused_local_flagged() {
    check(
        "class Foo { void m() { int x = 1; } }",
        expect![[r"
        unused-variables:27..28: unused local variable `x`
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
fn an_underscore_prefixed_binding_is_not_flagged() {
    // A leading `_` is how an author writes a name the syntax demands and the code does not want:
    // the parameter of an `@Override`, an exception that is genuinely ignored. Every kind one file
    // scopes honours it, so no line here is reported.
    check(
        "class Foo { void m(int _p) { int _x = 1; try { g(); } catch (Exception _e) { /* ignored */ } } void g() {} }",
        expect![""],
    );
}

#[test]
fn multi_declarator_only_unused_flagged() {
    check(
        "class Foo { int m() { int a = 1, b = 2; return a; } }",
        expect![[r"
            unused-variables:33..34: unused local variable `b`
        "]],
    );
}

#[test]
fn unused_parameter_of_bodied_method_flagged() {
    check(
        "class Foo { void m(int p) {} }",
        expect![[r"
        unused-variables:23..24: unused parameter `p`
    "]],
    );
}

#[test]
fn abstract_parameter_not_flagged() {
    // An interface method has no body; its parameter can never be used, so it is not flagged.
    check("interface Foo { void m(int p); }", expect![""]);
}

#[test]
fn unused_lambda_parameter_flagged() {
    // Java 22 gives an unwanted parameter a name of its own (`_`), so one written out and then
    // ignored is a finding like any other rather than the idiom it used to be.
    check(
        "class Foo { void m() { run(x -> 1); } }",
        expect![[r"
            unused-variables:27..28: unused lambda parameter `x`
        "]],
    );
}

#[test]
fn unused_type_parameter_flagged() {
    check(
        "class Foo { <T> void m() {} }",
        expect![[r"
            unused-variables:13..14: unused type parameter `T`
        "]],
    );
}

#[test]
fn unused_exception_and_pattern_parameters_flagged() {
    check(
        "class Foo { void m(Object o) { try { g(); } catch (Exception e) { /* handled */ } if (o instanceof String s) {} } void g() {} }",
        expect![[r"
            unused-variables:61..62: unused exception parameter `e`
            unused-variables:106..107: unused pattern variable `s`
        "]],
    );
}

#[test]
fn a_resource_is_never_flagged() {
    // try-with-resources exists for the `close()` it runs; the name is the syntax's demand, not
    // the author's, so there is no change the diagnostic could be asking for.
    check(
        "class Foo { void m() throws Exception { try (AutoCloseable c = open()) {} } AutoCloseable open() { return null; } }",
        expect![""],
    );
}

// ===== dead-code =====

#[test]
fn unused_private_members_flagged() {
    check(
        "class Foo { private int f; private void m() {} private class Inner {} }",
        expect![[r"
            dead-code:24..25: unused private field `f`
            dead-code:40..41: unused private method `m`
            dead-code:61..66: unused private class `Inner`
        "]],
    );
}

#[test]
fn an_underscore_prefixed_private_member_is_still_flagged() {
    // The `_` opt-out stops at the kinds one file scopes. On a member the prefix is a naming
    // *style* — a whole codebase spells its private fields that way — so honouring it here would
    // drop the finding rather than record an intention.
    check(
        "class Foo { private int _f; private void _m() {} }",
        expect![[r"
            dead-code:24..26: unused private field `_f`
            dead-code:41..43: unused private method `_m`
        "]],
    );
}

#[test]
fn a_visible_member_is_never_flagged() {
    // Package-private, `protected`, and `public` members are another file's question, and one file
    // is not entitled to answer it.
    check(
        "class Foo { int f; void m() {} protected int g; public void n() {} }",
        expect![""],
    );
}

#[test]
fn a_private_member_reached_through_a_receiver_is_not_flagged() {
    // `this.f` and `o.m()` bind no file-local definition — the right-hand name of a member access
    // needs a type — so the analysis records them as mentions and the rule stays silent.
    check(
        "class Foo { private int f; private void m() {} int read() { return this.f; } void call(Foo o) { o.m(); } }",
        expect![""],
    );
}

#[test]
fn an_annotated_private_member_is_not_flagged() {
    // `@Inject` and its kin assign a field nothing in the source names; the annotation is evidence
    // against reading non-use as disuse.
    check("class Foo { @Deprecated private int f; }", expect![""]);
}

#[test]
fn the_serialization_members_are_not_flagged() {
    // The one line that is reported comes from `naming-convention`: the name is the serialization
    // contract's, not the author's, and that is a separate rule's quarrel with the JDK.
    check(
        "class Foo { private static final long serialVersionUID = 1L; private Object writeReplace() { return null; } }",
        expect![[r"
            naming-convention:38..54: constant name `serialVersionUID` should be UPPER_SNAKE_CASE
        "]],
    );
}

#[test]
fn an_overloaded_private_method_is_not_flagged() {
    // The scope chain binds `pick(1)` to *a* declaration named `pick`, not to the overload its
    // argument selects — that needs types the file-local pass has not got. So the evidence for a
    // method is the name, and neither declaration is reported.
    check(
        "class Foo { int m() { return pick(1); } private int pick(int i) { return i; } private int pick(String s) { return s.length(); } }",
        expect![""],
    );
}

#[test]
fn a_private_type_used_as_a_static_qualifier_is_not_flagged() {
    // `Holder.VALUE` puts `Holder` in JLS §6.5.2's ambiguous-name position: this pass looks a bare
    // name up as a *value* and finds no binding, so the mention is what keeps the class off the
    // report.
    check(
        "class Foo { private static class Holder { static final int VALUE = 1; } int m() { return Holder.VALUE; } }",
        expect![""],
    );
}

#[test]
fn a_private_type_named_only_by_a_class_literal_is_not_flagged() {
    // `Inner.class` is the last position where a bare name denotes a *type* (JLS §15.8.2), and the
    // grammar spells it as a plain `NAME_REF` — so the value-namespace lookup can only miss and the
    // mention is the whole evidence. Without it every `private` nested type reached through
    // `X.class` (a logger key, a Mixin target, a reflective lookup) reads as dead.
    check(
        "class Foo { private static class Inner {} Object m() { return Inner.class; } }",
        expect![""],
    );
}

#[test]
fn an_annotation_written_on_a_binding_the_grammar_nests_is_not_flagged() {
    // A for-each variable and a lambda parameter park their annotations in a `MODIFIERS` child that
    // neither shape used to recurse, so the annotation type they name had no trace in the analysis
    // at all — and a `private` one then read as unused.
    check(
        "class Foo { private @interface M {} void m(java.util.List<String> xs) { for (@M String s : xs) { g(s); } r((@M String t) -> t); } void g(String s) { System.out.println(s); } void r(Object o) { System.out.println(o); } }",
        expect![""],
    );
}

#[test]
fn a_private_constructor_is_not_flagged() {
    // `new Foo()` records a reference to the *type*, never to the constructor, so non-resolution
    // here is silence rather than evidence — and a private constructor is also how a utility class
    // says it is not instantiable.
    check("class Foo { private Foo() {} }", expect![""]);
}

// ===== unused-imports =====

#[test]
fn unused_import_flagged() {
    check(
        "import java.util.List;\nimport java.util.Map;\nclass Foo { List<String> l = null; }",
        expect![[r"
            unused-imports:23..44: unused import `java.util.Map`
        "]],
    );
}

#[test]
fn an_import_used_only_by_an_annotation_or_javadoc_is_not_flagged() {
    check(
        "import java.lang.annotation.Retention;\nimport java.util.Set;\n\
         /** See {@link Set}. */\n@Retention(null) class Foo {}",
        expect![""],
    );
}

#[test]
fn unused_static_import_flagged() {
    check(
        "import static java.lang.Math.max;\nclass Foo {}",
        expect![[r"
            unused-imports:0..33: unused static import `java.lang.Math.max`
        "]],
    );
}

#[test]
fn a_wildcard_import_is_never_reported_as_unused() {
    // An on-demand import names no single type, so nothing can be looked for; `wildcard-import` is
    // the rule with something to say about it.
    check(
        "import java.util.*;\nclass Foo {}",
        expect![[r"
            wildcard-import:0..19: avoid wildcard imports; import the specific types you use
        "]],
    );
}

// ===== configuration =====

#[test]
fn allow_suppresses_a_rule() {
    let mut config = Config::default();
    config
        .rules
        .insert("wildcard-import".to_owned(), Severity::Allow);
    let out = jals_exec::block_on_inline(LintOutput::lint_source("import java.util.*;", &config));
    assert!(
        out.diagnostics.is_empty(),
        "rule set to allow should not fire"
    );
}

#[test]
fn the_three_unused_rules_are_suppressed_independently() {
    // The whole reason one `unused` rule became three named after their `rustc` counterparts: a
    // project that cannot drop a parameter it does not get to name still wants to hear about a
    // `private` member and an import nothing reaches. Allowing one must silence only that one.
    let src = "import java.util.Map;\nclass Foo { private int f; void m(int p) {} }";
    let mut config = Config::default();
    config
        .rules
        .insert("unused-variables".to_owned(), Severity::Allow);
    let out = jals_exec::block_on_inline(LintOutput::lint_source(src, &config));
    let rules: Vec<_> = out.diagnostics.iter().map(|d| d.rule).collect();
    assert_eq!(rules, ["unused-imports", "dead-code"], "{out:?}");
}

#[test]
fn severity_is_resolved_from_config() {
    let mut config = Config::default();
    config
        .rules
        .insert("wildcard-import".to_owned(), Severity::Error);
    let out = jals_exec::block_on_inline(LintOutput::lint_source("import java.util.*;", &config));
    assert_eq!(out.diagnostics.len(), 1);
    assert_eq!(out.diagnostics[0].severity, Severity::Error);
}

// ===== type-mismatch =====

#[test]
fn type_mismatch_narrowing_flagged() {
    // A field initializer (a package-private field is not subject to `dead-code`, isolating this
    // rule).
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
    render(&jals_exec::block_on_inline(LintOutput::lint_source(
        src, &config,
    )))
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
    let out = jals_exec::block_on_inline(LintOutput::lint_source("void main() {}", &config));
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
        lint_with_features(
            "import java.util.List;\nclass Foo { List<String> l = null; }",
            &[Feature::Java24]
        ),
        ""
    );
    assert_eq!(
        lint_with_features(
            "import module.foo.Bar;\nclass Foo { Bar b = null; }",
            &[Feature::Java24]
        ),
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
    let out =
        jals_exec::block_on_inline(LintOutput::lint_source("import module java.base;", &config));
    assert!(
        out.diagnostics.iter().all(|d| d.rule != "module-import"),
        "expected the rule to be suppressed: {:?}",
        out.diagnostics
    );
}

// ===== grouped-import =====

#[test]
fn grouped_import_flagged_without_the_dialect_feature() {
    // A non-empty feature set that lacks `grouped-imports` (a jals dialect feature no release
    // preset implies) gates the syntax, with the dialect-flavored "add it to features" hint.
    expect![[r#"
        grouped-import:0..38: grouped imports (`import a.b.{X, Y};`) are a jals dialect feature; to use them, add `"grouped-imports"` to `[package] features`
    "#]]
    .assert_eq(&lint_with_features(
        "import java.util.{HashMap, ArrayList};\nclass Foo { HashMap<String, String> m; ArrayList<String> l; }",
        &[Feature::Java25],
    ));
}

#[test]
fn grouped_import_allowed_with_the_feature() {
    assert_eq!(
        lint_with_features(
            "import java.util.{HashMap, ArrayList};\n\
             class Foo { HashMap<String, String> m; ArrayList<String> l; }",
            &[Feature::GroupedImports],
        ),
        ""
    );
}

#[test]
fn grouped_import_flagged_even_without_declared_features() {
    // The empty-set exemption covers Java features only. Grouped imports are not valid Java at any
    // release, so a project that declares no `[package] features` has not "opted out of gating" —
    // it simply cannot compile the syntax: the build keys desugaring off the feature being
    // present, so `javac` would see the raw `.{...}`. Staying silent here would leave that with no
    // report at all.
    expect![[r#"
        grouped-import:0..38: grouped imports (`import a.b.{X, Y};`) are a jals dialect feature; to use them, add `"grouped-imports"` to `[package] features`
    "#]]
    .assert_eq(&lint_with_features(
        "import java.util.{HashMap, ArrayList};\nclass Foo { HashMap<String, String> m; ArrayList<String> l; }",
        &[],
    ));
}

#[test]
fn java_feature_gates_keep_the_empty_set_exemption() {
    // The counterpart: `module-imports` is real Java, so an undeclared feature set still opts out
    // of *its* gate. Narrowing the exemption for dialect features must not narrow it for these.
    assert_eq!(lint_with_features("import module java.base;", &[]), "");
    assert_eq!(lint_with_features("void main() {}", &[]), "");
}

#[test]
fn ordinary_import_is_not_a_grouped_import() {
    // A plain import has no group, so it is never flagged by `grouped-import`.
    assert_eq!(
        lint_with_features(
            "import java.util.List;\nclass Foo { List<String> l = null; }",
            &[Feature::Java25]
        ),
        ""
    );
}

// ===== attribute =====

#[test]
fn attribute_flagged_without_the_dialect_feature() {
    // Attributes attach at several depths — an import, a member's modifiers, a statement — and
    // each occurrence is flagged individually with the dialect-flavored hint.
    expect![[r#"
        attribute:0..21: attributes (`#[cfg(...)]`) are a jals dialect feature; to use them, add `"attributes"` to `[package] features`
        attribute:43..65: attributes (`#[cfg(...)]`) are a jals dialect feature; to use them, add `"attributes"` to `[package] features`
        attribute:76..98: attributes (`#[cfg(...)]`) are a jals dialect feature; to use them, add `"attributes"` to `[package] features`
    "#]]
    .assert_eq(&lint_with_features(
        "#[cfg(feature = \"x\")] import a.B;\nclass C { #[cfg(feature = \"x\")] void m() { #[cfg(feature = \"y\")] f(); } B b; }",
        &[Feature::Java25],
    ));
}

#[test]
fn attribute_allowed_with_the_feature() {
    assert_eq!(
        lint_with_features(
            "#[cfg(feature = \"x\")] import a.B;\nclass C { #[cfg(feature = \"x\")] void m() {} B b; }",
            &[Feature::Attributes],
        ),
        ""
    );
}

#[test]
fn attribute_flagged_even_without_declared_features() {
    // Like every dialect feature, the empty-set exemption does not apply: `javac` has never heard
    // of `#[...]`, so an undeclared feature set cannot compile it and silence would hide the one
    // jals-side report.
    expect![[r#"
        attribute:0..21: attributes (`#[cfg(...)]`) are a jals dialect feature; to use them, add `"attributes"` to `[package] features`
    "#]]
    .assert_eq(&lint_with_features(
        "#[cfg(feature = \"x\")] class C {}",
        &[],
    ));
}

#[test]
fn java_annotation_is_not_an_attribute() {
    // `@Override` is Java, not a jals attribute; it is never flagged by `attribute`.
    assert_eq!(
        lint_with_features("class C { @Override public void m() {} }", &[]),
        ""
    );
}

// ===== cfg-aware linting =====

/// Lint under the `attributes` dialect feature with the given build features enabled, mirroring
/// how a host wires the two: `Feature::Attributes` in the config's `FeatureSet`, and the file's
/// `CfgMap` computed against the resolved build-feature names.
fn lint_with_cfg(src: &str, build_features: &[&str]) -> String {
    let config = Config {
        features: FeatureSet::resolve(&[Feature::Attributes]),
        ..Default::default()
    };
    let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse(src));
    let features = build_features
        .iter()
        .map(ToString::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    let cfg = jals_syntax::cfg::CfgMap::compute(&parse, &features);
    render(&jals_exec::block_on_inline(LintOutput::lint(
        LintRequest {
            cfg: Some(&cfg),
            ..LintRequest::new(&parse)
        },
        &config,
    )))
}

#[test]
fn findings_inside_a_disabled_host_are_dropped() {
    // The empty catch (a syntactic rule) and the unused local (a resolution rule) both sit in
    // `cfg`-disabled code: the file compiles without them, so neither is reported. With the
    // feature on, both come back.
    let src = "class C {\n    #[cfg(feature = \"x\")]\n    void m() {\n        int unused = 1;\n        try { g(); } catch (Exception e) {}\n    }\n    void g() {}\n}";
    assert_eq!(lint_with_cfg(src, &[]), "");
    let on = lint_with_cfg(src, &["x"]);
    assert!(
        on.contains("unused-variables") && on.contains("empty-catch"),
        "expected both rules with the feature on: {on}"
    );
}

#[test]
fn an_import_used_only_inside_a_disabled_host_is_not_flagged() {
    // The `cfg` map hides a disabled host from the *resolution*, but the import above it is not
    // disabled and the other feature set does use it — so an import's evidence is the token
    // stream, which no `cfg` map touches. Reporting it with the flag off would ask for a deletion
    // that breaks the build the flag turns on.
    let src = "import java.util.List;\n#[cfg(feature = \"x\")]\nclass Gated { List<String> l; }\n";
    assert_eq!(lint_with_cfg(src, &[]), "");
    assert_eq!(lint_with_cfg(src, &["x"]), "");
}

#[test]
fn a_member_used_only_inside_a_disabled_host_is_not_flagged() {
    // The mirror of `an_import_used_only_inside_a_disabled_host_is_not_flagged`, and the same
    // argument: the declaration is *not* disabled, so it serves the other feature set, where the
    // same file does use it. Reporting it with the flag off asks for a deletion that breaks the
    // build the flag turns on. The resolver still binds nothing inside the host — the name is kept
    // as a mention, which is evidence without being resolution.
    let src = "class C {\n    private int f;\n    private void helper() {}\n    #[cfg(feature = \"x\")]\n    void m() { helper(); f = 1; }\n}";
    assert_eq!(lint_with_cfg(src, &[]), "");
    assert_eq!(lint_with_cfg(src, &["x"]), "");
}

#[test]
fn disabled_duplicate_definition_does_not_shadow_the_live_one() {
    // Two same-name methods, mutually exclusive under `cfg` — the analysis must resolve calls
    // against whichever survives, not report the pair as clashing or unused.
    let src = "class C {\n    #[cfg(feature = \"x\")]\n    int pick() { return 1; }\n    #[cfg(not(feature = \"x\"))]\n    int pick() { return 2; }\n    int use() { return pick(); }\n}";
    assert_eq!(lint_with_cfg(src, &[]), "");
    assert_eq!(lint_with_cfg(src, &["x"]), "");
}

#[test]
fn cfg_none_still_lints_everything() {
    // Without a cfg map (attributes off / no dialect), disabled-looking code is ordinary code.
    let src = "class C {\n    void m() {\n        try { g(); } catch (Exception e) {}\n    }\n    void g() {}\n}";
    let out = lint(src);
    assert!(out.contains("empty-catch"), "{out}");
}
