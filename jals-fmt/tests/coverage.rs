//! Every rule in `jals_config::fmt::Config` must actually reach the formatter.
//!
//! "All 189 rules are implemented" is not a claim to make in prose. This walks the **schema** — so
//! a rule added later is covered the moment it exists — moves each leaf away from its default one
//! at a time, and requires the formatter to notice.
//!
//! It mirrors `jals_fmt::import`'s `every_inventoried_option_is_modeled`, which does the same on
//! the way in: that test proves no vendor setting is missing from the native models, this one
//! proves no jals rule is missing from the engine.
//!
//! # Noticing
//!
//! A rule is noticed when some fixture's output **changes**, or when the rule is one that reads
//! input whitespace and the engine reports **rounding** it instead (`DESIGN.md` §17). A rule that
//! does neither is inert, and the test names it.
//!
//! # Prerequisites
//!
//! Some rules are genuinely conditional: `comments.width` does nothing until a reflow rule is on,
//! `layout.tab-width` does nothing under `indent-style = space`. Those are documented conditions,
//! not gaps, so each such rule names the base it needs. A prerequisite is a claim about the rule's
//! meaning — if one is wrong, the test still fails, just for the base instead.

use jals_config::fmt::{Config, ImportOrder, IndentStyle, WrapPolicy};
use serde_json::{Map, Value};

/// Java that exercises most of the language at once.
const KITCHEN_SINK: &str = r#"// A header comment long enough that reflowing it against the comment width would move its words.
package com.example.demo;

import java.util.List;
import java.util.Map;
import static java.lang.Math.PI;

/**
 * A type comment long enough to be worth reflowing, with prose that runs past the comment width so
 * the refill rules have something to do.
 * <p>A second paragraph immediately after the first, with no blank line between them at all.
 * <pre>
 *   preformatted   text   whose   spacing   matters
 * </pre>
 *
 * @param <T> the element type of this container
 * @throws Exception when the description is long enough to wrap onto a second line of its own
 * @return nothing at all
 */
@Deprecated
@SuppressWarnings(value = "unchecked")
public class Demo<T extends Comparable<T> & Cloneable> extends Base implements First, Second {
    /* a block comment whose prose is long enough that reflowing it against a width would move it */
    private static final int ALPHA = 0xff; // trailing
    private long beta = 10l;
    private int[] flat = {1, 2, 3};
    private int[] made = new int[] {4, 5, 6};
    private int inline = /*count=*/ 3;
    private int hugged = /* note */ 4;
    private double gamma = 1.;
    private int[] table = {
        1, 2, 3,
        4, 5, 6,
    };
    private Map<String, List<Integer>> lookup = null;

    static { register(); }

    Demo(int a, int b) throws IllegalStateException, IllegalArgumentException {
        super(a);
    }

    @Override
    public <R extends Number> R apply(@Nullable T first, T second, T third) throws Exception {
        int total = ((int) first.hashCode()) + second.hashCode() + (third.hashCode() & 0xff);
        int mixed = (total << 2) | (total >> 1) ^ (total & 3);
        boolean flag = total > 0 && mixed < 0 || total == mixed;
        String label = total > 0 ? "positive" : "negative";
        Runnable ref = Demo::register;
        int cell = table[total];
        label:
        for (int i = 0; i < total; i++) {
            if (i % 2 == 0) continue label;
            else break;
        }
        for (T item : List.of(first, second)) {
            assert item != null : "item";
        }
        {
        if (total > 0) report();
        else if (total < 0) report();
        else report();
        while (total > 0) total--;
        do total++; while (total < 0);
        for (int i = 0; i < 2; i++) report();
        for (T item : List.of(first)) report();

        report();

    }
        try (AutoCloseable one = openTheFirstResource(); AutoCloseable two = openSecondResource()) {
            synchronized (this) { call(first, second, third, label, total, ALPHA, beta, gamma); }
        } catch (IllegalStateException | IllegalArgumentException | NullPointerException e) {
            throw new RuntimeException(e);
        } finally {
            cleanup();
        }
        switch (total) {
            case 1:
            case 2:
                report();
                break;
            default:
                break;
        }
        if (label instanceof String s && total > 0) { report(); }
        return switch (total) {
            case 1 -> null;
            default -> null;
        };
    }

