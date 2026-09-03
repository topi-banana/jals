//! The rule registry against the `jalslint.toml` schema, and every schema key against the engine.
//!
//! Three claims that `jals-lint/README.md` and `jals-lint/MAPPING-rustc-clippy.md` make in prose
//! are tests here instead:
//!
//! - **The registry and the schema are the same set.** A rule implemented and not declared cannot
//!   be configured; a rule declared and not implemented is a key that silences nothing. Both fail
//!   here, by name.
//! - **The default level set is what the documentation says it is.** Pinned as a list, so changing
//!   a built-in level is a deliberate edit to this file rather than a silent drift — the analogue
//!   of `jals_fmt::passes::token_license`'s `the_default_config_enables_exactly_these_rows`.
//! - **Every option reaches the engine.** The sweep walks the *serialized schema*, so an option
//!   added later is covered the moment it exists, moves each one off its default in turn, and
//!   requires the linter to notice. A key that reaches no rule is named. This is
//!   `jals-fmt/tests/coverage.rs`'s property, for the linter.

use jals_config::lint::Config;
use jals_config::{Category, Feature, FeatureSet, LintLevel};
use jals_lint::{LintOutput, RuleInfo};
use serde_json::{Map, Value};

/// Java that exercises every rule with options at once, so moving any one option off its default
/// has something to change.
const KITCHEN_SINK: &str = r#"import java.util.*;
import static java.lang.Math.*;

/***/
class Sink {
  private class bad_Nested {}

  private static final int Bad_Constant = 1;
  private int Bad_Field = 2;
  @Deprecated private int injected = 3;
  private static int Bad_Static = 4;
  // `nullness-mismatch`: `maybe` is the nullable source, `required` the non-null slot it flows
  // into, and `confused` the declaration that contradicts itself — one per option key.
  @Nullable @NonNull private String confused;

  @Nullable private String maybe() { return null; }

  private String required() { return maybe(); }

  int Bad_Method(int Bad_Param) {
    int Bad_Local = 0;
    int _unused = 1;
    Bad_Field = Bad_Local;
    if (Bad_Param > 0) if (Bad_Local > 0) System.out.println("x");
    if (Bad_Param > 1)
      return 1;
    try {
      return new Integer(2);
    } catch (RuntimeException ignored) {
    }
    return 0;
  }

  void explained() {
    try {
      hashCode();
    } catch (RuntimeException e) {
      // deliberately ignored, which `commented = "accept"` honours and `"reject"` does not
    }
  }
}
"#;

/// The findings `config` produces over the fixture, as `offset rule: message` lines, sorted.
///
/// The offset and the message both travel with the rule name: an option that suppresses *one* of
/// three identical findings, and one that changes only the wording — a naming rule's expected
/// case, say — are each a change the engine noticed, and a key that folded them away would let an
/// implemented option read as inert.
fn findings(config: &Config) -> Vec<String> {
    let out = jals_exec::block_on_inline(LintOutput::lint_source(KITCHEN_SINK, config));
    let mut lines: Vec<String> = out
        .diagnostics
        .iter()
        .map(|d| format!("{:>5} {}: {}", d.range.start, d.rule, d.message))
        .collect();
    lines.sort_unstable();
    lines
}

/// The whole schema as JSON: a table of sections, each a table of rules.
fn schema() -> Map<String, Value> {
    let Value::Object(root) = serde_json::to_value(Config::default()).expect("serializable") else {
        panic!("the config is a table of tables");
    };
    root
}

/// A non-default value for the option `section.rule.key`.
///
/// Hand-written, like `jals-fmt/tests/coverage.rs`'s `variants`: JSON does not carry an enum's
/// other variants, and an option whose alternatives nobody can name is an option nobody can set.
fn variant(section: &str, rule: &str, key: &str) -> Value {
    match (section, rule, key) {
        ("suspicious", "empty-catch", "commented") => Value::from("reject"),
        ("suspicious", "empty-catch", "allowed-names") => Value::from(vec!["ignored"]),
        ("unused", "unused-variables", "ignore-prefix") => Value::from(""),
        ("unused", "dead-code", "annotated") => Value::from("report"),
        ("style", "wildcard-import", "static-imports") => Value::from("allow"),
        ("style", "missing-braces", "policy") => Value::from("multi-line"),
        ("naming", "naming-convention", _) => Value::from("any"),
        ("correctness", "nullness-mismatch", "default") => Value::from("unspecified"),
        // Emptying either list un-teaches the annotation, which is a different finding set rather
        // than a smaller one: `maybe` stops being a nullable source *and* starts being a non-null
        // slot its own `return null` violates.
        ("correctness", "nullness-mismatch", "nullable" | "non-null") => {
            Value::from(Vec::<&str>::new())
        }
        ("restriction", "print-to-console", "streams") => Value::from("stderr"),
        ("restriction", "implicit-this", "scope") => Value::from("shadowed-only"),
        _ => panic!("no non-default value known for [{section}] {rule}.{key}"),
    }
}

