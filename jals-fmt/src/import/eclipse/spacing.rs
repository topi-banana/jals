//! Eclipse JDT — the 219 `insert_space_*` settings, the largest family by far.
//!
//! Every one is the two-valued [`Insert`] (`insert` / `do not insert` — note the interior
//! spaces), never a bool. jals's `[spacing]` section bundles these by token role into 49 keys;
//! the full context split survives here.

use serde::Deserialize;

use super::super::serde_kv::Kv;
use super::values::Insert;

/// The `insert_space_*` settings of a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Spacing {
    /// `insert_space_after_additive_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_additive_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_additive_operator: Option<Insert>,
    /// `insert_space_after_and_in_type_parameter`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_and_in_type_parameter",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_and_in_type_parameter: Option<Insert>,
    /// `insert_space_after_arrow_in_switch_case`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_arrow_in_switch_case",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_arrow_in_switch_case: Option<Insert>,
    /// `insert_space_after_arrow_in_switch_default`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_arrow_in_switch_default",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_arrow_in_switch_default: Option<Insert>,
    /// `insert_space_after_assignment_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_assignment_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_assignment_operator: Option<Insert>,
    /// `insert_space_after_at_in_annotation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_at_in_annotation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_at_in_annotation: Option<Insert>,
    /// `insert_space_after_at_in_annotation_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_at_in_annotation_type_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_at_in_annotation_type_declaration: Option<Insert>,
    /// `insert_space_after_binary_operator`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_binary_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_binary_operator: Option<Insert>,
    /// `insert_space_after_bitwise_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_bitwise_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_bitwise_operator: Option<Insert>,
    /// `insert_space_after_closing_angle_bracket_in_type_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_closing_angle_bracket_in_type_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_closing_angle_bracket_in_type_arguments: Option<Insert>,
    /// `insert_space_after_closing_angle_bracket_in_type_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_closing_angle_bracket_in_type_parameters",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_closing_angle_bracket_in_type_parameters: Option<Insert>,
    /// `insert_space_after_closing_brace_in_block`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_closing_brace_in_block",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_closing_brace_in_block: Option<Insert>,
    /// `insert_space_after_closing_paren_in_cast`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_closing_paren_in_cast",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_closing_paren_in_cast: Option<Insert>,
    /// `insert_space_after_colon_in_assert`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_colon_in_assert",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_colon_in_assert: Option<Insert>,
    /// `insert_space_after_colon_in_case`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_colon_in_case",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_colon_in_case: Option<Insert>,
    /// `insert_space_after_colon_in_conditional`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_colon_in_conditional",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_colon_in_conditional: Option<Insert>,
    /// `insert_space_after_colon_in_for`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_colon_in_for",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_colon_in_for: Option<Insert>,
    /// `insert_space_after_colon_in_labeled_statement`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_colon_in_labeled_statement",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_colon_in_labeled_statement: Option<Insert>,
    /// `insert_space_after_comma_in_allocation_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_allocation_expression",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_allocation_expression: Option<Insert>,
    /// `insert_space_after_comma_in_annotation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_annotation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_annotation: Option<Insert>,
    /// `insert_space_after_comma_in_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_array_initializer",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_array_initializer: Option<Insert>,
    /// `insert_space_after_comma_in_constructor_declaration_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_constructor_declaration_parameters",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_constructor_declaration_parameters: Option<Insert>,
    /// `insert_space_after_comma_in_constructor_declaration_throws`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_constructor_declaration_throws",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_constructor_declaration_throws: Option<Insert>,
    /// `insert_space_after_comma_in_enum_constant_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_enum_constant_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_enum_constant_arguments: Option<Insert>,
    /// `insert_space_after_comma_in_enum_declarations`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_enum_declarations",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_enum_declarations: Option<Insert>,
    /// `insert_space_after_comma_in_explicitconstructorcall_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_explicitconstructorcall_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_explicitconstructorcall_arguments: Option<Insert>,
    /// `insert_space_after_comma_in_for_increments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_for_increments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_for_increments: Option<Insert>,
    /// `insert_space_after_comma_in_for_inits`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_for_inits",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_for_inits: Option<Insert>,
    /// `insert_space_after_comma_in_method_declaration_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_method_declaration_parameters",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_method_declaration_parameters: Option<Insert>,
    /// `insert_space_after_comma_in_method_declaration_throws`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_method_declaration_throws",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_method_declaration_throws: Option<Insert>,
    /// `insert_space_after_comma_in_method_invocation_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_method_invocation_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_method_invocation_arguments: Option<Insert>,
    /// `insert_space_after_comma_in_multiple_field_declarations`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_multiple_field_declarations",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_multiple_field_declarations: Option<Insert>,
    /// `insert_space_after_comma_in_multiple_local_declarations`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_multiple_local_declarations",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_multiple_local_declarations: Option<Insert>,
    /// `insert_space_after_comma_in_parameterized_type_reference`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_parameterized_type_reference",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_parameterized_type_reference: Option<Insert>,
    /// `insert_space_after_comma_in_permitted_types`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_permitted_types",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_permitted_types: Option<Insert>,
    /// `insert_space_after_comma_in_record_components`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_record_components",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_record_components: Option<Insert>,
    /// `insert_space_after_comma_in_superinterfaces`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_superinterfaces",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_superinterfaces: Option<Insert>,
    /// `insert_space_after_comma_in_switch_case_expressions`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_switch_case_expressions",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_switch_case_expressions: Option<Insert>,
    /// `insert_space_after_comma_in_type_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_type_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_type_arguments: Option<Insert>,
    /// `insert_space_after_comma_in_type_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_comma_in_type_parameters",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_comma_in_type_parameters: Option<Insert>,
    /// `insert_space_after_ellipsis`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_ellipsis",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_ellipsis: Option<Insert>,
    /// `insert_space_after_lambda_arrow`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_lambda_arrow",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_lambda_arrow: Option<Insert>,
    /// `insert_space_after_logical_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_logical_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_logical_operator: Option<Insert>,
    /// `insert_space_after_multiplicative_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_multiplicative_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_multiplicative_operator: Option<Insert>,
    /// `insert_space_after_not_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_not_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_not_operator: Option<Insert>,
    /// `insert_space_after_opening_angle_bracket_in_parameterized_type_reference`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_angle_bracket_in_parameterized_type_reference",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_angle_bracket_in_parameterized_type_reference: Option<Insert>,
    /// `insert_space_after_opening_angle_bracket_in_type_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_angle_bracket_in_type_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_angle_bracket_in_type_arguments: Option<Insert>,
    /// `insert_space_after_opening_angle_bracket_in_type_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_angle_bracket_in_type_parameters",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_angle_bracket_in_type_parameters: Option<Insert>,
    /// `insert_space_after_opening_brace_in_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_brace_in_array_initializer",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_brace_in_array_initializer: Option<Insert>,
    /// `insert_space_after_opening_bracket_in_array_allocation_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_bracket_in_array_allocation_expression",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_bracket_in_array_allocation_expression: Option<Insert>,
    /// `insert_space_after_opening_bracket_in_array_reference`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_bracket_in_array_reference",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_bracket_in_array_reference: Option<Insert>,
    /// `insert_space_after_opening_paren_in_annotation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_annotation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_annotation: Option<Insert>,
    /// `insert_space_after_opening_paren_in_cast`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_cast",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_cast: Option<Insert>,
    /// `insert_space_after_opening_paren_in_catch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_catch",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_catch: Option<Insert>,
    /// `insert_space_after_opening_paren_in_constructor_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_constructor_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_constructor_declaration: Option<Insert>,
    /// `insert_space_after_opening_paren_in_enum_constant`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_enum_constant",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_enum_constant: Option<Insert>,
    /// `insert_space_after_opening_paren_in_for`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_for",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_for: Option<Insert>,
    /// `insert_space_after_opening_paren_in_if`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_if",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_if: Option<Insert>,
    /// `insert_space_after_opening_paren_in_method_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_method_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_method_declaration: Option<Insert>,
    /// `insert_space_after_opening_paren_in_method_invocation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_method_invocation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_method_invocation: Option<Insert>,
    /// `insert_space_after_opening_paren_in_parenthesized_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_parenthesized_expression",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_parenthesized_expression: Option<Insert>,
    /// `insert_space_after_opening_paren_in_record_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_record_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_record_declaration: Option<Insert>,
    /// `insert_space_after_opening_paren_in_switch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_switch",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_switch: Option<Insert>,
    /// `insert_space_after_opening_paren_in_synchronized`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_synchronized",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_synchronized: Option<Insert>,
    /// `insert_space_after_opening_paren_in_try`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_try",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_try: Option<Insert>,
    /// `insert_space_after_opening_paren_in_while`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_opening_paren_in_while",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_opening_paren_in_while: Option<Insert>,
    /// `insert_space_after_postfix_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_postfix_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_postfix_operator: Option<Insert>,
    /// `insert_space_after_prefix_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_prefix_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_prefix_operator: Option<Insert>,
    /// `insert_space_after_question_in_conditional`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_question_in_conditional",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_question_in_conditional: Option<Insert>,
    /// `insert_space_after_question_in_wildcard`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_question_in_wildcard",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_question_in_wildcard: Option<Insert>,
    /// `insert_space_after_relational_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_relational_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_relational_operator: Option<Insert>,
    /// `insert_space_after_semicolon_in_for`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_semicolon_in_for",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_semicolon_in_for: Option<Insert>,
    /// `insert_space_after_semicolon_in_try_resources`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_semicolon_in_try_resources",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_semicolon_in_try_resources: Option<Insert>,
    /// `insert_space_after_shift_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_shift_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_shift_operator: Option<Insert>,
    /// `insert_space_after_string_concatenation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_string_concatenation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_string_concatenation: Option<Insert>,
    /// `insert_space_after_unary_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_unary_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_after_unary_operator: Option<Insert>,
    /// `insert_space_before_additive_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_additive_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_additive_operator: Option<Insert>,
    /// `insert_space_before_and_in_type_parameter`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_and_in_type_parameter",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_and_in_type_parameter: Option<Insert>,
    /// `insert_space_before_arrow_in_switch_case`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_arrow_in_switch_case",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_arrow_in_switch_case: Option<Insert>,
    /// `insert_space_before_arrow_in_switch_default`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_arrow_in_switch_default",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_arrow_in_switch_default: Option<Insert>,
    /// `insert_space_before_assignment_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_assignment_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_assignment_operator: Option<Insert>,
    /// `insert_space_before_at_in_annotation_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_at_in_annotation_type_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_at_in_annotation_type_declaration: Option<Insert>,
    /// `insert_space_before_binary_operator`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_binary_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_binary_operator: Option<Insert>,
    /// `insert_space_before_bitwise_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_bitwise_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_bitwise_operator: Option<Insert>,
    /// `insert_space_before_closing_angle_bracket_in_parameterized_type_reference`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_angle_bracket_in_parameterized_type_reference",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_angle_bracket_in_parameterized_type_reference: Option<Insert>,
    /// `insert_space_before_closing_angle_bracket_in_type_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_angle_bracket_in_type_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_angle_bracket_in_type_arguments: Option<Insert>,
    /// `insert_space_before_closing_angle_bracket_in_type_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_angle_bracket_in_type_parameters",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_angle_bracket_in_type_parameters: Option<Insert>,
    /// `insert_space_before_closing_brace_in_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_brace_in_array_initializer",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_brace_in_array_initializer: Option<Insert>,
    /// `insert_space_before_closing_bracket_in_array_allocation_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_bracket_in_array_allocation_expression",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_bracket_in_array_allocation_expression: Option<Insert>,
    /// `insert_space_before_closing_bracket_in_array_reference`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_bracket_in_array_reference",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_bracket_in_array_reference: Option<Insert>,
    /// `insert_space_before_closing_paren_in_annotation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_annotation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_annotation: Option<Insert>,
    /// `insert_space_before_closing_paren_in_cast`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_cast",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_cast: Option<Insert>,
    /// `insert_space_before_closing_paren_in_catch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_catch",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_catch: Option<Insert>,
    /// `insert_space_before_closing_paren_in_constructor_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_constructor_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_constructor_declaration: Option<Insert>,
    /// `insert_space_before_closing_paren_in_enum_constant`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_enum_constant",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_enum_constant: Option<Insert>,
    /// `insert_space_before_closing_paren_in_for`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_for",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_for: Option<Insert>,
    /// `insert_space_before_closing_paren_in_if`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_if",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_if: Option<Insert>,
    /// `insert_space_before_closing_paren_in_method_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_method_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_method_declaration: Option<Insert>,
    /// `insert_space_before_closing_paren_in_method_invocation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_method_invocation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_method_invocation: Option<Insert>,
    /// `insert_space_before_closing_paren_in_parenthesized_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_parenthesized_expression",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_parenthesized_expression: Option<Insert>,
    /// `insert_space_before_closing_paren_in_record_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_record_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_record_declaration: Option<Insert>,
    /// `insert_space_before_closing_paren_in_switch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_switch",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_switch: Option<Insert>,
    /// `insert_space_before_closing_paren_in_synchronized`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_synchronized",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_synchronized: Option<Insert>,
    /// `insert_space_before_closing_paren_in_try`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_try",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_try: Option<Insert>,
    /// `insert_space_before_closing_paren_in_while`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_closing_paren_in_while",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_closing_paren_in_while: Option<Insert>,
    /// `insert_space_before_colon_in_assert`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_colon_in_assert",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_colon_in_assert: Option<Insert>,
    /// `insert_space_before_colon_in_case`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_colon_in_case",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_colon_in_case: Option<Insert>,
    /// `insert_space_before_colon_in_conditional`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_colon_in_conditional",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_colon_in_conditional: Option<Insert>,
    /// `insert_space_before_colon_in_default`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_colon_in_default",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_colon_in_default: Option<Insert>,
    /// `insert_space_before_colon_in_for`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_colon_in_for",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_colon_in_for: Option<Insert>,
    /// `insert_space_before_colon_in_labeled_statement`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_colon_in_labeled_statement",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_colon_in_labeled_statement: Option<Insert>,
    /// `insert_space_before_comma_in_allocation_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_allocation_expression",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_allocation_expression: Option<Insert>,
    /// `insert_space_before_comma_in_annotation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_annotation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_annotation: Option<Insert>,
    /// `insert_space_before_comma_in_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_array_initializer",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_array_initializer: Option<Insert>,
    /// `insert_space_before_comma_in_constructor_declaration_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_constructor_declaration_parameters",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_constructor_declaration_parameters: Option<Insert>,
    /// `insert_space_before_comma_in_constructor_declaration_throws`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_constructor_declaration_throws",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_constructor_declaration_throws: Option<Insert>,
    /// `insert_space_before_comma_in_enum_constant_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_enum_constant_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_enum_constant_arguments: Option<Insert>,
    /// `insert_space_before_comma_in_enum_declarations`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_enum_declarations",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_enum_declarations: Option<Insert>,
    /// `insert_space_before_comma_in_explicitconstructorcall_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_explicitconstructorcall_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_explicitconstructorcall_arguments: Option<Insert>,
    /// `insert_space_before_comma_in_for_increments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_for_increments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_for_increments: Option<Insert>,
    /// `insert_space_before_comma_in_for_inits`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_for_inits",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_for_inits: Option<Insert>,
    /// `insert_space_before_comma_in_method_declaration_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_method_declaration_parameters",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_method_declaration_parameters: Option<Insert>,
    /// `insert_space_before_comma_in_method_declaration_throws`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_method_declaration_throws",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_method_declaration_throws: Option<Insert>,
    /// `insert_space_before_comma_in_method_invocation_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_method_invocation_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_method_invocation_arguments: Option<Insert>,
    /// `insert_space_before_comma_in_multiple_field_declarations`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_multiple_field_declarations",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_multiple_field_declarations: Option<Insert>,
    /// `insert_space_before_comma_in_multiple_local_declarations`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_multiple_local_declarations",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_multiple_local_declarations: Option<Insert>,
    /// `insert_space_before_comma_in_parameterized_type_reference`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_parameterized_type_reference",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_parameterized_type_reference: Option<Insert>,
    /// `insert_space_before_comma_in_permitted_types`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_permitted_types",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_permitted_types: Option<Insert>,
    /// `insert_space_before_comma_in_record_components`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_record_components",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_record_components: Option<Insert>,
    /// `insert_space_before_comma_in_superinterfaces`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_superinterfaces",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_superinterfaces: Option<Insert>,
    /// `insert_space_before_comma_in_switch_case_expressions`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_switch_case_expressions",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_switch_case_expressions: Option<Insert>,
    /// `insert_space_before_comma_in_type_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_type_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_type_arguments: Option<Insert>,
    /// `insert_space_before_comma_in_type_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_comma_in_type_parameters",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_comma_in_type_parameters: Option<Insert>,
    /// `insert_space_before_ellipsis`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_ellipsis",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_ellipsis: Option<Insert>,
    /// `insert_space_before_lambda_arrow`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_lambda_arrow",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_lambda_arrow: Option<Insert>,
    /// `insert_space_before_logical_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_logical_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_logical_operator: Option<Insert>,
    /// `insert_space_before_multiplicative_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_multiplicative_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_multiplicative_operator: Option<Insert>,
    /// `insert_space_before_opening_angle_bracket_in_parameterized_type_reference`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_angle_bracket_in_parameterized_type_reference",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_angle_bracket_in_parameterized_type_reference: Option<Insert>,
    /// `insert_space_before_opening_angle_bracket_in_type_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_angle_bracket_in_type_arguments",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_angle_bracket_in_type_arguments: Option<Insert>,
    /// `insert_space_before_opening_angle_bracket_in_type_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_angle_bracket_in_type_parameters",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_angle_bracket_in_type_parameters: Option<Insert>,
    /// `insert_space_before_opening_brace_in_annotation_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_annotation_type_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_annotation_type_declaration: Option<Insert>,
    /// `insert_space_before_opening_brace_in_anonymous_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_anonymous_type_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_anonymous_type_declaration: Option<Insert>,
    /// `insert_space_before_opening_brace_in_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_array_initializer",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_array_initializer: Option<Insert>,
    /// `insert_space_before_opening_brace_in_block`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_block",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_block: Option<Insert>,
    /// `insert_space_before_opening_brace_in_constructor_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_constructor_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_constructor_declaration: Option<Insert>,
    /// `insert_space_before_opening_brace_in_enum_constant`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_enum_constant",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_enum_constant: Option<Insert>,
    /// `insert_space_before_opening_brace_in_enum_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_enum_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_enum_declaration: Option<Insert>,
    /// `insert_space_before_opening_brace_in_method_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_method_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_method_declaration: Option<Insert>,
    /// `insert_space_before_opening_brace_in_record_constructor`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_record_constructor",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_record_constructor: Option<Insert>,
    /// `insert_space_before_opening_brace_in_record_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_record_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_record_declaration: Option<Insert>,
    /// `insert_space_before_opening_brace_in_switch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_switch",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_switch: Option<Insert>,
    /// `insert_space_before_opening_brace_in_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_brace_in_type_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_brace_in_type_declaration: Option<Insert>,
    /// `insert_space_before_opening_bracket_in_array_allocation_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_bracket_in_array_allocation_expression",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_bracket_in_array_allocation_expression: Option<Insert>,
    /// `insert_space_before_opening_bracket_in_array_reference`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_bracket_in_array_reference",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_bracket_in_array_reference: Option<Insert>,
    /// `insert_space_before_opening_bracket_in_array_type_reference`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_bracket_in_array_type_reference",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_bracket_in_array_type_reference: Option<Insert>,
    /// `insert_space_before_opening_paren_in_annotation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_annotation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_annotation: Option<Insert>,
    /// `insert_space_before_opening_paren_in_annotation_type_member_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_annotation_type_member_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_annotation_type_member_declaration: Option<Insert>,
    /// `insert_space_before_opening_paren_in_catch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_catch",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_catch: Option<Insert>,
    /// `insert_space_before_opening_paren_in_constructor_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_constructor_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_constructor_declaration: Option<Insert>,
    /// `insert_space_before_opening_paren_in_enum_constant`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_enum_constant",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_enum_constant: Option<Insert>,
    /// `insert_space_before_opening_paren_in_for`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_for",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_for: Option<Insert>,
    /// `insert_space_before_opening_paren_in_if`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_if",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_if: Option<Insert>,
    /// `insert_space_before_opening_paren_in_method_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_method_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_method_declaration: Option<Insert>,
    /// `insert_space_before_opening_paren_in_method_invocation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_method_invocation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_method_invocation: Option<Insert>,
    /// `insert_space_before_opening_paren_in_parenthesized_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_parenthesized_expression",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_parenthesized_expression: Option<Insert>,
    /// `insert_space_before_opening_paren_in_record_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_record_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_record_declaration: Option<Insert>,
    /// `insert_space_before_opening_paren_in_switch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_switch",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_switch: Option<Insert>,
    /// `insert_space_before_opening_paren_in_synchronized`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_synchronized",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_synchronized: Option<Insert>,
    /// `insert_space_before_opening_paren_in_try`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_try",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_try: Option<Insert>,
    /// `insert_space_before_opening_paren_in_while`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_opening_paren_in_while",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_opening_paren_in_while: Option<Insert>,
    /// `insert_space_before_parenthesized_expression_in_return`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_parenthesized_expression_in_return",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_parenthesized_expression_in_return: Option<Insert>,
    /// `insert_space_before_parenthesized_expression_in_throw`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_parenthesized_expression_in_throw",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_parenthesized_expression_in_throw: Option<Insert>,
    /// `insert_space_before_postfix_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_postfix_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_postfix_operator: Option<Insert>,
    /// `insert_space_before_prefix_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_prefix_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_prefix_operator: Option<Insert>,
    /// `insert_space_before_question_in_conditional`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_question_in_conditional",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_question_in_conditional: Option<Insert>,
    /// `insert_space_before_question_in_wildcard`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_question_in_wildcard",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_question_in_wildcard: Option<Insert>,
    /// `insert_space_before_relational_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_relational_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_relational_operator: Option<Insert>,
    /// `insert_space_before_semicolon`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_semicolon",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_semicolon: Option<Insert>,
    /// `insert_space_before_semicolon_in_for`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_semicolon_in_for",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_semicolon_in_for: Option<Insert>,
    /// `insert_space_before_semicolon_in_try_resources`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_semicolon_in_try_resources",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_semicolon_in_try_resources: Option<Insert>,
    /// `insert_space_before_shift_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_shift_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_shift_operator: Option<Insert>,
    /// `insert_space_before_string_concatenation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_string_concatenation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_string_concatenation: Option<Insert>,
    /// `insert_space_before_unary_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_unary_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_before_unary_operator: Option<Insert>,
    /// `insert_space_between_brackets_in_array_type_reference`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_between_brackets_in_array_type_reference",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_between_brackets_in_array_type_reference: Option<Insert>,
    /// `insert_space_between_empty_braces_in_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_between_empty_braces_in_array_initializer",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_between_empty_braces_in_array_initializer: Option<Insert>,
    /// `insert_space_between_empty_brackets_in_array_allocation_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_between_empty_brackets_in_array_allocation_expression",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_between_empty_brackets_in_array_allocation_expression: Option<Insert>,
    /// `insert_space_between_empty_parens_in_annotation_type_member_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_between_empty_parens_in_annotation_type_member_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_between_empty_parens_in_annotation_type_member_declaration: Option<Insert>,
    /// `insert_space_between_empty_parens_in_constructor_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_between_empty_parens_in_constructor_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_between_empty_parens_in_constructor_declaration: Option<Insert>,
    /// `insert_space_between_empty_parens_in_enum_constant`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_between_empty_parens_in_enum_constant",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_between_empty_parens_in_enum_constant: Option<Insert>,
    /// `insert_space_between_empty_parens_in_method_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_between_empty_parens_in_method_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_between_empty_parens_in_method_declaration: Option<Insert>,
    /// `insert_space_between_empty_parens_in_method_invocation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_between_empty_parens_in_method_invocation",
        deserialize_with = "Kv::opt_enum"
    )]
    pub insert_space_between_empty_parens_in_method_invocation: Option<Insert>,
}
