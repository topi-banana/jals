//! `[wrapping]` — how a construct breaks across lines when it does not fit.
//!
//! This is the section the old rule set was missing entirely. Every Java formatter drives
//! wrapping from **one column limit plus a per-construct policy** — Eclipse's 53
//! `alignment_for_*` bitmasks, IntelliJ's 26 `*_WRAP` enums — never from rustfmt-style
//! per-construct *width thresholds*. [`WrapPolicy`] is the shared four-valued vocabulary those
//! two encodings collapse onto (`jals-fmt/MAPPING.md` §5.4).

use serde::{Deserialize, Serialize};

/// How a construct lays out when it does not fit the column limit.
///
/// Four values, chosen so that Eclipse's `alignment_for_*` bit encoding and IntelliJ's `*_WRAP`
/// tokens both land losslessly (`MAPPING.md` §5.4 has the table). IntelliJ's token names are
/// counter-intuitive — `split_into_lines` is *Wrap Always* and `on_every_item` is *Chop Down If
/// Long* — which is exactly why the shared vocabulary is spelled out here instead of reusing
/// either vendor's names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WrapPolicy {
    /// Never break here, even past the column limit. Eclipse's `Integer.MAX_VALUE` sentinel /
    /// IntelliJ `off`.
    Never,
    /// Break only on overflow, packing as many items per line as fit (a *fill*). Eclipse
    /// `M_COMPACT_SPLIT` / IntelliJ `normal`.
    IfLong,
    /// Break only on overflow, then one item per line. Eclipse `M_ONE_PER_LINE_SPLIT` without
    /// `M_FORCE` / IntelliJ `on_every_item`.
    IfLongPerItem,
    /// Always break, one item per line, even when the construct would fit. Eclipse's split bits
    /// with `M_FORCE` / IntelliJ `split_into_lines`.
    AlwaysPerItem,
}

/// Where the delimiters of a wrapped, paren- or brace-delimited list are placed.
///
/// Eclipse's `parentheses_positions_in_*` vocabulary. IntelliJ spells the same decision as two
/// booleans (`*_LPAREN_ON_NEXT_LINE` / `*_RPAREN_ON_NEXT_LINE`); its two asymmetric combinations
/// have no value here and fold onto [`SeparateLines`](Self::SeparateLines) on import, staying
/// visible as the original pair in the native model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ParenPositions {
    /// Both delimiters share a line with the adjacent item — the opening one ends the header
    /// line and the closing one hugs the last item. Eclipse `common_lines`; what
    /// google-java-format always does.
    CommonLines,
    /// Each delimiter takes its own line, but only when the list actually wrapped. Eclipse
    /// `separate_lines_if_wrapped`.
    SeparateLinesIfWrapped,
    /// Each delimiter takes its own line unless the list is empty. Eclipse
    /// `separate_lines_if_not_empty`.
    SeparateLinesIfNotEmpty,
    /// Each delimiter always takes its own line. Eclipse `separate_lines`.
    SeparateLines,
    /// Keep the delimiters wherever the source put them. Eclipse `preserve_positions`; reads
    /// input whitespace, which the single engine does not do: it rounds this to
    /// [`CommonLines`](Self::CommonLines) and warns (`DESIGN.md` §17).
    Preserve,
}