    interface Inner {
        int FIRST = 1;
        int SECOND = 2;

        void method();
    }

    enum Color { RED, GREEN, BLUE }

    record Point(int x, int y) {}

    @interface Marker {}
}
"#;

/// Constructs that only show their layout once they overflow the column limit.
const WRAPPING: &str = r#"class Overflow {
    @TheAnnotation(first = "aaaaaaaaaaaaaaaa", second = "bbbbbbbbbbbbbbbb", third = "cccccccccccc")
    private static final java.util.Map<java.lang.String, java.util.List<java.lang.Integer>> table = null;

    <TypeParameterOne extends Comparable<TypeParameterOne>, TypeParameterTwo> void declaration(
            TypeParameterOne firstParameter, TypeParameterTwo secondParameter, int thirdParameter)
            throws IllegalStateException, IllegalArgumentException, UnsupportedOperationException {
        int computed = firstOperandName + secondOperandName + thirdOperandName + fourthOperandName;
        String chained = builderFactory.newBuilder().withTheFirstOption().withTheSecondOption().withTheThirdOption().build();
        String tiny = api.withTheFirstOption().withTheSecondOption().withTheThirdOption().withTheFourthOption().build();
        String formatted = fmt("a format string %s with several placeholders %s and more %s", firstValue, secondValue, thirdValue);
        String conditional = computed > 0 ? "the affirmative branch text goes here" : "and the negative branch text goes over here";
        consume(/* note */ theFirstArgumentName, theSecondArgumentName, theThirdArgumentName, theFourthArgumentName, theFifth);
        assert computed > 0 : "a message long enough that the assert statement has to wrap somewhere";
        try (Resource firstResource = openFirst(); Resource secondResource = openSecondThing()) {
            switch (computed) {
                case AAAAAAAAAAAAAAAA, BBBBBBBBBBBBBBBB, CCCCCCCCCCCCCCCC, DDDDDDDDDDDDDDDD, EEEE:
                    break;
            }
        } catch (FirstExceptionType | SecondExceptionType | ThirdExceptionType | FourthType e) {
        }
        for (int index = 0; index < someRatherLongBoundExpression(computed); index += stepSize) {}
        Object result = switch (computed) { default -> theRatherLongResultExpressionGoesHere(); };
        if (result instanceof Point(int firstComponentName, int secondComponentName) && computed > 0) {}
        lambdaTaking((firstLambdaParameter, secondLambdaParameter) -> firstLambdaParameter + 1);
    }

    void annotated(@First @Second int parameter) {
        @Local int variable = 0;
    }
}
"#;

/// A `@formatter:off` region that must survive byte-identical.
const TAGGED: &str = "class T {\n  // @formatter:off\n  int   x   =   1;\n  // @formatter:on\n  int    y    =    2;\n}\n";

/// An unused import, imports out of order, and modifiers in non-canonical order.
const IMPORTS: &str = "package p;\n\nimport javax.tools.Tool;\nimport java.util.Map;\nimport java.util.List;\nimport static java.lang.Math.PI;\n\nclass T {\n  static final public List<String> xs = null;\n}\n";

/// A single string literal too long for its line, which is what `reflow-long-strings` splits.
/// A concatenation the author already broke into short pieces is *not* reflowed, so it would
/// leave the rule with nothing to do.
const LONG_STRING: &str = "class T {\n  void m() {\n    throw new RuntimeException(\"a single very long literal that runs well past the hundred column limit and then some more\");\n  }\n}\n";

/// Bodies the source itself wrote on one line, and empty ones.
const ONE_LINERS: &str = "class T { void m() { call(); } }\n\ninterface I {}\n\nenum E {}\n\nrecord R() {}\n\n@interface A {}\n\nclass U {\n  void n() {}\n\n  void o() { if (x) { y(); } else { z(); } for (;;) {} while (x) {} do {} while (x); }\n\n  Runnable r = () -> { run(); };\n\n  void p() { switch (x) { default: break; } }\n}\n";

