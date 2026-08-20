//! Eclipse JDT — indentation, width, and the settings that belong to no larger family.
//!
//! Everything under `org.eclipse.jdt.core.formatter.` that is not one of the eight regular
//! families: the tab / indent sizes, the `lineSplit` column limit, the on/off tags, the
//! `indent_*` toggles, `join_*`, `align_*_on_columns`, and the text-block settings.

use crate::import::serde_kv;
use alloc::string::String;

use serde::Deserialize;

use super::values::{TabChar, TextBlockIndentation};

/// The indentation, width, and miscellaneous settings of a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Indentation {
    /// `align_arrows_in_switch_on_columns`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.align_arrows_in_switch_on_columns",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_arrows_in_switch_on_columns: Option<bool>,
    /// `align_assignment_statements_on_columns`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.align_assignment_statements_on_columns",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_assignment_statements_on_columns: Option<bool>,
    /// `align_fields_grouping_blank_lines`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.align_fields_grouping_blank_lines",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub align_fields_grouping_blank_lines: Option<usize>,
    /// `align_selector_in_method_invocation_on_expression_first_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.align_selector_in_method_invocation_on_expression_first_line",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_selector_in_method_invocation_on_expression_first_line: Option<bool>,
    /// `align_type_members_on_columns`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.align_type_members_on_columns",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_type_members_on_columns: Option<bool>,
    /// `align_variable_declarations_on_columns`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.align_variable_declarations_on_columns",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_variable_declarations_on_columns: Option<bool>,
    /// `align_with_spaces`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.align_with_spaces",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_with_spaces: Option<bool>,
    /// `compact_else_if`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.compact_else_if",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub compact_else_if: Option<bool>,
    /// `continuation_indentation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.continuation_indentation",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub continuation_indentation: Option<usize>,
    /// `continuation_indentation_for_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.continuation_indentation_for_array_initializer",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub continuation_indentation_for_array_initializer: Option<usize>,
    /// `disabling_tag`.
    #[serde(rename = "org.eclipse.jdt.core.formatter.disabling_tag")]
    pub disabling_tag: Option<String>,
    /// `enabling_tag`.
    #[serde(rename = "org.eclipse.jdt.core.formatter.enabling_tag")]
    pub enabling_tag: Option<String>,
    /// `format_guardian_clause_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.format_guardian_clause_on_one_line",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub format_guardian_clause_on_one_line: Option<bool>,
    /// `format_line_comment_starting_on_first_column`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.format_line_comment_starting_on_first_column",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub format_line_comment_starting_on_first_column: Option<bool>,
    /// `indent_body_declarations_compare_to_annotation_declaration_header`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_body_declarations_compare_to_annotation_declaration_header",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_body_declarations_compare_to_annotation_declaration_header: Option<bool>,
    /// `indent_body_declarations_compare_to_enum_constant_header`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_body_declarations_compare_to_enum_constant_header",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_body_declarations_compare_to_enum_constant_header: Option<bool>,
    /// `indent_body_declarations_compare_to_enum_declaration_header`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_body_declarations_compare_to_enum_declaration_header",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_body_declarations_compare_to_enum_declaration_header: Option<bool>,
    /// `indent_body_declarations_compare_to_record_header`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_body_declarations_compare_to_record_header",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_body_declarations_compare_to_record_header: Option<bool>,
    /// `indent_body_declarations_compare_to_type_header`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_body_declarations_compare_to_type_header",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_body_declarations_compare_to_type_header: Option<bool>,
    /// `indent_breaks_compare_to_cases`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_breaks_compare_to_cases",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_breaks_compare_to_cases: Option<bool>,
    /// `indent_empty_lines`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_empty_lines",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_empty_lines: Option<bool>,
    /// `indent_statements_compare_to_block`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_statements_compare_to_block",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_statements_compare_to_block: Option<bool>,
    /// `indent_statements_compare_to_body`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_statements_compare_to_body",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_statements_compare_to_body: Option<bool>,
    /// `indent_switchstatements_compare_to_cases`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_switchstatements_compare_to_cases",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_switchstatements_compare_to_cases: Option<bool>,
    /// `indent_switchstatements_compare_to_switch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indent_switchstatements_compare_to_switch",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_switchstatements_compare_to_switch: Option<bool>,
    /// `indentation.size`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.indentation.size",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub indentation_size: Option<usize>,
    /// `join_line_comments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.join_line_comments",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub join_line_comments: Option<bool>,
    /// `join_lines_in_comments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.join_lines_in_comments",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub join_lines_in_comments: Option<bool>,
    /// `join_wrapped_lines`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.join_wrapped_lines",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub join_wrapped_lines: Option<bool>,
    /// `keep_else_statement_on_same_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_else_statement_on_same_line",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_else_statement_on_same_line: Option<bool>,
    /// `keep_simple_do_while_body_on_same_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_simple_do_while_body_on_same_line",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_simple_do_while_body_on_same_line: Option<bool>,
    /// `keep_simple_for_body_on_same_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_simple_for_body_on_same_line",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_simple_for_body_on_same_line: Option<bool>,
    /// `keep_simple_while_body_on_same_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_simple_while_body_on_same_line",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_simple_while_body_on_same_line: Option<bool>,
    /// `keep_then_statement_on_same_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_then_statement_on_same_line",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_then_statement_on_same_line: Option<bool>,
    /// `lineSplit`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.lineSplit",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub line_split: Option<usize>,
    /// `never_indent_block_comments_on_first_column`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.never_indent_block_comments_on_first_column",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub never_indent_block_comments_on_first_column: Option<bool>,
    /// `never_indent_line_comments_on_first_column`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.never_indent_line_comments_on_first_column",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub never_indent_line_comments_on_first_column: Option<bool>,
    /// `put_empty_statement_on_new_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.put_empty_statement_on_new_line",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub put_empty_statement_on_new_line: Option<bool>,
    /// `put_text_block_quotes_on_new_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.put_text_block_quotes_on_new_line",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub put_text_block_quotes_on_new_line: Option<bool>,
    /// `tabulation.char`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.tabulation.char",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub tabulation_char: Option<TabChar>,
    /// `tabulation.size`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.tabulation.size",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub tabulation_size: Option<usize>,
    /// `text_block_indentation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.text_block_indentation",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub text_block_indentation: Option<TextBlockIndentation>,
    /// `use_on_off_tags`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.use_on_off_tags",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub use_on_off_tags: Option<bool>,
    /// `use_tabs_only_for_leading_indentations`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.use_tabs_only_for_leading_indentations",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub use_tabs_only_for_leading_indentations: Option<bool>,
    /// `wrap_outer_expressions_when_nested`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_outer_expressions_when_nested",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub wrap_outer_expressions_when_nested: Option<bool>,
}
