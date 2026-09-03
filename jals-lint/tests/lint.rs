use std::fmt::Write;

use expect_test::{Expect, expect};
use jals_config::lint::{
    AnnotatedMembers, BracePolicy, Config, ConsoleStreams, Nullness, ThisScope,
};
use jals_config::{Feature, FeatureSet, LintLevel};
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
        "import java.util.List;\nclass Foo { List<String> l; }",
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
        "import java.util.{HashMap, concurrent.*};\nclass Foo { HashMap<String, String> m; }",
        &[Feature::GroupedImports],
    ));
}

#[test]
fn grouped_import_without_a_wildcard_member_ok() {
    assert_eq!(
        lint_with_features(
            "import java.util.{HashMap, regex.Pattern};\n\
             class Foo { HashMap<String, String> m; Pattern p; }",
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
fn naming_static_field_flagged_under_its_own_cell() {
    // The name is what an instance field would be written as, so this only reports at all because
    // a `static` without `final` reads the `statics` cell and not `fields` — and the message names
    // that cell, so the reader knows which of the two keys to reach for.
    check(
        "class Foo { static int itemCount; }",
        expect![[r"
            naming-convention:23..32: static field name `itemCount` should be UPPER_SNAKE_CASE
        "]],
    );
}

#[test]
fn an_interface_constant_reads_the_fields_cell_not_the_constants_one() {
    // JLS §9.3 makes this `public static final` with none of those tokens spelled, and
    // `jals-hir`'s `Def::is_static` folds that implication in. `naming-convention` deliberately
    // does *not*: its three cells are read off the modifiers the declaration **writes**, so an
    // interface field reads as `fields` and is asked for lowerCamelCase. Pinned because no other
    // naming test uses an interface, so a rule moved onto the folded fact would change this answer
    // with nothing failing.
    check(
        "interface I { int SIDES = 3; }",
        expect![[r"
            naming-convention:18..23: field name `SIDES` should be lowerCamelCase
        "]],
    );
}

#[test]
fn a_mutable_global_is_upper_snake_case_by_default() {
    // The built-in takes rustc's `non_upper_case_globals` reading: a `static` is a global whether
    // or not it is `final`, so the logger every Java codebase declares is a finding out of the
    // box. Google Java Style §5.2.4 says otherwise, and that reading is the opt-out below.
    check(
        "class Foo { static Object logger; }",
        expect![[r"
            naming-convention:26..32: static field name `logger` should be UPPER_SNAKE_CASE
        "]],
    );
}

#[test]
fn statics_lower_camel_case_gives_the_google_java_style_reading() {
    // §5.2.4 writes every non-constant field in `lowerCamelCase` however it is scoped. Taking that
    // line moves `logger` and nothing else: the misspelled constant beside it is still reported,
    // which is the whole reason `statics` is its own key rather than part of `constants`.
    let config: Config =
        toml::from_str("[naming.naming-convention]\nstatics = \"lower-camel-case\"\n").unwrap();
    let out = jals_exec::block_on_inline(LintOutput::lint_source(
        "class Foo { static Object logger; static final int maxValue = 1; int count; }",
        &config,
    ));
    assert_eq!(
        render(&out),
        "naming-convention:51..59: constant name `maxValue` should be UPPER_SNAKE_CASE\n"
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
        "class Foo { void m() throws Exception { try (AutoCloseable c = open()) {} } AutoCloseable open() { return () -> {}; } }",
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
        "class Foo { private static final long serialVersionUID = 1L; private Object writeReplace() { return this; } }",
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
        "import java.util.List;\nimport java.util.Map;\nclass Foo { List<String> l; }",
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
fn an_import_spelled_with_an_escape_in_a_comment_is_not_flagged() {
    // JLS §3.3 resolves `\\uXXXX` before the lexer even recognizes a comment, so `\\u0053et` in a
    // Javadoc reference *is* `Set`. The identifier walk decodes; the comment walk must too, or the
    // asymmetry reports an import the documentation names.
    check(
        "import java.util.Set;\n/** see {@link \\u0053et} */\nclass Foo {}",
        expect![""],
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

// ===== collapsible-if =====

#[test]
fn collapsible_if_flagged() {
    check(
        "class C { void m(boolean a, boolean b) { if (a) { if (b) { m(a, b); } } } }",
        expect![[r"
            collapsible-if:45..46: this `if` only guards another `if`; join the two conditions with `&&`
        "]],
    );
}

#[test]
fn a_brace_less_nested_if_is_still_collapsible() {
    let out = lint("class C { void m(boolean a, boolean b) { if (a) if (b) m(a, b); } }");
    assert!(out.contains("collapsible-if"), "{out}");
}

#[test]
fn an_if_with_an_else_is_not_collapsible() {
    // Neither an `else` on the outer `if` (the branches are not the same branch) …
    let out = lint(
        "class C { void m(boolean a, boolean b) { if (a) { if (b) { m(a, b); } } else { m(b, a); } } }",
    );
    assert!(!out.contains("collapsible-if"), "{out}");
    // … nor one on the inner.
    let out = lint(
        "class C { void m(boolean a, boolean b) { if (a) { if (b) { m(a, b); } else { m(b, a); } } } }",
    );
    assert!(!out.contains("collapsible-if"), "{out}");
}

#[test]
fn an_else_if_is_eligible_as_the_outer_if() {
    // `else if (b) { if (a) … }` collapses to `else if (b && a)` exactly as a free-standing one
    // does. What the chain rules out is the `if` above it, and an outer `if` with an `else` has
    // two branches — so it is already excluded, once, by the `else` test.
    let out = lint(
        "class C { void m(boolean a, boolean b) { if (a) { m(a, b); } else if (b) { if (a) { m(b, a); } } } }",
    );
    assert_eq!(out.matches("collapsible-if").count(), 1, "{out}");
}

#[test]
fn a_statement_or_comment_beside_the_inner_if_keeps_the_nesting() {
    let out = lint(
        "class C { void m(boolean a, boolean b) { if (a) { m(a, b); if (b) { m(b, a); } } } }",
    );
    assert!(!out.contains("collapsible-if"), "{out}");
    let out = lint(
        "class C { void m(boolean a, boolean b) { if (a) { /* why */ if (b) { m(b, a); } } } }",
    );
    assert!(!out.contains("collapsible-if"), "{out}");
}

// ===== boxed-primitive-constructor =====

#[test]
fn boxed_primitive_constructor_flagged() {
    check(
        "class C { Object o = new Integer(1); }",
        expect![[r"
            boxed-primitive-constructor:20..35: `new Integer(…)` always allocates; use `Integer.valueOf(…)`
        "]],
    );
}

#[test]
fn a_qualified_wrapper_constructor_is_flagged_too() {
    let out = lint("class C { Object o = new java.lang.Double(1.0); }");
    assert!(out.contains("boxed-primitive-constructor"), "{out}");
}

#[test]
fn a_non_wrapper_and_an_anonymous_subclass_are_not_flagged() {
    let out = lint("class C { Object o = new Object(); Object p = new Integer(1) {}; }");
    assert!(!out.contains("boxed-primitive-constructor"), "{out}");
}

#[test]
fn a_wrapper_used_as_a_type_argument_or_an_array_element_is_not_flagged() {
    // The shape half of real Java is written in. The name has to be the *constructed* type, which
    // is `Type::simple_name` — the last top-level `IDENT` — and not the last `IDENT` anywhere in
    // the type's subtree, which is the type argument.
    let out = lint(
        "class C {\n  Object a = new java.util.ArrayList<Integer>();\n  Object b = new java.util.HashMap<String, Long>();\n  Object c = new Integer[10];\n}",
    );
    assert!(!out.contains("boxed-primitive-constructor"), "{out}");
}

// ===== empty-javadoc =====

#[test]
fn empty_javadoc_flagged() {
    check(
        "/***/\nclass C {}",
        expect![[r"
            empty-javadoc:0..5: empty Javadoc comment; document the declaration or remove the comment
        "]],
    );
}

#[test]
fn a_javadoc_with_prose_and_a_plain_block_comment_are_not_flagged() {
    let out = lint("/** Something. */\nclass C {}\n/* */\nclass D {}");
    assert!(!out.contains("empty-javadoc"), "{out}");
}

// ===== print-to-console =====

#[test]
fn print_to_console_is_off_by_default() {
    // Every `[restriction]` rule is: a restriction nobody asked for is not a finding.
    let out = lint("class C { void m() { System.out.println(\"x\"); } }");
    assert!(!out.contains("print-to-console"), "{out}");
}

#[test]
fn print_to_console_reports_the_configured_streams() {
    let src = "class C { void m() { System.out.println(\"o\"); System.err.println(\"e\"); } }";
    let mut config = Config::default();
    config.restriction.print_to_console.level = LintLevel::Warn;
    let both = jals_exec::block_on_inline(LintOutput::lint_source(src, &config));
    assert_eq!(both.diagnostics.len(), 2, "{both:?}");

    // clippy spells this as two lints a config can enable in any combination; one key with three
    // values reaches every reachable state and no unreachable one.
    config.restriction.print_to_console.options.streams = ConsoleStreams::Stderr;
    let err_only = jals_exec::block_on_inline(LintOutput::lint_source(src, &config));
    let messages: Vec<&str> = err_only
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(messages.len(), 1, "{err_only:?}");
    assert!(messages[0].contains("System.err"), "{messages:?}");
}

// ===== implicit-this =====

/// The `implicit-this` findings alone, as `start..end: message` lines.
///
/// The rule is `allow` by default, so a test has to raise it — and a fixture that exercises it
/// trips `dead-code` and `naming-convention` on the same fields, which is noise this rule's
/// expectations should not carry.
fn implicit_this(src: &str, scope: ThisScope) -> String {
    let mut config = Config::default();
    config.restriction.implicit_this.options.scope = scope;
    let out = jals_exec::block_on_inline(LintOutput::lint_source(src, &config));
    let mut s = String::new();
    for d in out.diagnostics.iter().filter(|d| d.rule == "implicit-this") {
        writeln!(s, "{}..{}: {}", d.range.start, d.range.end, d.message).unwrap();
    }
    s
}

#[allow(clippy::needless_pass_by_value)]
fn check_this(src: &str, expected: Expect) {
    expected.assert_eq(&implicit_this(src, ThisScope::Always));
}

#[test]
fn implicit_this_is_on_by_default() {
    // The one `[restriction]` rule that is not `allow`. The section no longer decides a level for
    // every rule it holds: an unqualified field is a name a reader can take for a local, which is
    // a hazard before it is a policy.
    let out = lint("class C { int count; void m() { count++; } }");
    assert!(out.contains("implicit-this"), "{out}");
}

#[test]
fn every_non_static_context_is_checked() {
    // The four places an instance field can be reached without a receiver: a method, a
    // constructor, an instance initializer, and — the one checkstyle also checks — another
    // field's initializer.
    check_this(
        "class C {\n  int a;\n  int b = a + 1;\n  C() { a = 0; }\n  { a = 1; }\n  void m() { a = 2; }\n}",
        expect![[r"
            29..30: field `a` should be qualified with `this.`
            44..45: field `a` should be qualified with `this.`
            57..58: field `a` should be qualified with `this.`
            79..80: field `a` should be qualified with `this.`
        "]],
    );
}

#[test]
fn a_qualified_reference_is_what_the_rule_asks_for() {
    // `this.a` parses as a FIELD_ACCESS whose member name is a bare IDENT, so `jals-hir` records
    // no reference for it at all — the rule's whole detection rests on that.
    check_this("class C { int a; void m() { this.a = 1; } }", expect![""]);
}

#[test]
fn a_static_context_is_not_reported() {
    // `this` does not exist there, so there is no qualifier to ask for.
    check_this(
        "class C {\n  static int s;\n  static { s = 1; }\n  static void m() { s = 2; }\n}",
        expect![""],
    );
}

#[test]
fn a_static_field_is_not_reported_from_an_instance_method() {
    // Reachable without a receiver, but `this.` is not the qualifier Java wants for it — the type
    // name is.
    check_this("class C { static int s; void m() { s = 1; } }", expect![""]);
}

#[test]
fn an_interface_field_is_implicitly_static() {
    // JLS §9.3: a field declared in an interface is `public static final` with none of the three
    // spelled, so reading the modifiers alone would have called it an instance field.
    check_this(
        "interface I { int MAX = 1; default int m() { return MAX; } }",
        expect![""],
    );
}

#[test]
fn a_local_shadowing_the_field_never_reaches_this_rule() {
    // The scope chain stops at the innermost match, so `a` here is the local. Reporting it would
    // ask for a qualifier that changes which binding the name denotes.
    check_this(
        "class C { int a; void m() { int a = 1; a = 2; } }",
        expect![""],
    );
}

#[test]
fn a_lambda_body_is_still_the_enclosing_instance() {
    // A lambda introduces no type, so `this` inside one is the same instance — the qualifier is
    // available and the rule asks for it.
    check_this(
        "class C {\n  int a;\n  Runnable r = () -> a++;\n}",
        expect![[r"
            40..41: field `a` should be qualified with `this.`
        "]],
    );
}

#[test]
fn an_anonymous_class_is_a_different_instance() {
    // `this.a` inside the anonymous body would denote the anonymous instance, which has no `a`.
    // The qualifier Java wants is `C.this.a`, which this rule does not ask for.
    check_this(
        "class C {\n  int a;\n  Runnable r = new Runnable() { public void run() { a++; } };\n}",
        expect![""],
    );
}

#[test]
fn a_nested_class_is_a_different_instance() {
    // Same reason as the anonymous one, and the same for a class declared inside a method body.
    check_this(
        "class C {\n  int a;\n  class Inner { void m() { a++; } }\n  void outer() { class Local { void m() { a++; } } }\n}",
        expect![""],
    );
}

#[test]
fn a_record_component_is_a_field_outside_the_compact_constructor() {
    // A component *is* the record's `private final` field, so `this.x` is right in an ordinary
    // method. Inside `Point { ... }` the same spelling is the implicit parameter, which no
    // qualifier can name — and the resolver registers no parameter there, so without the skip it
    // would bind to the component and be reported.
    check_this(
        "record Point(int x) {\n  Point { if (x < 0) { x = 0; } }\n  int doubled() { return x * 2; }\n}",
        expect![[r"
            81..82: field `x` should be qualified with `this.`
        "]],
    );
}

#[test]
fn an_inherited_field_is_out_of_reach() {
    // The documented limit: a superclass field stays unresolved in the file-local pass, so it is
    // silently not reported. Binding it would need a `ProjectIndex`, and a `Checker::Semantic`
    // rule reports nothing at all when a host supplies no project.
    check_this(
        "class Base { protected int a; }\nclass C extends Base { void m() { a = 1; } }",
        expect![""],
    );
}

#[test]
fn an_enum_member_is_checked_and_a_constant_body_is_not() {
    // `ENUM_BODY` is deliberately not one of the type-introducing kinds — an enum's own members
    // sit under it and the walk falls through to `ENUM_DECL`, which is what makes `hits` here the
    // enclosing type's field. A constant *with a body* does introduce a type, so reaching `hits`
    // from inside one is the deliberate miss the module docs claim: the qualifier Java wants
    // there is `E.this.`.
    check_this(
        "enum E {\n  A,\n  B { void go() { hits++; } };\n  int hits;\n  void bump() { hits++; }\n  void go() {}\n}",
        expect![[r"
            73..77: field `hits` should be qualified with `this.`
        "]],
    );
}

#[test]
fn every_declarator_of_a_multi_field_is_reported() {
    // `int a, b;` binds two definitions off one declaration. The rule no longer keys a table of its
    // own on those names — it asks `jals-hir` for each field's `is_static` and for the declaration
    // behind it — but both names still have to come back, and the one declaration has to answer for
    // both, so the behaviour stays pinned here.
    check_this(
        "class C { int a, b; void m() { a = b; } }",
        expect![[r"
            31..32: field `a` should be qualified with `this.`
            35..36: field `b` should be qualified with `this.`
        "]],
    );
}

#[test]
fn shadowed_only_reports_where_the_executable_also_declares_the_name() {
    // checkstyle's `validateOnlyOverlapping`: the sites where the spelling alone no longer says
    // which binding a reader is looking at. `inc` has no local named `count`, so it goes quiet;
    // `reset` does, and its `count = 0` — which JLS §6.3 binds to the *field*, the local not yet
    // being in scope — is exactly the confusing site.
    let src = "class C {\n  int count;\n  void inc() { count++; }\n  void reset() { count = 0; int count = 1; use(count); }\n}";
    let always = implicit_this(src, ThisScope::Always);
    assert_eq!(always.lines().count(), 2, "{always}");
    expect![[r"
        66..71: field `count` should be qualified with `this.`
    "]]
    .assert_eq(&implicit_this(src, ThisScope::ShadowedOnly));
}

#[test]
fn implicit_this_is_suppressible_by_rule_and_by_section() {
    // Nothing in `suppress.rs` names a rule: it reads `RuleMeta`'s own name and category, so a
    // rule added later is suppressible the day it lands.
    for name in ["implicit-this", "restriction", "all"] {
        let src =
            format!("class C {{ int a; @SuppressWarnings(\"{name}\") void m() {{ a = 1; }} }}");
        assert_eq!(implicit_this(&src, ThisScope::Always), "", "@{name}");
    }
}

// ===== nullness-mismatch =====

/// The `nullness-mismatch` findings of `src` under `config`, one `start..end: message` line each.
///
/// Filtered to the one rule because the fixtures below are about nullness and nothing else: a
/// declaration written to exercise a slot should not have to also satisfy `naming-convention`.
fn nullness_with(src: &str, config: &Config) -> String {
    let out = jals_exec::block_on_inline(LintOutput::lint_source(src, config));
    let mut s = String::new();
    for d in out
        .diagnostics
        .iter()
        .filter(|d| d.rule == "nullness-mismatch")
    {
        writeln!(s, "{}..{}: {}", d.range.start, d.range.end, d.message).unwrap();
    }
    s
}

fn nullness(src: &str) -> String {
    nullness_with(src, &Config::default())
}

#[test]
fn null_into_an_unannotated_field_is_flagged() {
    // The built-in `default = "non-null"`: silence in the declaration is a claim, and `null`
    // contradicts it.
    expect![[r"
        21..25: `null` cannot be assigned to `s`, which is non-null
    "]]
    .assert_eq(&nullness("class C { String s = null; }"));
}

#[test]
fn null_into_an_unannotated_local_is_flagged() {
    // A local is not exempt. JSpecify leaves locals out of `@NullMarked` on the grounds that their
    // nullness is inferred from the initializer; jals does not, because a project that asked for
    // the strict reading asked for it about the code it writes.
    expect![[r"
        32..36: `null` cannot be assigned to `s`, which is non-null
    "]]
    .assert_eq(&nullness("class C { void m() { String s = null; } }"));
}

#[test]
fn null_returned_from_an_unannotated_method_is_flagged() {
    expect![[r"
        30..34: `null` cannot be returned from `m`, which is non-null
    "]]
    .assert_eq(&nullness("class C { String m() { return null; } }"));
}

#[test]
fn null_passed_to_an_unannotated_parameter_is_flagged() {
    expect![[r"
        50..54: `null` cannot be passed to parameter `x` of `take`, which is non-null
    "]]
    .assert_eq(&nullness(
        "class C { void take(String x) {} void go() { take(null); } }",
    ));
}

#[test]
fn a_nullable_value_flowing_into_a_non_null_slot_is_flagged() {
    // The finding the rule exists for, and the one that needs no `null` literal anywhere: the
    // contract says the call may answer `null` and the slot says it never holds one.
    expect![[r"
        74..80: a nullable value cannot be returned from `name`, which is non-null
    "]]
    .assert_eq(&nullness(
        "class C { @Nullable String find() { return null; } String name() { return find(); } }",
    ));
}

#[test]
fn a_nullable_declaration_accepts_null() {
    // Both halves of the contract in one fixture: `find` may answer `null`, and `keep` may hold
    // what it answers.
    assert_eq!(
        nullness(
            "class C { @Nullable String find() { return null; } @Nullable String keep = find(); }"
        ),
        ""
    );
}

#[test]
fn a_contradictory_declaration_is_flagged() {
    // The one finding that is about a declaration rather than about a value reaching it.
    expect![[r"
        10..38: this declaration is annotated both nullable and non-null
    "]]
    .assert_eq(&nullness("class C { @Nullable @NonNull String s; }"));
    // A parameter and a method reach the check through the walk's *other* arm — the declaring
    // forms that are not declarators — so each is reported exactly once and neither twice.
    expect![[r"
        10..78: this declaration is annotated both nullable and non-null
        38..62: this declaration is annotated both nullable and non-null
    "]]
    .assert_eq(&nullness(
        "class C { @Nullable @NonNull String m(@Nullable @NonNull int x) { return \"\"; } }",
    ));
}

#[test]
fn a_conditional_stands_down() {
    // One arm is nullable and the expression as a whole is guarded — a reader sees a choice, not a
    // violation. Reporting the arm is the false positive this rule's scope was chosen to avoid.
    assert_eq!(
        nullness(
            "class C { @Nullable String find() { return null; } \
             String name(boolean c) { return c ? find() : \"x\"; } }"
        ),
        ""
    );
}

#[test]
fn an_overloaded_callee_stands_down() {
    // The scope chain binds a call to *an* overload rather than to the one the arguments select,
    // so neither the parameter nor the return type read off `take` is known to be the one this
    // call reaches.
    assert_eq!(
        nullness(
            "class C { void take(String x) {} void take(Integer x, int y) {} \
             void go() { take(null); } }"
        ),
        ""
    );
}

#[test]
fn a_lambda_return_stands_down() {
    // `return null;` inside a lambda returns from the lambda, whose nullness belongs to the
    // functional interface rather than to the method the lambda is written in.
    assert_eq!(
        nullness("class C { Object m() { Runnable r = () -> { return null; }; return r; } }"),
        ""
    );
}

#[test]
fn a_declaration_without_an_initializer_is_not_a_finding() {
    // Nothing flows into it. The later assignment is what the rule has to answer, and it does.
    expect![[r"
        35..39: `null` cannot be assigned to `s`, which is non-null
    "]]
    .assert_eq(&nullness("class C { void m() { String s; s = null; } }"));
}

#[test]
fn each_declarator_is_paired_with_its_own_initializer() {
    // The CST is flat — one `LOCAL_VAR_DECL` holds both names and both initializers — so only the
    // token order says which value belongs to which name. Reading the first-name accessor would
    // report `a` for a `null` written after `b`.
    expect![[r"
        32..36: `null` cannot be assigned to `a`, which is non-null
        42..46: `null` cannot be assigned to `b`, which is non-null
    "]]
    .assert_eq(&nullness(
        "class C { void m() { String a = null, b = null; } }",
    ));
}

#[test]
fn a_try_resource_is_a_declarator_too() {
    // It declares a name and takes an initializer, so it is the same context — and a `null` there
    // is an NPE at the implicit `close()`, which is the reading a `[correctness]` rule owes.
    expect![[r"
        61..65: `null` cannot be assigned to `c`, which is non-null
    "]]
    .assert_eq(&nullness(
        "class C { void m() throws Exception { try (AutoCloseable c = null) {} } }",
    ));
    // …and a resource that is an existing variable declares nothing, so nothing flows into it.
    assert_eq!(
        nullness(
            "class C { void m(AutoCloseable existing) throws Exception { try (existing) {} } }"
        ),
        ""
    );
}

#[test]
fn an_import_says_which_nullable_it_is() {
    // The precision an FQN list buys. `com.acme.Nullable` is a perfectly good annotation and it is
    // not one of the ten this rule knows, so the declaration still reads as non-null — where a
    // last-segment match would have silently accepted it.
    expect![[r"
        57..61: `null` cannot be assigned to `s`, which is non-null
    "]]
    .assert_eq(&nullness(
        "import com.acme.Nullable;\nclass C { @Nullable String s = null; }",
    ));
    // …and the same file with a configured import is silent, so it is the import that decided.
    assert_eq!(
        nullness(
            "import org.jspecify.annotations.Nullable;\nclass C { @Nullable String s = null; }"
        ),
        ""
    );
}

#[test]
fn a_qualified_annotation_needs_no_import() {
    assert_eq!(
        nullness("class C { @org.jspecify.annotations.Nullable String s = null; }"),
        ""
    );
}

#[test]
fn unspecified_checks_only_what_the_source_annotated() {
    // The one-line escape hatch for a codebase that annotates part of itself: the unannotated
    // declaration goes quiet and the annotated one still speaks.
    let mut config = Config::default();
    config.correctness.nullness_mismatch.options.default = Nullness::Unspecified;
    assert_eq!(nullness_with("class C { String s = null; }", &config), "");
    expect![[r"
        55..59: `null` cannot be assigned to `s`, which is non-null
    "]]
    .assert_eq(&nullness_with(
        "class C { @org.jspecify.annotations.NonNull String s = null; }",
        &config,
    ));
}

#[test]
fn the_nullable_list_is_the_whole_vocabulary() {
    // Replaces rather than extends, so a project on one in-house annotation writes just that one —
    // and the families it did not name stop counting.
    let mut config = Config::default();
    config.correctness.nullness_mismatch.options.nullable = vec!["com.acme.MaybeNull".to_owned()];
    assert_eq!(
        nullness_with(
            "import com.acme.MaybeNull;\nclass C { @MaybeNull String s = null; }",
            &config
        ),
        ""
    );
    expect![[r"
        73..77: `null` cannot be assigned to `s`, which is non-null
    "]]
    .assert_eq(&nullness_with(
        "import org.jspecify.annotations.Nullable;\nclass C { @Nullable String s = null; }",
        &config,
    ));
}

/// The `nullness-mismatch` findings of `sources[0]`, linted with every source indexed as one
/// project — the route a real run takes, and the only one that can read a contract another file
/// wrote.
fn nullness_in_project(sources: &[&str], stdlib: bool) -> String {
    let parses: Vec<jals_syntax::Parse> = sources
        .iter()
        .map(|src| jals_exec::block_on_inline(jals_syntax::Parse::parse(src)))
        .collect();
    let nodes: Vec<(jals_hir::FileId, jals_syntax::SyntaxNode)> = parses
        .iter()
        .enumerate()
        .map(|(i, parse)| (jals_hir::FileId(u32::try_from(i).unwrap()), parse.syntax()))
        .collect();
    let mut builder = jals_hir::ProjectIndex::builder(&nodes);
    if stdlib {
        builder = builder.with_stdlib();
    }
    let index = jals_exec::block_on_inline(builder.build());
    let analysis = jals_exec::block_on_inline(jals_hir::FileAnalysis::of(&nodes[0].1));
    let semantics = analysis.in_project(&index, jals_hir::FileId(0));
    let out = jals_exec::block_on_inline(LintOutput::lint(
        LintRequest {
            file: Some(&semantics),
            ..LintRequest::new(&parses[0])
        },
        &Config::default(),
    ));
    let mut s = String::new();
    for d in out
        .diagnostics
        .iter()
        .filter(|d| d.rule == "nullness-mismatch")
    {
        writeln!(s, "{}..{}: {}", d.range.start, d.range.end, d.message).unwrap();
    }
    s
}

#[test]
fn another_files_nullable_is_read_through_the_index() {
    // The finding a project actually needs: the `@Nullable` a call has to respect is almost never
    // in the file making the call, and the file-local route cannot see it at all.
    expect![[r"
        51..59: a nullable value cannot be assigned to `s`, which is non-null
    "]]
    .assert_eq(&nullness_in_project(
        &[
            "class C { void m() { Api a = new Api(); String s = a.find(); } }",
            "public class Api { @org.jspecify.annotations.Nullable public String find() { return null; } }",
        ],
        false,
    ));
}

#[test]
fn the_overload_the_index_selected_is_the_one_checked() {
    // Two overloads, and `null` fits only one of them. The file-local route stands down on an
    // overloaded name because the scope chain binds *an* overload rather than the selected one;
    // the index has no such doubt, so this is a case the project route answers rather than skips.
    expect![[r"
        47..51: `null` cannot be passed to parameter `s` of `take`, which is non-null
    "]]
    .assert_eq(&nullness_in_project(
        &[
            "class C { void m() { Api a = new Api(); a.take(null); } }",
            "public class Api {\n  public void take(int n) {}\n  public void take(String s) {}\n}",
        ],
        false,
    ));
}

#[test]
fn a_nullable_parameter_in_another_file_accepts_null() {
    // The mirror of the case above, and the false positive Stage 1's silence was avoiding: without
    // the index, `take`'s parameter would read as unannotated and therefore non-null. The
    // unannotated twin below is the control — without it, "no findings" would also be what a
    // project route that never fired produces.
    let call = "class C { void m() { Api a = new Api(); a.take(null); } }";
    assert_eq!(
        nullness_in_project(
            &[
                call,
                "public class Api { public void take(@org.jspecify.annotations.Nullable String s) {} }",
            ],
            false,
        ),
        ""
    );
    expect![[r"
        47..51: `null` cannot be passed to parameter `s` of `take`, which is non-null
    "]]
    .assert_eq(&nullness_in_project(
        &[call, "public class Api { public void take(String s) {} }"],
        false,
    ));
}

#[test]
fn a_library_member_is_unknown_rather_than_unannotated() {
    // `String.equals(Object)` accepts `null` and says so nowhere jals can read: the embedded stubs
    // carry no annotations at all. Reading that silence as "the author wrote none" — and therefore,
    // under `default = "non-null"`, as a claim — would report every `null` passed to the standard
    // library. `ItemOrigin::carries_annotations` is the question that keeps it quiet, and the
    // project-declared twin below is what shows the route was live either way.
    assert_eq!(
        nullness_in_project(
            &["class C { boolean m(String s) { return s.equals(null); } }"],
            true,
        ),
        ""
    );
    expect![[r"
        45..49: `null` cannot be passed to parameter `o` of `equals`, which is non-null
    "]]
    .assert_eq(&nullness_in_project(
        &[
            "class C { boolean m(Api a) { return a.equals(null); } }",
            "public class Api { public boolean equals(Object o) { return false; } }",
        ],
        true,
    ));
}

// ===== rule options =====

#[test]
fn a_table_form_key_configures_the_rule_it_names() {
    // The whole point of the schema change: a level and an option in one key, and an option key
    // that does not have to restate the level it did not choose.
    let config: Config = toml::from_str(
        "[style]\nwildcard-import = { level = \"error\", static-imports = \"allow\" }\n",
    )
    .unwrap();
    let out = jals_exec::block_on_inline(LintOutput::lint_source(
        "import java.util.*;\nimport static java.lang.Math.*;",
        &config,
    ));
    assert_eq!(
        out.diagnostics.len(),
        1,
        "the static wildcard is exempt: {out:?}"
    );
    assert_eq!(out.diagnostics[0].severity, LintLevel::Error);
}

#[test]
fn missing_braces_multi_line_accepts_a_one_line_guard() {
    // The `if` is itself on its own line, which is where the naive reading of the statement's
    // `text_range` goes wrong: rowan parks the *preceding* newline inside the statement, so a
    // window measured from there contains a newline for every guard clause in the file.
    let mut config = Config::default();
    config.style.missing_braces.options.policy = BracePolicy::MultiLine;
    let src = "class C {\n  int m(int x) {\n    if (x > 0) return 1;\n    if (x < 0)\n      return -1;\n    return 0;\n  }\n}\n";
    let out = jals_exec::block_on_inline(LintOutput::lint_source(src, &config));
    let braces = out
        .diagnostics
        .iter()
        .filter(|d| d.rule == "missing-braces")
        .count();
    assert_eq!(
        braces, 1,
        "only the body that left its keyword's line: {out:?}"
    );
    // …and `always` still reports both, so the option is what made the difference.
    let out = jals_exec::block_on_inline(LintOutput::lint_source(src, &Config::default()));
    assert_eq!(
        out.diagnostics
            .iter()
            .filter(|d| d.rule == "missing-braces")
            .count(),
        2,
        "{out:?}"
    );
}

#[test]
fn empty_catch_honours_an_allowed_name() {
    let mut config = Config::default();
    config
        .suspicious
        .empty_catch
        .options
        .allowed_names
        .push("ignored".to_owned());
    let src = "class C { void m() { try { hashCode(); } catch (RuntimeException ignored) {} } }";
    let out = jals_exec::block_on_inline(LintOutput::lint_source(src, &config));
    assert!(
        out.diagnostics.iter().all(|d| d.rule != "empty-catch"),
        "{out:?}"
    );
}

#[test]
fn unused_variables_honours_the_configured_prefix() {
    let src = "class C { void m() { int _a = 0; int ignore_b = 1; } }";
    let mut config = Config::default();
    config.unused.unused_variables.options.ignore_prefix = "ignore_".to_owned();
    let out = jals_exec::block_on_inline(LintOutput::lint_source(src, &config));
    let names: Vec<&str> = out
        .diagnostics
        .iter()
        .filter(|d| d.rule == "unused-variables")
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        names.len(),
        1,
        "the prefix moved, so `_a` is now reported: {out:?}"
    );
    assert!(names[0].contains("_a"), "{names:?}");
}

#[test]
fn dead_code_can_be_told_that_annotations_do_not_inject() {
    let src = "class C { @Deprecated private int f; }";
    let mut config = Config::default();
    assert!(
        jals_exec::block_on_inline(LintOutput::lint_source(src, &config))
            .diagnostics
            .is_empty()
    );
    config.unused.dead_code.options.annotated = AnnotatedMembers::Report;
    let out = jals_exec::block_on_inline(LintOutput::lint_source(src, &config));
    assert!(
        out.diagnostics.iter().any(|d| d.rule == "dead-code"),
        "{out:?}"
    );
}

#[test]
fn an_unknown_key_is_reported_rather_than_dropped_or_fatal() {
    // The migration hazard the schema takes on deliberately: a `jalslint.toml` written against the
    // flat `[rules]` table still loads, every key it got right still applies, and the one it got
    // wrong is named instead of silently doing nothing.
    let config: Config = toml::from_str(
        "[rules]\nwildcard-import = \"allow\"\n\n[style]\nmissing-braces = \"error\"\n",
    )
    .unwrap();
    assert_eq!(config.unknown_keys(), ["rules"]);
    assert_eq!(config.style.missing_braces.level, LintLevel::Error);
    assert_eq!(
        config.style.wildcard_import.level,
        LintLevel::Warn,
        "the stale key configured nothing"
    );
}

// ===== configuration =====

#[test]
fn allow_suppresses_a_rule() {
    let mut config = Config::default();
    config.style.wildcard_import.level = LintLevel::Allow;
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
    config.unused.unused_variables.level = LintLevel::Allow;
    let out = jals_exec::block_on_inline(LintOutput::lint_source(src, &config));
    let rules: Vec<_> = out.diagnostics.iter().map(|d| d.rule).collect();
    assert_eq!(rules, ["unused-imports", "dead-code"], "{out:?}");
}

#[test]
fn severity_is_resolved_from_config() {
    let mut config = Config::default();
    config.style.wildcard_import.level = LintLevel::Error;
    let out = jals_exec::block_on_inline(LintOutput::lint_source("import java.util.*;", &config));
    assert_eq!(out.diagnostics.len(), 1);
    assert_eq!(out.diagnostics[0].severity, LintLevel::Error);
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
    let config = Config::default().with_features(FeatureSet::resolve(features));
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
    let mut config = Config::default().with_features(FeatureSet::resolve(&[Feature::Java24]));
    config.compatibility.compact_source_file.level = LintLevel::Allow;
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
            "import java.util.List;\nclass Foo { List<String> l; }",
            &[Feature::Java24]
        ),
        ""
    );
    assert_eq!(
        lint_with_features(
            "import module.foo.Bar;\nclass Foo { Bar b; }",
            &[Feature::Java24]
        ),
        ""
    );
}

#[test]
fn module_import_respects_allow_config() {
    let mut config = Config::default().with_features(FeatureSet::resolve(&[Feature::Java24]));
    config.compatibility.module_import.level = LintLevel::Allow;
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
            "import java.util.List;\nclass Foo { List<String> l; }",
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
    let config = Config::default().with_features(FeatureSet::resolve(&[Feature::Attributes]));
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
    // The field write is qualified because `implicit-this` is on by default and would report the
    // bare `f` — a finding about this fixture's spelling rather than about `cfg`.
    let src = "class C {\n    private int f;\n    private void helper() {}\n    #[cfg(feature = \"x\")]\n    void m() { helper(); this.f = 1; }\n}";
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

// ===== in-source suppression (`@SuppressWarnings`) =====

#[test]
fn suppression_by_rule_name() {
    // The narrowest spelling: the name the diagnostic carries, which is also the `jalslint.toml`
    // key — so a reported finding names the string that silences it, in the config *and* here.
    check(
        "class C { void m() { int unused = 1; if (true) { g(); } } void g() {} }",
        expect![[r"
            unused-variables:25..31: unused local variable `unused`
            constant-condition:41..45: `if` condition is always true
        "]],
    );
    check(
        "class C { @SuppressWarnings(\"unused-variables\") void m() { int unused = 1; if (true) { g(); } } void g() {} }",
        expect![[r"
            constant-condition:79..83: `if` condition is always true
        "]],
    );
}

#[test]
fn suppression_by_section_name() {
    // A section name suppresses every rule configured under it. This is the compatibility that
    // matters most: javac's `@SuppressWarnings("unused")` and jals's `[unused]` section are the
    // same word, so the annotation a Java codebase already carries silences the section — here
    // both `unused-variables` and `dead-code`, which are two rules and one defect class.
    let src = "class C { private int dead; void m() { int unused = 1; } }";
    check(
        src,
        expect![[r"
            dead-code:22..26: unused private field `dead`
            unused-variables:43..49: unused local variable `unused`
        "]],
    );
    check(
        "@SuppressWarnings(\"unused\") class C { private int dead; void m() { int unused = 1; } }",
        expect![""],
    );
}

#[test]
fn all_suppresses_every_rule() {
    // javac's catch-all, and the one name that is neither a rule nor a section.
    check(
        "@SuppressWarnings(\"all\") class C { private int dead; void m() { int unused = 1; if (true) { g(); } } void g() {} }",
        expect![""],
    );
}

#[test]
fn the_array_and_value_spellings_suppress() {
    // The three legal argument shapes reach the same map; `tests/../src/suppress.rs` pins the
    // extraction, and this pins that each one actually silences.
    let bare = "class C { void m() { int unused = 1; if (true) { g(); } } void g() {} }";
    assert!(!lint(bare).is_empty(), "the fixture must lint");
    for annotation in [
        "@SuppressWarnings({\"unused-variables\", \"constant-condition\"})",
        "@SuppressWarnings(value = \"all\")",
        "@SuppressWarnings(value = {\"unused\", \"suspicious\"})",
    ] {
        assert_eq!(
            lint(&format!("{annotation} {bare}")),
            "",
            "`{annotation}` suppressed nothing"
        );
    }
}

#[test]
fn a_suppression_covers_what_the_declaration_contains() {
    // Containment, over the host's whole significant span: one annotation on the type silences a
    // finding several levels down. Nesting needs no innermost-wins rule — `@SuppressWarnings` has
    // no negative form, so there is nothing an inner annotation could take back.
    check(
        "@SuppressWarnings(\"unused-variables\")\nclass C { class Inner { void m() { int unused = 1; } } }",
        expect![""],
    );
    // …and it covers only what it contains: the sibling method keeps its finding.
    check(
        "class C {\n  @SuppressWarnings(\"unused-variables\")\n  void m() { int a = 1; }\n  void n() { int b = 2; }\n}",
        expect![[r"
            unused-variables:93..94: unused local variable `b`
        "]],
    );
}

#[test]
fn an_unknown_suppression_name_changes_nothing() {
    // `unchecked`, `rawtypes`, `serial`, an IntelliJ inspection id: a real Java corpus is full of
    // names addressed to other tools, and JLS §9.6.4.5 leaves an unrecognized one to the
    // compiler's discretion. Ignoring it silently is that discretion — reporting it would make
    // every ported codebase noisy about annotations javac accepts.
    let src = "class C { @SuppressWarnings(\"unchecked\") void m() { int unused = 1; } }";
    check(
        src,
        expect![[r"
            unused-variables:56..62: unused local variable `unused`
        "]],
    );
}

#[test]
fn a_qualified_suppress_warnings_suppresses() {
    // `@java.lang.SuppressWarnings` is the same annotation, and the match is on the last segment.
    check(
        "class C { @java.lang.SuppressWarnings(\"unused\") void m() { int unused = 1; } }",
        expect![""],
    );
}

#[test]
fn an_import_cannot_be_suppressed_in_source() {
    // A stated limitation rather than a gap to fill. Java does not allow `@SuppressWarnings` on an
    // import declaration, and imports sit outside the type declaration that could carry one — so
    // even `"all"` on the class leaves `unused-imports` reporting. The `jalslint.toml` key is the
    // answer; a file-level jals syntax would be a second suppression language.
    check(
        "import java.util.List;\n@SuppressWarnings(\"all\")\nclass C {}\n",
        expect![[r"
            unused-imports:0..22: unused import `java.util.List`
        "]],
    );
}

#[test]
fn suppression_is_the_only_path_left_when_annotated_members_are_reported() {
    // `dead-code` answers this question twice over, and the two must not be confused. By default
    // *any* annotation exempts a private member (`AnnotatedMembers::Skip`: `@Inject` and friends
    // reach a member without naming it), so a `@SuppressWarnings` there is not what silences it.
    // A project that sets `annotated = "report"` has turned that reading off — and then suppression
    // is the only thing left, which is the configuration where this map is load-bearing for the
    // rule.
    let src = "class C { @SuppressWarnings(\"dead-code\") private int f; }";
    let mut config = Config::default();
    config.unused.dead_code.options.annotated = AnnotatedMembers::Report;
    let out = jals_exec::block_on_inline(LintOutput::lint_source(src, &config));
    assert!(
        out.diagnostics.iter().all(|d| d.rule != "dead-code"),
        "the suppression must survive `annotated = \"report\"`: {out:?}"
    );
    // …and without it the same configuration reports, so the fixture really is testing the
    // suppression rather than an exemption that was going to apply anyway.
    let out = jals_exec::block_on_inline(LintOutput::lint_source(
        "class C { @Deprecated private int f; }",
        &config,
    ));
    assert!(
        out.diagnostics.iter().any(|d| d.rule == "dead-code"),
        "{out:?}"
    );
}

#[test]
fn a_structural_attribute_error_is_not_suppressible() {
    // The one diagnostic outside the rule table. A malformed attribute is the failure the compile
    // frontend rejects a build with, not a judgement about the code, so it is fixed at `error` and
    // has no `jalslint.toml` key — and by the same argument no `@SuppressWarnings` reaches it.
    // Structural rather than a `rule == "cfg"` test: the filter runs before the `cfg` errors are
    // added at all.
    let src = "class C {\n  @SuppressWarnings(\"all\")\n  void m() {\n    #[nope]\n    int x = 1;\n  }\n}";
    let out = lint_with_cfg(src, &[]);
    assert!(
        out.contains("cfg:") && out.contains("unknown attribute `nope`"),
        "the fixed `cfg` diagnostic must survive `all`: {out}"
    );
}
