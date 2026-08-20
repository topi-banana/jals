use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use jals_config::fmt::{
    BraceStyle, Config, IndentStyle, KeepOnOneLine, ParenPositions, WrapPolicy,
};

use super::super::ConfigImporter;
use super::{EclipseConfig, EclipsePrefs};

/// The machine-extracted option inventory this importer is measured against.
const INVENTORY: &str = include_str!("inventory.tsv");

/// One inventory row: the full setting id and its value kind.
struct Row {
    id: String,
    kind: String,
}

/// The inventory reader and the profile builder, grouped so they are not free functions.
pub(crate) mod api {
    use super::*;

    /// Every option row, comments and blank lines dropped.
    pub(super) fn inventory() -> Vec<Row> {
        INVENTORY
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                let mut columns = line.split('\t');
                let id = columns.next().expect("row has an id");
                let kind = columns.next().expect("row has a kind");
                Row {
                    id: format!("{}{id}", crate::import::text::ECLIPSE_FORMATTER_PREFIX),
                    kind: kind.to_owned(),
                }
            })
            .collect()
    }

    /// A parseable, non-default value for a setting of `kind`.
    pub(crate) fn probe(kind: &str) -> &'static str {
        match kind {
            "insert" => "insert",
            "bool" => "true",
            "int" => "7",
            "alignment" => "16",
            "brace" => "next_line",
            "one-line" => "one_line_always",
            "paren" => "separate_lines",
            "tab-char" => "mixed",
            "text-block-indent" => "indent_by_one",
            "string" => "@off",
            other => panic!("inventory row has an unknown value kind `{other}`"),
        }
    }

    /// Parse a profile given as `.prefs` lines.
    pub(crate) fn prefs(lines: &[(&str, &str)]) -> EclipseConfig {
        let mut src = String::new();
        for (id, value) in lines {
            let prefix = crate::import::text::ECLIPSE_FORMATTER_PREFIX;
            writeln!(src, "{prefix}{id}={value}").expect("writing to a String cannot fail");
        }
        EclipsePrefs::parse(&src).expect("profile should parse")
    }
}

#[test]
fn every_inventoried_option_is_modeled() {
    // The inventory is extracted from `DefaultCodeFormatterConstants.java`, so "every row is
    // captured" is exactly "no Eclipse setting is missing from the model".
    let baseline = EclipseConfig::default();
    let mut missing = Vec::new();

    for row in api::inventory() {
        let mut pairs = BTreeMap::new();
        pairs.insert(row.id.clone(), api::probe(&row.kind).to_owned());
        let parsed = EclipseConfig::from_pairs(pairs).expect("single setting should parse");
        if parsed == baseline {
            missing.push(row.id);
        }
    }

    assert!(
        missing.is_empty(),
        "{} Eclipse setting(s) in inventory.tsv are not captured by the model: {missing:?}",
        missing.len()
    );
}

#[test]
fn the_inventory_is_the_documented_size() {
    // A guard against silently shrinking the checklist the test above depends on.
    let rows = api::inventory();
    assert_eq!(rows.len(), 416, "Eclipse ships 401 live + 15 legacy ids");
}

#[test]
fn a_profile_key_outside_the_formatter_namespace_is_ignored() {
    // `.prefs` files mix in `org.eclipse.jdt.core.compiler.*` and the prefs store version.
    let config = EclipsePrefs::parse(
        "eclipse.preferences.version=1\n\
         org.eclipse.jdt.core.compiler.compliance=21\n\
         org.eclipse.jdt.core.formatter.lineSplit=120\n",
    )
    .expect("profile should parse");
    assert_eq!(config.indentation.line_split, Some(120));
    assert_eq!(config.indentation.tabulation_size, None);
}

#[test]
fn indentation_and_width_project() {
    let config: Config = api::prefs(&[
        ("tabulation.char", "space"),
        ("tabulation.size", "2"),
        ("continuation_indentation", "2"),
        ("lineSplit", "120"),
    ])
    .into();

    assert_eq!(config.layout.indent_style, IndentStyle::Space);
    assert_eq!(config.layout.indent_width, 2);
    // Eclipse counts the continuation indent in levels: 2 levels × 2 columns.
    assert_eq!(config.layout.continuation_indent, Some(4));
    assert_eq!(config.layout.max_width, 120);
}

