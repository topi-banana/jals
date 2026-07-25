//! Emitter tests.
//!
//! Two of these are structural rather than behavioral: [`the_schema_is_two_levels_deep`] and
//! [`sections_covers_every_key`] pin the depth and the width of the shape the emitter assumes. The
//! rest are round trips — emit, parse, compare — because `Config` ignores unknown keys, so a
//! generated file that drifted from the schema would still parse and silently yield defaults.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use jals_config::fmt::{
    BraceStyle, Config, HexLiteralCase, ImportOrder, KeepOnOneLine, LineEnding, ParenPositions,
    WrapPolicy,
};
use serde_json::Value;

use super::toml_out::Toml;
use super::{MigrationWarning, MigrationWarningKind, Provenance};
use crate::import::{ConfigImporter, EclipsePrefs, IntellijEditorConfig};

/// Shared inputs for the emitter tests.
struct Fixture;

impl Fixture {
    /// A provenance whose rendering is not the point of the test using it.
    fn provenance() -> Provenance {
        Provenance {
            source: ".settings/org.eclipse.jdt.core.prefs".to_owned(),
            tool: "eclipse",
            version: None,
        }
    }

    /// Emit `config` with no warnings and parse the result straight back.
    fn round_trip(config: &Config) -> Config {
        let text = Self::provenance().jalsfmt_toml(config, &[]);
        toml::from_str(&text)
            .unwrap_or_else(|err| panic!("generated config should parse: {err}\n{text}"))
    }

    /// A config with at least one non-default key in every section, chosen to exercise every
    /// value shape the emitter can meet: bool, integer, `Option`, enum, string, and string list.
    fn every_section_touched() -> Config {
        let mut config = Config::default();
        config.layout.indent_width = 2;
        config.layout.max_width = 120;
        config.layout.continuation_indent = Some(8);
        config.layout.line_ending = LineEnding::Crlf;
        // A tag carrying every character class the TOML escaper has to handle.
        config.layout.formatter_off_tag = "off \"quoted\" \\ back\ttab\u{1}".to_owned();
        config.blank_lines.max_in_code = 3;
        config.braces.type_declaration = BraceStyle::NextLine;
        config.wrapping.call_arguments = WrapPolicy::AlwaysPerItem;
        config.spacing.around_assignment_operators = false;
        config.comments.format_javadoc = true;
        config.imports.order = ImportOrder::Group;
        config.imports.groups = vec!["java.".to_owned(), "\"odd\".".to_owned(), "*".to_owned()];
        config.literals.hex_case = HexLiteralCase::Upper;
        config
    }
}

#[test]
fn default_config_emits_only_a_header() {
    let text = Fixture::provenance().jalsfmt_toml(&Config::default(), &[]);

    assert!(
        !text.contains('['),
        "a default config should write no section: {text}"
    );
    assert!(text.ends_with('\n'), "the file should end in a newline");
    // Still a real, parsable file — a host writes it so the next run discovers it and stops
    // re-detecting.
    let parsed: Config = toml::from_str(&text).expect("a header-only file should parse");
    assert_eq!(parsed, Config::default());
}

#[test]
fn the_header_does_not_collide_with_the_documented_defaults_marker() {
    // `jals-tests` picks the documented *defaults* sample out of a Markdown page by this exact
    // string. A generated example pasted into the README must not be mistaken for it.
    let text = Fixture::provenance().jalsfmt_toml(&Fixture::every_section_touched(), &[]);
    assert!(!text.contains("# jalsfmt.toml"), "{text}");
}

#[test]
fn every_section_round_trips() {
    let config = Fixture::every_section_touched();
    let text = Fixture::provenance().jalsfmt_toml(&config, &[]);

    // Every section is represented, so the round trip below covers all eight.
    for section in Toml::SECTIONS {
        assert!(
            text.contains(&alloc::format!("[{section}]")),
            "{section} should be written: {text}"
        );
    }
    assert_eq!(Fixture::round_trip(&config), config);
}

