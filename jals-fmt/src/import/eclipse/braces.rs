//! Eclipse JDT — the 15 `brace_position_for_*` settings.
//!
//! Four-valued ([`BracePosition`]), one per construct. jals's `[braces]` section keeps six of
//! these; the rest stay modeled here, unprojected (`MAPPING.md` §7).

use crate::import::serde_kv;
use serde::Deserialize;

use super::values::BracePosition;

/// The `brace_position_for_*` settings of a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Braces {
    /// `brace_position_for_annotation_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_annotation_type_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_annotation_type_declaration: Option<BracePosition>,
    /// `brace_position_for_anonymous_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_anonymous_type_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_anonymous_type_declaration: Option<BracePosition>,
    /// `brace_position_for_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_array_initializer",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_array_initializer: Option<BracePosition>,
    /// `brace_position_for_block`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_block",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_block: Option<BracePosition>,
    /// `brace_position_for_block_in_case`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_block_in_case",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_block_in_case: Option<BracePosition>,
    /// `brace_position_for_block_in_case_after_arrow`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_block_in_case_after_arrow",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_block_in_case_after_arrow: Option<BracePosition>,
    /// `brace_position_for_constructor_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_constructor_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_constructor_declaration: Option<BracePosition>,
    /// `brace_position_for_enum_constant`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_enum_constant",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_enum_constant: Option<BracePosition>,
    /// `brace_position_for_enum_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_enum_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_enum_declaration: Option<BracePosition>,
    /// `brace_position_for_lambda_body`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_lambda_body",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_lambda_body: Option<BracePosition>,
    /// `brace_position_for_method_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_method_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_method_declaration: Option<BracePosition>,
    /// `brace_position_for_record_constructor`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_record_constructor",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_record_constructor: Option<BracePosition>,
    /// `brace_position_for_record_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_record_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_record_declaration: Option<BracePosition>,
    /// `brace_position_for_switch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_switch",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_switch: Option<BracePosition>,
    /// `brace_position_for_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_type_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub brace_position_for_type_declaration: Option<BracePosition>,
}