/// Declarations packed with no separation, so every `[blank-lines]` rule has a gap to widen: a
/// documented field between undocumented ones, a Javadoc whose tags follow its description
/// immediately, argument-free and argument-carrying annotations, and a three-blank-line run for
/// the `max-*` caps to trim.
const PACKED: &str = r#"package p;



import java.util.List;

class Packed {
  int a = 1;
  /**
   * Documented.
   * @param x nothing
   */
  int b = 2;
  int c = 3;
  @Deprecated int d = 4;
  @SuppressWarnings("x") int e = 5;
}



class Second {}
"#;

/// The Javadoc shapes whose rules are about *gaps and units* rather than about width: a blank
/// line between two block tags, a blank line between a doc comment and what it documents, and an
/// inline `{@code …}` sitting exactly where a refill wants to break.
const JAVADOC: &str = r"/** A header comment, so the type's own Javadoc is not the header one. */
package p;

/**
 * A description whose last sentence runs long enough that the inline tag near the column limit
 * has to move, which is the only place {@code breakInsideInlineTags} can be seen deciding.
 * <ul>
 * <li>a list item, so the list indent and the gap above the list have somewhere to appear
 * <li>a second one
 * </ul>
 *
 * <table>
 * <tr>
 * <td>a cell, so a table has a row to be read as HTML rather than as a preformatted region
 * </tr>
 * </table>
 *
 * @param x the first
 *
 * @throws IllegalStateException when the description is long enough to wrap onto a second line
 * @since 1.0
 */
class Documented {

  /** Documented, with a blank line before the field it documents. */

  int a = 1;

  /**
   * A fenced snippet whose own indentation is not the configured one.
   *
   * <pre>{@code
   * if (a > 0) {
   *   report();
   * }
   * }</pre>
   */
  void m() {}
}
";

/// A file that does not end with a newline, so `insert-final-newline` has one to add. Every other
/// fixture already ends with one, and adding a newline that is there changes nothing.
const NO_FINAL_NEWLINE: &str = "class Bare {}";

/// Every fixture; a rule may be noticed on any of them.
const FIXTURES: [&str; 9] = [
    KITCHEN_SINK,
    WRAPPING,
    TAGGED,
    IMPORTS,
    LONG_STRING,
    ONE_LINERS,
    PACKED,
    JAVADOC,
    NO_FINAL_NEWLINE,
];

/// An import-group list that leaves the static block's position to `static-first`.
fn alloc_groups() -> Vec<String> {
    vec!["java.".into(), "*".into()]
}

/// Format `src` under `config`, and require the formatter to have vouched for the result.
///
/// A run the fail-safe refused is not a formatting of `src` — it *is* `src`. This test reads "the
/// output did not change" as "the rule is inert", so a fallback would surface here as exactly the
/// failure the test exists to detect, named against a rule that is in fact implemented. The
/// progress property `jals-fmt` states in `src/invariants.rs` therefore has to hold here too, and
/// this is the wider sweep of the two: that corpus is five profiles, this one moves every schema
/// leaf off its default in turn.
///
/// Asserted in the helper rather than at the two call sites so a third one cannot skip it.
fn format(src: &str, config: &Config) -> jals_fmt::FormatOutput {
    let out = jals_exec::block_on_inline(jals_fmt::FormatOutput::format_source(src, config));
    assert!(
        !out.fell_back(),
        "the fail-safe refused its own output, so this run formatted nothing and every rule under \
         it would be reported inert. Either a pass changed a token no `OPERATIONS` row licenses, \
         or a row is missing.\n--- config off its default ---\n  {}\n--- source ---\n{src}",
        off_default(config).join("\n  "),
    );
    out
}