/// Per-construct wrapping policy and break placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
#[allow(clippy::struct_excessive_bools)]
pub struct Wrapping {
    /// A call's argument list. Eclipse `alignment_for_arguments_in_method_invocation` /
    /// IntelliJ `CALL_PARAMETERS_WRAP`.
    pub call_arguments: WrapPolicy,
    /// A method / constructor parameter list. Eclipse
    /// `alignment_for_parameters_in_method_declaration` / IntelliJ `METHOD_PARAMETERS_WRAP`.
    pub method_parameters: WrapPolicy,
    /// A record header's component list. Eclipse `alignment_for_record_components` /
    /// IntelliJ `RECORD_COMPONENTS_WRAP`.
    pub record_components: WrapPolicy,
    /// A `try`-with-resources resource list. Eclipse `alignment_for_resources_in_try` /
    /// IntelliJ `RESOURCE_LIST_WRAP`.
    pub resource_list: WrapPolicy,
    /// A `throws` clause. Eclipse `alignment_for_throws_clause_in_method_declaration` /
    /// IntelliJ `THROWS_LIST_WRAP`.
    pub throws_list: WrapPolicy,
    /// An `extends` / `implements` list. Eclipse
    /// `alignment_for_superinterfaces_in_type_declaration` / IntelliJ `EXTENDS_LIST_WRAP`.
    pub extends_list: WrapPolicy,
    /// An `enum`'s constant list. Eclipse `alignment_for_enum_constants` / IntelliJ
    /// `ENUM_CONSTANTS_WRAP`.
    pub enum_constants: WrapPolicy,
    /// An array initializer's elements. Eclipse
    /// `alignment_for_expressions_in_array_initializer` / IntelliJ `ARRAY_INITIALIZER_WRAP`.
    pub array_initializer: WrapPolicy,
    /// An annotation's argument list. Eclipse `alignment_for_arguments_in_annotation` /
    /// IntelliJ `ANNOTATION_PARAMETER_WRAP`.
    pub annotation_arguments: WrapPolicy,
    /// A type-argument list (`Map<K, V>`). Eclipse
    /// `alignment_for_parameterized_type_references`.
    pub type_arguments: WrapPolicy,
    /// A type-parameter list (`<T extends A>`). Eclipse `alignment_for_type_parameters`.
    pub type_parameters: WrapPolicy,
    /// A multi-`catch` type list (`A | B`). Eclipse `alignment_for_union_type_in_multicatch` /
    /// IntelliJ `MULTI_CATCH_TYPES_WRAP`.
    pub multi_catch_types: WrapPolicy,
    /// A record deconstruction pattern's component list. IntelliJ `DECONSTRUCTION_LIST_WRAP`.
    pub deconstruction_list: WrapPolicy,
    /// A `case` label's constant list (`case A, B ->`). Eclipse
    /// `alignment_for_expressions_in_switch_case_with_colon`; google-java-format wraps a long
    /// label list, IntelliJ never does.
    pub case_labels: WrapPolicy,
    /// A method call chain (`a.b().c()`). Eclipse `alignment_for_selector_in_method_invocation` /
    /// IntelliJ `METHOD_CALL_CHAIN_WRAP`.
    pub method_chain: WrapPolicy,
    /// A same-precedence binary-operator run. Eclipse's seven per-operator-class
    /// `alignment_for_*_operator` settings / IntelliJ `BINARY_OPERATION_WRAP`.
    pub binary_operation: WrapPolicy,
    /// A ternary conditional. Eclipse `alignment_for_conditional_expression` / IntelliJ
    /// `TERNARY_OPERATION_WRAP`.
    pub ternary: WrapPolicy,
    /// An assignment or variable initializer's right-hand side. Eclipse
    /// `alignment_for_assignment` / IntelliJ `ASSIGNMENT_WRAP`.
    pub assignment: WrapPolicy,
    /// A basic-`for` header. Eclipse `alignment_for_expressions_in_for_loop_header` /
    /// IntelliJ `FOR_STATEMENT_WRAP`.
    pub for_statement: WrapPolicy,
    /// An `assert` statement. Eclipse `alignment_for_assertion_message` / IntelliJ
    /// `ASSERT_STATEMENT_WRAP`.
    pub assert_statement: WrapPolicy,
    /// A `switch` expression's arms. Eclipse
    /// `alignment_for_expressions_in_switch_case_with_arrow` / IntelliJ `SWITCH_EXPRESSIONS_WRAP`.
    pub switch_expression: WrapPolicy,
    /// A type declaration's leading annotations. Eclipse
    /// `insert_new_line_after_annotation_on_type` / IntelliJ `CLASS_ANNOTATION_WRAP`.
    pub type_annotations: WrapPolicy,
    /// A method / constructor declaration's leading annotations. IntelliJ `METHOD_ANNOTATION_WRAP`.
    pub method_annotations: WrapPolicy,
    /// A field declaration's leading annotations. IntelliJ `FIELD_ANNOTATION_WRAP`.
    pub field_annotations: WrapPolicy,
    /// A parameter's annotations. IntelliJ `PARAMETER_ANNOTATION_WRAP`.
    pub parameter_annotations: WrapPolicy,
    /// A local-variable declaration's annotations. IntelliJ `VARIABLE_ANNOTATION_WRAP`.
    pub variable_annotations: WrapPolicy,
    /// Put a wrapped binary operator at the start of the continuation line rather than at the
    /// end of the broken line. Eclipse `wrap_before_additive_operator` and its six siblings /
    /// IntelliJ `BINARY_OPERATION_SIGN_ON_NEXT_LINE`.
    pub before_binary_operator: bool,
    /// Same, for a ternary's `?` / `:`. Eclipse `wrap_before_conditional_operator` /
    /// IntelliJ `TERNARY_OPERATION_SIGNS_ON_NEXT_LINE`.
    pub before_ternary_operator: bool,
    /// Same, for an assignment operator. Eclipse `wrap_before_assignment_operator` /
    /// IntelliJ `PLACE_ASSIGNMENT_SIGN_ON_NEXT_LINE`.
    pub before_assignment_operator: bool,
    /// Same, for the `.` of a wrapped method chain. Eclipse
    /// `wrap_before_or_operator_multicatch`'s sibling `alignment_for_selector_in_method_invocation`
    /// break position.
    pub before_method_chain_dot: bool,
    /// Same, for a list separator comma — the break falls *before* the comma. Eclipse
    /// `wrap_before_comma_in_*`.
    pub before_comma: bool,
    /// Same, for an `assert` message colon. Eclipse `wrap_before_assertion_message_operator` /
    /// IntelliJ `ASSERT_STATEMENT_COLON_ON_NEXT_LINE`.
    pub before_assert_colon: bool,
    /// Break before the *first* call of a wrapped chain too, instead of leaving it on the
    /// receiver's line. IntelliJ `WRAP_FIRST_METHOD_IN_CALL_CHAIN`.
    pub wrap_first_method_in_chain: bool,
    /// Delimiters of a method / constructor parameter list. Eclipse
    /// `parenthesis_positions_in_method_declaration` (spelled `..._method_delcaration` in the
    /// product) / IntelliJ `METHOD_PARAMETERS_{L,R}PAREN_ON_NEXT_LINE`.
    pub paren_method_declaration: ParenPositions,
    /// Delimiters of a call's argument list. Eclipse `parenthesis_positions_in_method_invocation` /
    /// IntelliJ `CALL_PARAMETERS_{L,R}PAREN_ON_NEXT_LINE`.
    pub paren_method_invocation: ParenPositions,
    /// Delimiters of a control-flow header's parentheses (`if` / `while` / `for` / `switch` /
    /// `try` / `catch`). Eclipse's five `parenthesis_positions_in_*` statement settings /
    /// IntelliJ `FOR_STATEMENT_{L,R}PAREN_ON_NEXT_LINE` and `RESOURCE_LIST_*`.
    pub paren_control: ParenPositions,
    /// Delimiters of an annotation's argument list. Eclipse `parenthesis_positions_in_annotation` /
    /// IntelliJ `NEW_LINE_AFTER_LPAREN_IN_ANNOTATION` / `RPAREN_ON_NEW_LINE_IN_ANNOTATION`.
    pub paren_annotation: ParenPositions,
    /// Delimiters of a lambda's parameter list. Eclipse
    /// `parenthesis_positions_in_lambda_declaration`.
    pub paren_lambda: ParenPositions,
    /// Delimiters of a record header. Eclipse `parenthesis_positions_in_record_declaration` /
    /// IntelliJ `NEW_LINE_AFTER_LPAREN_IN_RECORD_HEADER` / `RPAREN_ON_NEW_LINE_IN_RECORD_HEADER`.
    pub paren_record: ParenPositions,
    /// Rejoin lines the source broke but the policy would keep together. Eclipse
    /// `join_wrapped_lines` / IntelliJ `KEEP_LINE_BREAKS` (inverted). Off means the source's
    /// breaks survive, which reads input whitespace — so the single engine rounds this back to
    /// `true` (always rejoin) and warns (`DESIGN.md` §17).
    pub join_wrapped_lines: bool,
    /// Break lines that exceed the column limit even where no policy allows a break.
    /// IntelliJ `WRAP_LONG_LINES`.
    pub wrap_long_lines: bool,
    /// Preserve the *tabular* layout of a grid-shaped array initializer instead of reflowing it
    /// by width. google-java-format keeps such tables; Eclipse and IntelliJ reflow them.
    pub tabular_array_initializers: bool,
}

