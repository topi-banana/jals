//! Eclipse JDT — `wrap_before_*` break placement and `parentheses_positions_in_*`.
//!
//! `wrap_before_*` decides whether a break falls before or after the operator; the
//! `parentheses_positions_in_*` family decides where a wrapped list's delimiters go. Note the
//! two id typos Eclipse ships and that this model reproduces verbatim:
//! `parentheses_positions_in_for_statment` and `..._in_method_delcaration`.

use serde::Deserialize;

use super::super::serde_kv::Kv;
use super::values::ParenthesisPositions;

/// The break-placement and delimiter-position settings of a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Wrapping {
    /// `parentheses_positions_in_annotation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_annotation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_annotation: Option<ParenthesisPositions>,
    /// `parentheses_positions_in_catch_clause`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_catch_clause",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_catch_clause: Option<ParenthesisPositions>,
    /// `parentheses_positions_in_enum_constant_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_enum_constant_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_enum_constant_declaration: Option<ParenthesisPositions>,
    /// `parentheses_positions_in_for_statment`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_for_statment",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_for_statment: Option<ParenthesisPositions>,
    /// `parentheses_positions_in_if_while_statement`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_if_while_statement",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_if_while_statement: Option<ParenthesisPositions>,
    /// `parentheses_positions_in_lambda_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_lambda_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_lambda_declaration: Option<ParenthesisPositions>,
    /// `parentheses_positions_in_method_delcaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_method_delcaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_method_delcaration: Option<ParenthesisPositions>,
    /// `parentheses_positions_in_method_invocation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_method_invocation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_method_invocation: Option<ParenthesisPositions>,
    /// `parentheses_positions_in_record_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_record_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_record_declaration: Option<ParenthesisPositions>,
    /// `parentheses_positions_in_switch_statement`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_switch_statement",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_switch_statement: Option<ParenthesisPositions>,
    /// `parentheses_positions_in_try_clause`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.parentheses_positions_in_try_clause",
        deserialize_with = "Kv::opt_enum"
    )]
    pub parentheses_positions_in_try_clause: Option<ParenthesisPositions>,
    /// `wrap_before_additive_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_additive_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_additive_operator: Option<bool>,
    /// `wrap_before_assertion_message_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_assertion_message_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_assertion_message_operator: Option<bool>,
    /// `wrap_before_assignment_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_assignment_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_assignment_operator: Option<bool>,
    /// `wrap_before_binary_operator`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_binary_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_binary_operator: Option<bool>,
    /// `wrap_before_bitwise_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_bitwise_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_bitwise_operator: Option<bool>,
    /// `wrap_before_conditional_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_conditional_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_conditional_operator: Option<bool>,
    /// `wrap_before_logical_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_logical_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_logical_operator: Option<bool>,
    /// `wrap_before_multiplicative_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_multiplicative_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_multiplicative_operator: Option<bool>,
    /// `wrap_before_or_operator_multicatch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_or_operator_multicatch",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_or_operator_multicatch: Option<bool>,
    /// `wrap_before_relational_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_relational_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_relational_operator: Option<bool>,
    /// `wrap_before_shift_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_shift_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_shift_operator: Option<bool>,
    /// `wrap_before_string_concatenation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_string_concatenation",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_string_concatenation: Option<bool>,
    /// `wrap_before_switch_case_arrow_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_switch_case_arrow_operator",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_before_switch_case_arrow_operator: Option<bool>,
}