/// The `section.key = value` pairs where `config` differs from [`Config::default`].
///
/// The sweep moves one leaf at a time, so this is normally one entry — plus whatever
/// [`base_for`] had to turn on first. Enough to reproduce a failure without printing all 189 rules.
fn off_default(config: &Config) -> Vec<String> {
    let Value::Object(current) = serde_json::to_value(config).expect("serializable") else {
        panic!("the config is a table of tables");
    };
    let defaults = schema();
    let mut out = Vec::new();
    for (section, values) in &current {
        let (Some(values), Some(Value::Object(defaults))) =
            (values.as_object(), defaults.get(section))
        else {
            continue;
        };
        for (key, value) in values {
            if defaults.get(key) != Some(value) {
                out.push(format!("{section}.{key} = {value}"));
            }
        }
    }
    out
}

/// The default config as a two-level JSON object.
fn schema() -> Map<String, Value> {
    let Value::Object(root) = serde_json::to_value(Config::default()).expect("serializable") else {
        panic!("the config is a table of tables");
    };
    root
}

/// The config a rule needs before it can do anything, as JSON.
///
/// Each entry is a documented condition on the rule, not a workaround: a comment rule is inert
/// until reflow is on, a tab width is inert under space indentation, and an import group is inert
/// until imports are grouped.
fn base_for(section: &str, key: &str) -> Value {
    let mut config = Config::default();
    match (section, key) {
        // These two decide where a parameter comment goes and how it is spelled; reflow would
        // rewrite it either way and mask the difference.
        ("comments", "inline-block-comments" | "normalize-parameter-comments") => {}
        ("comments", _) => {
            config.comments.format_line = true;
            config.comments.format_block = true;
            config.comments.format_javadoc = true;
            config.comments.format_header = true;
        }
        ("layout", "tab-width") => config.layout.indent_style = IndentStyle::Tab,
        // The first call's break only exists once the chain wraps one call per line.
        ("wrapping", "wrap-first-method-in-chain") => {
            config.wrapping.method_chain = WrapPolicy::IfLongPerItem;
        }
        ("layout", "indent-empty-lines") => config.layout.trim_trailing_whitespace = false,
        ("layout", "trim-trailing-whitespace") => config.layout.indent_empty_lines = true,
        ("layout", "formatter-off-tag" | "formatter-on-tag") => config.layout.formatter_tags = true,
        ("imports", "groups") | ("blank-lines", "between-import-groups") => {
            config.imports.order = ImportOrder::Group;
        }
        // `static-first` only decides where the static block goes when `groups` has not pinned
        // it, which the default list does.
        ("imports", "static-first") => {
            config.imports.order = ImportOrder::Group;
            config.imports.groups = alloc_groups();
        }
        _ => {}
    }
    serde_json::to_value(config).expect("serializable")
}

/// The values worth trying for a leaf, each expected to move the formatter.
///
/// Booleans and counts move mechanically. A `String` leaf is an enum variant or free text, and the
/// variants are the part a schema walk cannot guess — a new enum-valued rule fails here until its
/// variants are named, which is the point. `preserve`-style variants get their own entry because
/// the engine answers them with a rounding warning rather than a different layout.
fn candidates(section: &str, key: &str, current: &Value) -> Vec<Value> {
    match current {
        Value::Bool(value) => vec![Value::Bool(!value)],
        // `continuation-indent` is the one optional leaf: unset means "track indent-width".
        Value::Number(_) | Value::Null => vec![Value::from(3), Value::from(0)],
        Value::Array(_) => vec![Value::from(vec![
            Value::from("static"),
            Value::from("javax."),
            Value::from("*"),
        ])],
        Value::String(_) => variants(section, key)
            .into_iter()
            .map(Value::from)
            .collect(),
        other @ Value::Object(_) => panic!("unexpected leaf shape for {section}.{key}: {other}"),
    }
}

