//! Schema tests for `jalslint.toml`: the two spellings of a rule key, the patch semantics that let
//! the table form omit `level`, and the unknown-key policy.

use super::*;

#[test]
fn defaults_are_the_built_in_levels() {
    let c = Config::default();
    assert_eq!(c.correctness.cannot_resolve.level, LintLevel::Error);
    assert_eq!(c.correctness.type_mismatch.level, LintLevel::Warn);
    assert_eq!(c.restriction.print_to_console.level, LintLevel::Allow);
    assert!(c.unknown_keys().is_empty());
}

#[test]
fn a_bare_string_sets_the_level() {
    let c: Config = toml::from_str(
        r#"
        [style]
        wildcard-import = "error"

        [unused]
        dead-code = "allow"
        "#,
    )
    .unwrap();
    assert_eq!(c.style.wildcard_import.level, LintLevel::Error);
    assert_eq!(c.unused.dead_code.level, LintLevel::Allow);
    // Untouched rules keep their built-in level.
    assert_eq!(c.style.missing_braces.level, LintLevel::Warn);
}

#[test]
fn a_table_sets_options_beside_the_level() {
    let c: Config = toml::from_str(
        r#"
        [style]
        missing-braces = { level = "error", policy = "multi-line" }
        "#,
    )
    .unwrap();
    assert_eq!(c.style.missing_braces.level, LintLevel::Error);
    assert_eq!(
        c.style.missing_braces.options.policy,
        BracePolicy::MultiLine
    );
}

#[test]
fn a_table_without_a_level_keeps_the_built_in_one() {
    // The whole reason a rule key deserializes as a patch: a config that sets an option must not
    // have to restate a default it did not choose, where it would silently go stale.
    let c: Config = toml::from_str(
        r#"
        [naming.naming-convention]
        fields = "any"
        "#,
    )
    .unwrap();
    assert_eq!(c.naming.naming_convention.level, LintLevel::Warn);
    assert_eq!(c.naming.naming_convention.options.fields, Case::Any);
    // ...and the option keys it did not write keep theirs.
    assert_eq!(
        c.naming.naming_convention.options.types,
        Case::UpperCamelCase
    );
}

#[test]
fn an_unknown_key_is_kept_and_named() {
    // One stale key must not stop the rest of the file from loading, so it is recorded rather than
    // rejected — and recorded rather than dropped, so a host can report it.
    let c: Config = toml::from_str(
        r#"
        [rules]
        wildcard-import = "error"

        [style]
        no-such-rule = "warn"
        missing-braces = "error"
        "#,
    )
    .unwrap();
    assert_eq!(c.style.missing_braces.level, LintLevel::Error);
    assert_eq!(c.unknown_keys(), ["rules", "style.no-such-rule"]);
}

#[test]
fn each_restriction_rule_carries_its_own_built_in_level() {
    // The section once gave every rule `Allow`; it no longer does, so the levels are per rule and
    // `jals-lint/tests/registry.rs` pins the whole set. See the section's module docs.
    let restriction = Restriction::default();
    assert_eq!(restriction.print_to_console.level, LintLevel::Allow);
    assert_eq!(restriction.implicit_this.level, LintLevel::Warn);
    assert_eq!(Restriction::RULES, ["print-to-console", "implicit-this"]);
}

#[test]
fn the_serialized_shape_follows_the_options_type() {
    // A bare level for a rule that takes no options, the table form for one that does — whatever
    // the values are. That is what lets `jals-lint/tests/registry.rs` find every option key by
    // walking one serialized config instead of listing them.
    let config = Config::default();
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(
        value["correctness"]["cannot-resolve"],
        serde_json::json!("error")
    );
    assert_eq!(
        value["style"]["missing-braces"],
        serde_json::json!({ "level": "warn", "policy": "always" })
    );
    let mut config = config;
    config.style.missing_braces.options.policy = BracePolicy::MultiLine;
    assert_eq!(
        serde_json::to_value(&config).unwrap()["style"]["missing-braces"],
        serde_json::json!({ "level": "warn", "policy": "multi-line" })
    );
}

#[test]
fn every_serialized_key_is_a_rule_its_section_declares() {
    // The schema's own half of the registry check: a field whose serde name drifted from the name
    // literal that declares it would show up here as a key no `RULES` entry matches. The other
    // half — that a declared rule reaches the engine — is `jals-lint/tests/registry.rs`.
    let value = serde_json::to_value(Config::default()).unwrap();
    let sections: &[(&str, &[&str])] = &[
        ("correctness", Correctness::RULES),
        ("compatibility", Compatibility::RULES),
        ("suspicious", Suspicious::RULES),
        ("unused", Unused::RULES),
        ("complexity", Complexity::RULES),
        ("performance", Performance::RULES),
        ("style", Style::RULES),
        ("naming", Naming::RULES),
        ("documentation", Documentation::RULES),
        ("restriction", Restriction::RULES),
    ];
    assert_eq!(
        sections.len(),
        Category::ALL.len(),
        "every category is one section"
    );
    for (section, rules) in sections {
        let table = value[section].as_object().expect("a section is a table");
        // JSON objects come back sorted, the declaration order is not; compare as sets.
        let keys: alloc::collections::BTreeSet<&str> =
            table.keys().map(alloc::string::String::as_str).collect();
        let declared: alloc::collections::BTreeSet<&str> = rules.iter().copied().collect();
        assert_eq!(keys, declared, "`[{section}]` keys");
    }
}
