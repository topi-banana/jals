//! Eclipse JDT — the 25 `insert_new_line_*` settings.
//!
//! Line breaks at fixed structural positions (after an annotation, before `else`, at end of
//! file, …), each the two-valued [`Insert`].

use crate::import::serde_kv;
use serde::Deserialize;

use super::values::Insert;

/// The `insert_new_line_*` settings of a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NewLines {
    /// `insert_new_line_after_annotation`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_annotation",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_annotation: Option<Insert>,
    /// `insert_new_line_after_annotation_on_enum_constant`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_annotation_on_enum_constant",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_annotation_on_enum_constant: Option<Insert>,
    /// `insert_new_line_after_annotation_on_field`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_annotation_on_field",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_annotation_on_field: Option<Insert>,
    /// `insert_new_line_after_annotation_on_local_variable`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_annotation_on_local_variable",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_annotation_on_local_variable: Option<Insert>,
    /// `insert_new_line_after_annotation_on_member`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_annotation_on_member",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_annotation_on_member: Option<Insert>,
    /// `insert_new_line_after_annotation_on_method`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_annotation_on_method",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_annotation_on_method: Option<Insert>,
    /// `insert_new_line_after_annotation_on_package`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_annotation_on_package",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_annotation_on_package: Option<Insert>,
    /// `insert_new_line_after_annotation_on_parameter`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_annotation_on_parameter",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_annotation_on_parameter: Option<Insert>,
    /// `insert_new_line_after_annotation_on_type`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_annotation_on_type",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_annotation_on_type: Option<Insert>,
    /// `insert_new_line_after_label`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_label",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_label: Option<Insert>,
    /// `insert_new_line_after_opening_brace_in_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_opening_brace_in_array_initializer",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_opening_brace_in_array_initializer: Option<Insert>,
    /// `insert_new_line_after_type_annotation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_type_annotation",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_after_type_annotation: Option<Insert>,
    /// `insert_new_line_at_end_of_file_if_missing`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_at_end_of_file_if_missing",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_at_end_of_file_if_missing: Option<Insert>,
    /// `insert_new_line_before_catch_in_try_statement`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_before_catch_in_try_statement",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_before_catch_in_try_statement: Option<Insert>,
    /// `insert_new_line_before_closing_brace_in_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_before_closing_brace_in_array_initializer",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_before_closing_brace_in_array_initializer: Option<Insert>,
    /// `insert_new_line_before_else_in_if_statement`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_before_else_in_if_statement",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_before_else_in_if_statement: Option<Insert>,
    /// `insert_new_line_before_finally_in_try_statement`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_before_finally_in_try_statement",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_before_finally_in_try_statement: Option<Insert>,
    /// `insert_new_line_before_while_in_do_statement`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_before_while_in_do_statement",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_before_while_in_do_statement: Option<Insert>,
    /// `insert_new_line_in_empty_annotation_declaration`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_in_empty_annotation_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_in_empty_annotation_declaration: Option<Insert>,
    /// `insert_new_line_in_empty_anonymous_type_declaration`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_in_empty_anonymous_type_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_in_empty_anonymous_type_declaration: Option<Insert>,
    /// `insert_new_line_in_empty_block`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_in_empty_block",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_in_empty_block: Option<Insert>,
    /// `insert_new_line_in_empty_enum_constant`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_in_empty_enum_constant",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_in_empty_enum_constant: Option<Insert>,
    /// `insert_new_line_in_empty_enum_declaration`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_in_empty_enum_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_in_empty_enum_declaration: Option<Insert>,
    /// `insert_new_line_in_empty_method_body`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_in_empty_method_body",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_in_empty_method_body: Option<Insert>,
    /// `insert_new_line_in_empty_type_declaration`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_in_empty_type_declaration",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub insert_new_line_in_empty_type_declaration: Option<Insert>,
}