#[test]
fn mixed_indentation_separates_the_level_width_from_the_tab_stop() {
    let config: Config = api::prefs(&[
        ("tabulation.char", "mixed"),
        ("tabulation.size", "8"),
        ("indentation.size", "4"),
    ])
    .into();

    assert_eq!(config.layout.indent_style, IndentStyle::Mixed);
    // Under `mixed` the level width is `indentation.size` and the tab stop is `tabulation.size`.
    assert_eq!(config.layout.indent_width, 4);
    assert_eq!(config.layout.tab_width, 8);
}

#[test]
fn brace_positions_keep_their_four_way_distinction() {
    let config: Config = api::prefs(&[
        ("brace_position_for_type_declaration", "next_line"),
        ("brace_position_for_method_declaration", "next_line_shifted"),
        ("brace_position_for_block", "next_line_on_wrap"),
        ("brace_position_for_lambda_body", "end_of_line"),
    ])
    .into();

    assert_eq!(config.braces.type_declaration, BraceStyle::NextLine);
    assert_eq!(
        config.braces.method_declaration,
        BraceStyle::NextLineShifted
    );
    assert_eq!(config.braces.block, BraceStyle::NextLineOnWrap);
    assert_eq!(config.braces.lambda_body, BraceStyle::SameLine);
}

#[test]
fn alignment_bits_decide_the_wrap_policy() {
    // `M_COMPACT_SPLIT`(16) is a fill; adding `M_FORCE`(1) makes it unconditional; the
    // one-per-line split(48) chops down; `Integer.MAX_VALUE` is the never-wrap sentinel; and a
    // value with no split bits means "no alignment here".
    let config: Config = api::prefs(&[
        ("alignment_for_arguments_in_method_invocation", "16"),
        ("alignment_for_parameters_in_method_declaration", "17"),
        ("alignment_for_expressions_in_array_initializer", "48"),
        ("alignment_for_enum_constants", "2147483647"),
        ("alignment_for_assignment", "0"),
    ])
    .into();

    assert_eq!(config.wrapping.call_arguments, WrapPolicy::IfLong);
    assert_eq!(config.wrapping.method_parameters, WrapPolicy::AlwaysPerItem);
    assert_eq!(config.wrapping.array_initializer, WrapPolicy::IfLongPerItem);
    assert_eq!(config.wrapping.enum_constants, WrapPolicy::Never);
    assert_eq!(config.wrapping.assignment, WrapPolicy::Never);
    // An alignment is a bitmask, not an opaque id: `17` keeps its split mode and adds `M_FORCE`.
    let forced = api::prefs(&[("alignment_for_parameters_in_method_declaration", "17")]);
    let alignment = forced
        .alignment
        .alignment_for_parameters_in_method_declaration
        .expect("modeled");
    assert!(alignment.is_forced());
    assert_eq!(alignment.split(), super::Alignment::COMPACT);
}

#[test]
fn the_deprecated_binary_operator_ids_still_land() {
    // JDT keeps reading `wrap_before_binary_operator` and fans it out; jals uses it as the
    // fallback for the per-operator-class setting.
    let config: Config = api::prefs(&[("wrap_before_binary_operator", "false")]).into();
    assert!(!config.wrapping.before_binary_operator);

    // The granular setting wins when both are present.
    let config: Config = api::prefs(&[
        ("wrap_before_binary_operator", "false"),
        ("wrap_before_additive_operator", "true"),
    ])
    .into();
    assert!(config.wrapping.before_binary_operator);
}