#[test]
fn only_the_changed_keys_of_a_section_are_written() {
    let mut config = Config::default();
    config.layout.max_width = 120;

    let text = Fixture::provenance().jalsfmt_toml(&config, &[]);

    assert!(text.contains("max-width = 120"), "{text}");
    // `indent-width` is untouched, so it stays out of the file (DESIGN.md §15 P-gen-6).
    assert!(!text.contains("indent-width"), "{text}");
    assert!(!text.contains("[braces]"), "{text}");
}

#[test]
fn the_schema_is_two_levels_deep() {
    // The one structural invariant the emitter rests on: a root of section tables, each holding
    // only leaves. A section that grew a nested struct would need real sub-table handling, so
    // fail loudly here rather than emit TOML that does not round-trip.
    let Value::Object(root) = serde_json::to_value(Config::default()).expect("config serializes")
    else {
        panic!("the config root should be a table");
    };
    for (section, values) in &root {
        let Value::Object(values) = values else {
            panic!("[{section}] should be a table");
        };
        for (key, value) in values {
            assert!(
                !value.is_object(),
                "{section}.{key} is a nested table; the emitter only handles two levels"
            );
        }
    }
}

#[test]
fn sections_covers_every_key() {
    // `SECTIONS` hardcodes the kebab-case table names. A typo there drops a whole section from
    // every generated file without producing any error, so pin it against the schema itself.
    let Value::Object(root) = serde_json::to_value(Config::default()).expect("config serializes")
    else {
        panic!("the config root should be a table");
    };
    let mut listed: Vec<&str> = Toml::SECTIONS.to_vec();
    listed.sort_unstable();
    let mut actual: Vec<&str> = root.keys().map(String::as_str).collect();
    actual.sort_unstable();
    assert_eq!(listed, actual);
}

#[test]
fn section_order_is_declaration_order() {
    let text = Fixture::provenance().jalsfmt_toml(&Fixture::every_section_touched(), &[]);

    let written: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix('[')?.strip_suffix(']'))
        .collect();
    assert_eq!(written, Toml::SECTIONS);
}

#[test]
fn the_provenance_and_warnings_reach_the_header() {
    let provenance = Provenance {
        source: "formatter.xml".to_owned(),
        tool: "eclipse",
        version: Some("23".to_owned()),
    };
    let warnings = [MigrationWarning::ambiguous(
        "formatter.xml",
        "the file declares 2 profiles and their settings were merged",
    )];

    let text = provenance.jalsfmt_toml(&Config::default(), &warnings);

    assert!(
        text.contains("# Generated by jals from formatter.xml (eclipse 23)."),
        "{text}"
    );
    assert!(
        text.contains("# warning: ambiguous: formatter.xml: the file declares 2 profiles"),
        "{text}"
    );
    // A warning line must stay inside its comment, or the rest of it becomes TOML.
    for line in text.lines().take_while(|line| !line.starts_with('[')) {
        assert!(
            line.is_empty() || line.starts_with('#'),
            "header line escaped its comment: {line}"
        );
    }
}

#[test]
fn an_imported_eclipse_config_round_trips() {
    let prefs = "\
eclipse.preferences.version=1
org.eclipse.jdt.core.formatter.lineSplit=120
org.eclipse.jdt.core.formatter.tabulation.size=2
org.eclipse.jdt.core.formatter.tabulation.char=space
org.eclipse.jdt.core.formatter.brace_position_for_type_declaration=next_line
";
    let config = EclipsePrefs::import(prefs).expect("the prefs fixture should import");

    assert_eq!(config.layout.max_width, 120);
    assert_eq!(config.layout.indent_width, 2);
    assert_eq!(config.braces.type_declaration, BraceStyle::NextLine);
    // The projection and the emitter cannot drift: what was imported is what is written back.
    assert_eq!(Fixture::round_trip(&config), config);
}