/// The config every option sweep starts from: every rule on, so a rule that is `allow` by default
/// still has findings for its option to change.
fn all_enabled() -> Config {
    let mut config = Config::default().with_features(FeatureSet::resolve(&[Feature::Java25]));
    config.restriction.print_to_console.level = LintLevel::Warn;
    config
}

#[test]
fn the_fixture_parses_cleanly() {
    // A precondition of the sweep rather than a nicety. `nullness-mismatch` carries
    // `needs_clean_parse`, and the driver skips such a rule outright on a broken parse — so one
    // syntax error in the fixture would take all three of its option keys out of the sweep and
    // report them, by name, as options that reach no rule.
    let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse(KITCHEN_SINK));
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
}

#[test]
fn the_registry_and_the_schema_are_the_same_set() {
    let schema = schema();
    for category in Category::ALL {
        let section = category.config_name();
        let declared: Vec<&str> = schema[section]
            .as_object()
            .expect("a section is a table")
            .keys()
            .map(String::as_str)
            .collect();
        let mut implemented: Vec<&str> = RuleInfo::all()
            .filter(|rule| rule.category == *category)
            .map(|rule| rule.name)
            .collect();
        implemented.sort_unstable();
        assert_eq!(
            implemented, declared,
            "`[{section}]`: the rules the engine runs and the keys the schema declares"
        );
    }
}

#[test]
fn rule_names_are_unique_across_sections() {
    // The name is what a diagnostic carries, so two rules sharing one would make a finding name a
    // key that does not configure it.
    let mut names: Vec<&str> = RuleInfo::all().map(|rule| rule.name).collect();
    let total = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), total, "duplicate rule name");
}

#[test]
fn no_rule_is_named_cfg() {
    // `cfg` is the one diagnostic the engine emits outside the rule table — a malformed attribute
    // is a build-blocking error, not a configurable lint — so it must not collide with a rule
    // name, which would make it look configurable.
    assert!(RuleInfo::all().all(|rule| rule.name != "cfg"));
}

#[test]
fn the_default_config_enables_exactly_these_rules() {
    // Changing a built-in level is an edit to this list, never a silent drift. `error` is for a
    // finding the compiler itself would refuse; everything else that is on is `warn`.
    let level_of = |want: LintLevel| {
        let mut names: Vec<&str> = RuleInfo::all()
            .filter(|rule| rule.default_level == want)
            .map(|rule| rule.name)
            .collect();
        names.sort_unstable();
        names
    };
    assert_eq!(
        level_of(LintLevel::Error),
        [
            "attribute",
            "cannot-resolve",
            "compact-source-file",
            "grouped-import",
            "module-import",
        ]
    );
    assert_eq!(
        level_of(LintLevel::Warn),
        [
            "boxed-primitive-constructor",
            "collapsible-if",
            "constant-condition",
            "dead-code",
            "empty-catch",
            "empty-javadoc",
            "implicit-this",
            "missing-braces",
            "naming-convention",
            "nullness-mismatch",
            "type-mismatch",
            "unreported-exception",
            "unused-imports",
            "unused-variables",
            "wildcard-import",
        ]
    );
    assert_eq!(level_of(LintLevel::Allow), ["print-to-console"]);
}

#[test]
fn every_option_reaches_the_engine() {
    // The sweep: one option off its default at a time, over a fixture that exercises every rule
    // that has options. A key the engine never reads produces identical findings and is named.
    let base = all_enabled();
    let baseline = findings(&base);
    let schema = schema();
    let mut swept = 0;
    for (section, rules) in &schema {
        let Some(rules) = rules.as_object() else {
            continue; // `features` and the unknown-key record are `serde(skip)`; nothing else is scalar
        };
        for (rule, value) in rules {
            // A rule with no options serializes as a bare level string — nothing to sweep.
            let Some(table) = value.as_object() else {
                continue;
            };
            for key in table.keys().filter(|key| *key != "level") {
                let mut document = serde_json::to_value(&base).expect("serializable");
                document[section][rule][key] = variant(section, rule, key);
                let config: Config =
                    serde_json::from_value(document).expect("the schema round-trips");
                let config = config.with_features(base.features);
                assert_ne!(
                    findings(&config),
                    baseline,
                    "`[{section}] {rule}.{key}` changed nothing: the option reaches no rule"
                );
                swept += 1;
            }
        }
    }
    assert!(swept >= 10, "the sweep found only {swept} options");
}