#[test]
fn the_five_colon_contexts_project_independently() {
    let config: Config = api::prefs(&[
        ("insert_space_before_colon_in_conditional", "insert"),
        ("insert_space_before_colon_in_for", "do not insert"),
        ("insert_space_before_colon_in_labeled_statement", "insert"),
        ("insert_space_before_colon_in_case", "do not insert"),
        ("insert_space_before_colon_in_assert", "insert"),
    ])
    .into();

    assert!(config.spacing.before_ternary_colon);
    assert!(!config.spacing.before_foreach_colon);
    assert!(config.spacing.before_label_colon);
    assert!(!config.spacing.before_case_colon);
    assert!(config.spacing.before_assert_colon);
}

#[test]
fn one_line_policies_map_one_to_one() {
    let config: Config = api::prefs(&[
        ("keep_method_body_on_one_line", "one_line_if_single_item"),
        ("keep_code_block_on_one_line", "one_line_preserve"),
        ("keep_type_declaration_on_one_line", "one_line_never"),
    ])
    .into();

    assert_eq!(
        config.braces.keep_method_body_on_one_line,
        KeepOnOneLine::IfSingleItem
    );
    assert_eq!(
        config.braces.keep_block_on_one_line,
        KeepOnOneLine::Preserve
    );
    assert_eq!(
        config.braces.keep_type_body_on_one_line,
        KeepOnOneLine::Never
    );
}

#[test]
fn parenthesis_positions_map_one_to_one() {
    // Note the two ids Eclipse ships misspelled; the model reproduces them verbatim.
    let config: Config = api::prefs(&[
        (
            "parentheses_positions_in_method_delcaration",
            "separate_lines",
        ),
        ("parentheses_positions_in_method_invocation", "common_lines"),
        (
            "parentheses_positions_in_if_while_statement",
            "preserve_positions",
        ),
    ])
    .into();

    assert_eq!(
        config.wrapping.paren_method_declaration,
        ParenPositions::SeparateLines
    );
    assert_eq!(
        config.wrapping.paren_method_invocation,
        ParenPositions::CommonLines
    );
    assert_eq!(config.wrapping.paren_control, ParenPositions::Preserve);
}

#[test]
fn an_unparsable_value_leaves_the_setting_unset() {
    // Real profiles carry stray and tool-specific values; those must not fail the whole import.
    let config = api::prefs(&[("lineSplit", "not-a-number"), ("tabulation.char", "wobbly")]);
    assert_eq!(config.indentation.line_split, None);
    assert_eq!(config.indentation.tabulation_char, None);

    // And an unset setting leaves the jals option at *jals's* default, not Eclipse's.
    let projected: Config = config.into();
    assert_eq!(
        projected.layout.max_width,
        Config::default().layout.max_width
    );
}

#[test]
fn comment_settings_project() {
    let config: Config = api::prefs(&[
        ("comment.format_javadoc_comments", "true"),
        ("comment.format_line_comments", "false"),
        ("comment.line_length", "72"),
        ("comment.clear_blank_lines_in_javadoc_comment", "true"),
    ])
    .into();

    assert!(config.comments.format_javadoc);
    assert!(!config.comments.format_line);
    assert_eq!(config.comments.width, 72);
    // Eclipse states the negative ("clear them"); jals states the positive.
    assert!(!config.comments.preserve_blank_lines);
}

#[cfg(feature = "std")]
#[test]
fn the_xml_profile_and_the_prefs_file_agree() {
    use super::super::eclipse::EclipseXmlProfile;

    let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<profiles version="23">
<profile kind="CodeFormatterProfile" name="Eclipse" version="23">
<setting id="org.eclipse.jdt.core.formatter.tabulation.char" value="space"/>
<setting id="org.eclipse.jdt.core.formatter.tabulation.size" value="2"/>
<setting id="org.eclipse.jdt.core.formatter.lineSplit" value="120"/>
<setting id="org.eclipse.jdt.core.formatter.brace_position_for_type_declaration" value="next_line"/>
</profile>
</profiles>"#;

    let from_xml = EclipseXmlProfile::parse(xml).expect("profile should parse");
    let from_prefs = api::prefs(&[
        ("tabulation.char", "space"),
        ("tabulation.size", "2"),
        ("lineSplit", "120"),
        ("brace_position_for_type_declaration", "next_line"),
    ]);
    assert_eq!(from_xml, from_prefs);
}
