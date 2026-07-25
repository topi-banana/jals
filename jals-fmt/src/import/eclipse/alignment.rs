//! Eclipse JDT — the 53 `alignment_for_*` wrap policies.
//!
//! Each value is a decimal integer whose **bits** carry the policy, not an opaque id — see
//! [`Alignment`] for the bit layout and `jals-fmt/MAPPING.md` §5.4 for the projection onto
//! jals's four-valued `WrapPolicy`.

use serde::Deserialize;

use super::values::Alignment;

/// The `alignment_for_*` wrap bitmasks of a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Alignments {
    /// `alignment_for_additive_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_additive_operator",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_additive_operator: Option<Alignment>,
    /// `alignment_for_annotations_on_enum_constant`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_annotations_on_enum_constant",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_annotations_on_enum_constant: Option<Alignment>,
    /// `alignment_for_annotations_on_field`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_annotations_on_field",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_annotations_on_field: Option<Alignment>,
    /// `alignment_for_annotations_on_local_variable`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_annotations_on_local_variable",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_annotations_on_local_variable: Option<Alignment>,
    /// `alignment_for_annotations_on_method`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_annotations_on_method",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_annotations_on_method: Option<Alignment>,
    /// `alignment_for_annotations_on_package`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_annotations_on_package",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_annotations_on_package: Option<Alignment>,
    /// `alignment_for_annotations_on_parameter`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_annotations_on_parameter",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_annotations_on_parameter: Option<Alignment>,
    /// `alignment_for_annotations_on_type`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_annotations_on_type",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_annotations_on_type: Option<Alignment>,
    /// `alignment_for_arguments_in_allocation_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_arguments_in_allocation_expression",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_arguments_in_allocation_expression: Option<Alignment>,
    /// `alignment_for_arguments_in_annotation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_arguments_in_annotation",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_arguments_in_annotation: Option<Alignment>,
    /// `alignment_for_arguments_in_enum_constant`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_arguments_in_enum_constant",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_arguments_in_enum_constant: Option<Alignment>,
    /// `alignment_for_arguments_in_explicit_constructor_call`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_arguments_in_explicit_constructor_call",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_arguments_in_explicit_constructor_call: Option<Alignment>,
    /// `alignment_for_arguments_in_method_invocation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_arguments_in_method_invocation",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_arguments_in_method_invocation: Option<Alignment>,
    /// `alignment_for_arguments_in_qualified_allocation_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_arguments_in_qualified_allocation_expression",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_arguments_in_qualified_allocation_expression: Option<Alignment>,
    /// `alignment_for_assertion_message`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_assertion_message",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_assertion_message: Option<Alignment>,
    /// `alignment_for_assignment`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_assignment",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_assignment: Option<Alignment>,
    /// `alignment_for_binary_expression`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_binary_expression",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_binary_expression: Option<Alignment>,
    /// `alignment_for_bitwise_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_bitwise_operator",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_bitwise_operator: Option<Alignment>,
    /// `alignment_for_compact_if`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_compact_if",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_compact_if: Option<Alignment>,
    /// `alignment_for_compact_loops`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_compact_loops",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_compact_loops: Option<Alignment>,
    /// `alignment_for_conditional_expression`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_conditional_expression",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_conditional_expression: Option<Alignment>,
    /// `alignment_for_conditional_expression_chain`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_conditional_expression_chain",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_conditional_expression_chain: Option<Alignment>,
    /// `alignment_for_enum_constants`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_enum_constants",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_enum_constants: Option<Alignment>,
    /// `alignment_for_expressions_in_array_initializer`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_expressions_in_array_initializer",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_expressions_in_array_initializer: Option<Alignment>,
    /// `alignment_for_expressions_in_for_loop_header`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_expressions_in_for_loop_header",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_expressions_in_for_loop_header: Option<Alignment>,
    /// `alignment_for_expressions_in_switch_case_with_arrow`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_expressions_in_switch_case_with_arrow",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_expressions_in_switch_case_with_arrow: Option<Alignment>,
    /// `alignment_for_expressions_in_switch_case_with_colon`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_expressions_in_switch_case_with_colon",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_expressions_in_switch_case_with_colon: Option<Alignment>,
    /// `alignment_for_logical_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_logical_operator",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_logical_operator: Option<Alignment>,
    /// `alignment_for_method_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_method_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_method_declaration: Option<Alignment>,
    /// `alignment_for_module_statements`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_module_statements",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_module_statements: Option<Alignment>,
    /// `alignment_for_multiple_fields`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_multiple_fields",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_multiple_fields: Option<Alignment>,
    /// `alignment_for_multiplicative_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_multiplicative_operator",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_multiplicative_operator: Option<Alignment>,
    /// `alignment_for_parameterized_type_references`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_parameterized_type_references",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_parameterized_type_references: Option<Alignment>,
    /// `alignment_for_parameters_in_constructor_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_parameters_in_constructor_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_parameters_in_constructor_declaration: Option<Alignment>,
    /// `alignment_for_parameters_in_method_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_parameters_in_method_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_parameters_in_method_declaration: Option<Alignment>,
    /// `alignment_for_permitted_types_in_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_permitted_types_in_type_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_permitted_types_in_type_declaration: Option<Alignment>,
    /// `alignment_for_record_components`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_record_components",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_record_components: Option<Alignment>,
    /// `alignment_for_relational_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_relational_operator",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_relational_operator: Option<Alignment>,
    /// `alignment_for_resources_in_try`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_resources_in_try",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_resources_in_try: Option<Alignment>,
    /// `alignment_for_selector_in_method_invocation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_selector_in_method_invocation",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_selector_in_method_invocation: Option<Alignment>,
    /// `alignment_for_shift_operator`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_shift_operator",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_shift_operator: Option<Alignment>,
    /// `alignment_for_string_concatenation`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_string_concatenation",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_string_concatenation: Option<Alignment>,
    /// `alignment_for_superclass_in_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_superclass_in_type_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_superclass_in_type_declaration: Option<Alignment>,
    /// `alignment_for_superinterfaces_in_enum_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_superinterfaces_in_enum_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_superinterfaces_in_enum_declaration: Option<Alignment>,
    /// `alignment_for_superinterfaces_in_record_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_superinterfaces_in_record_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_superinterfaces_in_record_declaration: Option<Alignment>,
    /// `alignment_for_superinterfaces_in_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_superinterfaces_in_type_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_superinterfaces_in_type_declaration: Option<Alignment>,
    /// `alignment_for_switch_case_with_arrow`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_switch_case_with_arrow",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_switch_case_with_arrow: Option<Alignment>,
    /// `alignment_for_throws_clause_in_constructor_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_throws_clause_in_constructor_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_throws_clause_in_constructor_declaration: Option<Alignment>,
    /// `alignment_for_throws_clause_in_method_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_throws_clause_in_method_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_throws_clause_in_method_declaration: Option<Alignment>,
    /// `alignment_for_type_annotations`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_type_annotations",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_type_annotations: Option<Alignment>,
    /// `alignment_for_type_arguments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_type_arguments",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_type_arguments: Option<Alignment>,
    /// `alignment_for_type_parameters`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_type_parameters",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_type_parameters: Option<Alignment>,
    /// `alignment_for_union_type_in_multicatch`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_union_type_in_multicatch",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_union_type_in_multicatch: Option<Alignment>,
}
