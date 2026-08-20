//! Eclipse JDT — the `blank_lines_*` and `number_of_*` counts.
//!
//! Both the *enforced* counts (`blank_lines_before_method`) and the *preserved* ones
//! (`number_of_empty_lines_to_preserve`), which is the split jals's `[blank-lines]` mirrors.

use crate::import::serde_kv;
use serde::Deserialize;

/// The blank-line counts of a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct BlankLines {
    /// `blank_lines_after_imports`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_after_imports",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_after_imports: Option<usize>,
    /// `blank_lines_after_last_class_body_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_after_last_class_body_declaration",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_after_last_class_body_declaration: Option<usize>,
    /// `blank_lines_after_package`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_after_package",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_after_package: Option<usize>,
    /// `blank_lines_before_abstract_method`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_before_abstract_method",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_abstract_method: Option<usize>,
    /// `blank_lines_before_field`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_before_field",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_field: Option<usize>,
    /// `blank_lines_before_first_class_body_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_before_first_class_body_declaration",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_first_class_body_declaration: Option<usize>,
    /// `blank_lines_before_imports`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_before_imports",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_imports: Option<usize>,
    /// `blank_lines_before_member_type`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_before_member_type",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_member_type: Option<usize>,
    /// `blank_lines_before_method`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_before_method",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_method: Option<usize>,
    /// `blank_lines_before_new_chunk`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_before_new_chunk",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_new_chunk: Option<usize>,
    /// `blank_lines_before_package`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_before_package",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_package: Option<usize>,
    /// `blank_lines_between_import_groups`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_between_import_groups",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_between_import_groups: Option<usize>,
    /// `blank_lines_between_statement_group_in_switch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_between_statement_group_in_switch",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_between_statement_group_in_switch: Option<usize>,
    /// `blank_lines_between_type_declarations`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.blank_lines_between_type_declarations",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_between_type_declarations: Option<usize>,
    /// `number_of_blank_lines_after_code_block`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.number_of_blank_lines_after_code_block",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub number_of_blank_lines_after_code_block: Option<usize>,
    /// `number_of_blank_lines_at_beginning_of_code_block`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.number_of_blank_lines_at_beginning_of_code_block",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub number_of_blank_lines_at_beginning_of_code_block: Option<usize>,
    /// `number_of_blank_lines_at_beginning_of_method_body`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.number_of_blank_lines_at_beginning_of_method_body",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub number_of_blank_lines_at_beginning_of_method_body: Option<usize>,
    /// `number_of_blank_lines_at_end_of_code_block`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.number_of_blank_lines_at_end_of_code_block",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub number_of_blank_lines_at_end_of_code_block: Option<usize>,
    /// `number_of_blank_lines_at_end_of_method_body`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.number_of_blank_lines_at_end_of_method_body",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub number_of_blank_lines_at_end_of_method_body: Option<usize>,
    /// `number_of_blank_lines_before_code_block`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.number_of_blank_lines_before_code_block",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub number_of_blank_lines_before_code_block: Option<usize>,
    /// `number_of_empty_lines_to_preserve`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.number_of_empty_lines_to_preserve",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub number_of_empty_lines_to_preserve: Option<usize>,
}
