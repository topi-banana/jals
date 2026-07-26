//! Eclipse JDT — the 14 `keep_*_on_one_line` settings.
//!
//! Five-valued ([`OneLine`]) — the vocabulary jals adopted wholesale as `KeepOnOneLine`,
//! because it is a strict superset of IntelliJ's `KEEP_SIMPLE_*_IN_ONE_LINE` booleans.

use serde::Deserialize;

use super::super::serde_kv::Kv;
use super::values::OneLine;

/// The one-line-body settings of a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct OneLineBodies {
    /// `keep_annotation_declaration_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_annotation_declaration_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_annotation_declaration_on_one_line: Option<OneLine>,
    /// `keep_anonymous_type_declaration_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_anonymous_type_declaration_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_anonymous_type_declaration_on_one_line: Option<OneLine>,
    /// `keep_code_block_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_code_block_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_code_block_on_one_line: Option<OneLine>,
    /// `keep_empty_array_initializer_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_empty_array_initializer_on_one_line",
        deserialize_with = "Kv::opt_bool"
    )]
    pub keep_empty_array_initializer_on_one_line: Option<bool>,
    /// `keep_enum_constant_declaration_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_enum_constant_declaration_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_enum_constant_declaration_on_one_line: Option<OneLine>,
    /// `keep_enum_declaration_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_enum_declaration_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_enum_declaration_on_one_line: Option<OneLine>,
    /// `keep_if_then_body_block_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_if_then_body_block_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_if_then_body_block_on_one_line: Option<OneLine>,
    /// `keep_imple_if_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_imple_if_on_one_line",
        deserialize_with = "Kv::opt_bool"
    )]
    pub keep_imple_if_on_one_line: Option<bool>,
    /// `keep_lambda_body_block_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_lambda_body_block_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_lambda_body_block_on_one_line: Option<OneLine>,
    /// `keep_loop_body_block_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_loop_body_block_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_loop_body_block_on_one_line: Option<OneLine>,
    /// `keep_method_body_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_method_body_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_method_body_on_one_line: Option<OneLine>,
    /// `keep_record_constructor_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_record_constructor_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_record_constructor_on_one_line: Option<OneLine>,
    /// `keep_record_declaration_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_record_declaration_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_record_declaration_on_one_line: Option<OneLine>,
    /// `keep_simple_getter_setter_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_simple_getter_setter_on_one_line",
        deserialize_with = "Kv::opt_bool"
    )]
    pub keep_simple_getter_setter_on_one_line: Option<bool>,
    /// `keep_switch_body_block_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_switch_body_block_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_switch_body_block_on_one_line: Option<OneLine>,
    /// `keep_switch_case_with_arrow_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_switch_case_with_arrow_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_switch_case_with_arrow_on_one_line: Option<OneLine>,
    /// `keep_type_declaration_on_one_line`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.keep_type_declaration_on_one_line",
        deserialize_with = "Kv::opt_enum"
    )]
    pub keep_type_declaration_on_one_line: Option<OneLine>,
}