impl Default for Wrapping {
    fn default() -> Self {
        Self {
            call_arguments: WrapPolicy::IfLong,
            method_parameters: WrapPolicy::IfLong,
            record_components: WrapPolicy::IfLong,
            resource_list: WrapPolicy::IfLong,
            throws_list: WrapPolicy::IfLong,
            extends_list: WrapPolicy::IfLong,
            enum_constants: WrapPolicy::IfLong,
            array_initializer: WrapPolicy::IfLong,
            annotation_arguments: WrapPolicy::IfLong,
            type_arguments: WrapPolicy::IfLong,
            type_parameters: WrapPolicy::IfLong,
            multi_catch_types: WrapPolicy::IfLong,
            deconstruction_list: WrapPolicy::IfLong,
            case_labels: WrapPolicy::Never,
            method_chain: WrapPolicy::IfLong,
            binary_operation: WrapPolicy::IfLong,
            ternary: WrapPolicy::IfLong,
            assignment: WrapPolicy::IfLong,
            for_statement: WrapPolicy::IfLong,
            assert_statement: WrapPolicy::IfLong,
            switch_expression: WrapPolicy::IfLong,
            type_annotations: WrapPolicy::AlwaysPerItem,
            method_annotations: WrapPolicy::AlwaysPerItem,
            field_annotations: WrapPolicy::AlwaysPerItem,
            parameter_annotations: WrapPolicy::Never,
            variable_annotations: WrapPolicy::Never,
            before_binary_operator: true,
            before_ternary_operator: true,
            before_assignment_operator: false,
            before_method_chain_dot: true,
            before_comma: false,
            before_assert_colon: true,
            wrap_first_method_in_chain: false,
            paren_method_declaration: ParenPositions::CommonLines,
            paren_method_invocation: ParenPositions::CommonLines,
            paren_control: ParenPositions::CommonLines,
            paren_annotation: ParenPositions::CommonLines,
            paren_lambda: ParenPositions::CommonLines,
            paren_record: ParenPositions::CommonLines,
            join_wrapped_lines: true,
            wrap_long_lines: false,
            tabular_array_initializers: false,
        }
    }
}