/// The non-default variants of an enum- or text-valued leaf.
fn variants(section: &str, key: &str) -> Vec<&'static str> {
    match (section, key) {
        ("layout", "indent-style") => vec!["tab", "mixed"],
        ("layout", "line-ending") => vec!["crlf"],
        ("layout", "formatter-off-tag") => vec!["@fmt:off"],
        ("layout", "formatter-on-tag") => vec!["@fmt:on"],
        ("imports", "order") => vec!["group", "sort"],
        ("comments", "paragraph-tags") => vec!["own-line", "authored"],
        ("comments", "tag-alignment") => vec!["grouped", "all"],
        ("literals", "hex-case" | "suffix-case") => vec!["upper", "lower"],
        ("literals", "float-trailing-zero") => vec!["always", "never"],
        ("braces", key) if key.starts_with("force-") => vec!["always", "if-multiline"],
        ("braces", key) if key.starts_with("keep-") => {
            vec!["never", "always", "if-single-item", "preserve"]
        }
        ("braces", _) => vec!["next-line", "next-line-shifted", "next-line-on-wrap"],
        ("wrapping", "inline-argumentless-annotations") => vec!["locals", "declarations"],
        ("wrapping", key) if key.starts_with("paren-") => {
            vec!["separate-lines", "separate-lines-if-wrapped", "preserve"]
        }
        ("wrapping", _) => vec!["always-per-item", "never", "if-long-per-item"],
        _ => panic!("no non-default variant known for {section}.{key}"),
    }
}

/// Rules the single engine cannot reach, with the reason.
///
/// This list is a claim about the *engine*, not an excuse: each entry names a layout the engine
/// never produces, so the rule has no position to decide. Adding an entry is a design decision
/// and belongs in `DESIGN.md` §18.2 alongside the other permanent differences.
const UNREACHABLE: [(&str, &str); 1] = [(
    "spacing",
    // The engine always breaks after a colon-form `case` label, so nothing ever follows the
    // colon on its line for this rule to space.
    "after-case-colon",
)];

#[test]
fn every_rule_reaches_the_formatter() {
    let mut inert = Vec::new();
    let mut checked = 0usize;

    for (section, values) in schema() {
        let Value::Object(values) = values else {
            panic!("the config is a table of tables");
        };
        let base = base_for(&section, "");
        for (key, _default) in values {
            checked += 1;
            let base = if base_for(&section, &key) == base {
                base.clone()
            } else {
                base_for(&section, &key)
            };
            let baseline: Config =
                serde_json::from_value(base.clone()).expect("the base deserializes");
            let outputs: Vec<String> = FIXTURES
                .iter()
                .map(|src| format(src, &baseline).formatted)
                .collect();

            // The value to move *away from* is the base's, not the schema default's: a rule with
            // a prerequisite is compared against the base that enables it.
            let current = base[&section][&key].clone();
            let noticed = candidates(&section, &key, &current)
                .into_iter()
                .any(|value| {
                    if value == current {
                        return false;
                    }
                    let mut root = base.clone();
                    root[&section][&key] = value;
                    let Ok(config) = serde_json::from_value::<Config>(root) else {
                        return false;
                    };
                    FIXTURES.iter().zip(&outputs).any(|(src, expected)| {
                        let out = format(src, &config);
                        out.formatted != *expected || out.warnings.iter().any(|w| w.range.is_none())
                    })
                });
            let excused = UNREACHABLE.contains(&(section.as_str(), key.as_str()));
            assert!(
                !(noticed && excused),
                "{section}.{key} is listed as unreachable but the formatter noticed it",
            );
            if !noticed && !excused {
                inert.push(alloc_key(&section, &key));
            }
        }
    }

    assert!(
        inert.is_empty(),
        "{} of {checked} rules changed nothing:\n  {}",
        inert.len(),
        inert.join("\n  "),
    );
}

/// `section.key`, for the failure message.
fn alloc_key(section: &str, key: &str) -> String {
    format!("{section}.{key}")
}

#[test]
fn the_schema_is_the_documented_size() {
    let total: usize = schema()
        .values()
        .map(|section| section.as_object().map_or(0, Map::len))
        .sum();
    assert_eq!(
        total, 189,
        "the rule set is documented as 189 keys in jals-fmt/MAPPING.md",
    );
}