#[test]
fn an_imported_editorconfig_round_trips() {
    let editorconfig = "\
root = true
[*.java]
indent_style = space
indent_size = 2
max_line_length = 120
insert_final_newline = true
";
    let config =
        IntellijEditorConfig::import(editorconfig).expect("the editorconfig fixture should import");

    assert_eq!(config.layout.indent_width, 2);
    assert_eq!(config.layout.max_width, 120);
    assert_eq!(Fixture::round_trip(&config), config);
}

#[test]
fn rounding_warnings_are_empty_for_the_defaults() {
    // `wrapping.wrap-long-lines` defaults to `false`, which *is* the value §17 rounds away — so
    // without the default check this would fire on every config ever generated.
    assert_eq!(MigrationWarning::rounding(&Config::default()), []);
}

#[test]
fn rounding_warnings_name_every_section_17_row() {
    // Every §17 row at once, in the order the table lists them. The narrower test below exercises
    // one row per family; this is what keeps a row from being dropped or misspelled silently.
    let mut config = Config::default();
    for keep in [
        &mut config.braces.keep_type_body_on_one_line,
        &mut config.braces.keep_method_body_on_one_line,
        &mut config.braces.keep_block_on_one_line,
        &mut config.braces.keep_lambda_body_on_one_line,
        &mut config.braces.keep_switch_body_on_one_line,
        &mut config.braces.keep_enum_declaration_on_one_line,
        &mut config.braces.keep_record_declaration_on_one_line,
        &mut config.braces.keep_annotation_declaration_on_one_line,
    ] {
        *keep = KeepOnOneLine::Preserve;
    }
    for paren in [
        &mut config.wrapping.paren_method_declaration,
        &mut config.wrapping.paren_method_invocation,
        &mut config.wrapping.paren_control,
        &mut config.wrapping.paren_annotation,
        &mut config.wrapping.paren_lambda,
        &mut config.wrapping.paren_record,
    ] {
        *paren = ParenPositions::Preserve;
    }
    config.wrapping.join_wrapped_lines = false;
    config.wrapping.wrap_long_lines = false;
    config.comments.preserve_line_breaks = true;

    let subjects: Vec<String> = MigrationWarning::rounding(&config)
        .iter()
        .map(|warning| warning.subject.clone())
        .collect();

    // Sixteen rows, not seventeen: `wrapping.wrap-long-lines` cannot fire while its default is
    // `false`, because `false` is both the value §17 rounds away and the default. It becomes
    // reachable only if that default ever flips.
    assert_eq!(
        subjects,
        [
            "braces.keep-type-body-on-one-line",
            "braces.keep-method-body-on-one-line",
            "braces.keep-block-on-one-line",
            "braces.keep-lambda-body-on-one-line",
            "braces.keep-switch-body-on-one-line",
            "braces.keep-enum-declaration-on-one-line",
            "braces.keep-record-declaration-on-one-line",
            "braces.keep-annotation-declaration-on-one-line",
            "wrapping.paren-method-declaration",
            "wrapping.paren-method-invocation",
            "wrapping.paren-control",
            "wrapping.paren-annotation",
            "wrapping.paren-lambda",
            "wrapping.paren-record",
            "wrapping.join-wrapped-lines",
            "comments.preserve-line-breaks",
        ]
    );
}

#[test]
fn rounding_warnings_cover_the_section_17_table() {
    let mut config = Config::default();
    config.braces.keep_block_on_one_line = KeepOnOneLine::Preserve;
    config.wrapping.paren_control = ParenPositions::Preserve;
    config.wrapping.join_wrapped_lines = false;
    config.comments.preserve_line_breaks = true;

    let warnings = MigrationWarning::rounding(&config);
    let subjects: Vec<&str> = warnings.iter().map(|w| w.subject.as_str()).collect();

    assert_eq!(
        subjects,
        [
            "braces.keep-block-on-one-line",
            "wrapping.paren-control",
            "wrapping.join-wrapped-lines",
            "comments.preserve-line-breaks",
        ]
    );
    assert!(
        warnings
            .iter()
            .all(|w| w.kind == MigrationWarningKind::Rounded)
    );
    // The detail is rendered inside a `#` comment line.
    assert!(warnings.iter().all(|w| !w.detail.contains('\n')));
}
