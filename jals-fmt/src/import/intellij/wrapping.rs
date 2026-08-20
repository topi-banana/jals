//! IntelliJ — wrapping, alignment, brace style, brace forcing, and one-line keeping.
//!
//! Three *different* int -> token tables meet in this family and must never be shared:
//! `*_WRAP` ([`IjWrap`]), `*_BRACE_STYLE` ([`IjBraceStyle`]), and `*_BRACE_FORCE`
//! ([`IjForceBraces`]). The `ALIGN_*` settings are column alignment, which jals's single layout
//! engine does not reproduce; they are kept here as the typed record of that divergence
//! (`MAPPING.md` §7, `DESIGN.md` §18.2 D1).

use crate::import::serde_kv;
use serde::Deserialize;

use super::values::{IjBraceStyle, IjForceBraces, IjWrap};

/// IDEA's own default for `WRAP_COMMENTS`: **off**, so a stock IDEA moves no line break inside a
/// comment however much of the Javadoc pass is on. `DESIGN.md` §18.2's **D5**.
pub(crate) const WRAP_COMMENTS_DEFAULT: bool = false;

/// The wrapping, alignment, and brace settings of a Java code style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijWrapping {
    /// `ALIGN_CONSECUTIVE_ASSIGNMENTS` in `<codeStyleSettings language="JAVA">`; `ij_java_align_consecutive_assignments` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_CONSECUTIVE_ASSIGNMENTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_consecutive_assignments: Option<bool>,
    /// `ALIGN_CONSECUTIVE_VARIABLE_DECLARATIONS` in `<codeStyleSettings language="JAVA">`; `ij_java_align_consecutive_variable_declarations` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_CONSECUTIVE_VARIABLE_DECLARATIONS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_consecutive_variable_declarations: Option<bool>,
    /// `ALIGN_GROUP_FIELD_DECLARATIONS` in `<codeStyleSettings language="JAVA">`; `ij_java_align_group_field_declarations` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_GROUP_FIELD_DECLARATIONS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_group_field_declarations: Option<bool>,
    /// `ALIGN_MULTILINE_ANNOTATION_PARAMETERS` in `<JavaCodeStyleSettings>`; `ij_java_align_multiline_annotation_parameters` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_ANNOTATION_PARAMETERS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_annotation_parameters: Option<bool>,
    /// `ALIGN_MULTILINE_ARRAY_INITIALIZER_EXPRESSION` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_array_initializer_expression` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_ARRAY_INITIALIZER_EXPRESSION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_array_initializer_expression: Option<bool>,
    /// `ALIGN_MULTILINE_ASSIGNMENT` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_assignment` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_ASSIGNMENT",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_assignment: Option<bool>,
    /// `ALIGN_MULTILINE_BINARY_OPERATION` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_binary_operation` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_BINARY_OPERATION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_binary_operation: Option<bool>,
    /// `ALIGN_MULTILINE_CHAINED_METHODS` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_chained_methods` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_CHAINED_METHODS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_chained_methods: Option<bool>,
    /// `ALIGN_MULTILINE_DECONSTRUCTION_LIST_COMPONENTS` in `<JavaCodeStyleSettings>`; `ij_java_align_multiline_deconstruction_list_components` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_DECONSTRUCTION_LIST_COMPONENTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_deconstruction_list_components: Option<bool>,
    /// `ALIGN_MULTILINE_EXTENDS_LIST` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_extends_list` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_EXTENDS_LIST",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_extends_list: Option<bool>,
    /// `ALIGN_MULTILINE_FOR` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_for` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_FOR",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_for: Option<bool>,
    /// `ALIGN_MULTILINE_METHOD_BRACKETS` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_method_parentheses` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_METHOD_BRACKETS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_method_brackets: Option<bool>,
    /// `ALIGN_MULTILINE_PARAMETERS` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_parameters` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_PARAMETERS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_parameters: Option<bool>,
    /// `ALIGN_MULTILINE_PARAMETERS_IN_CALLS` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_parameters_in_calls` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_PARAMETERS_IN_CALLS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_parameters_in_calls: Option<bool>,
    /// `ALIGN_MULTILINE_PARENTHESIZED_EXPRESSION` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_parenthesized_expression` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_PARENTHESIZED_EXPRESSION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_parenthesized_expression: Option<bool>,
    /// `ALIGN_MULTILINE_RECORDS` in `<JavaCodeStyleSettings>`; `ij_java_align_multiline_records` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_RECORDS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_records: Option<bool>,
    /// `ALIGN_MULTILINE_RESOURCES` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_resources` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_RESOURCES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_resources: Option<bool>,
    /// `ALIGN_MULTILINE_TERNARY_OPERATION` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_ternary_operation` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_TERNARY_OPERATION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_ternary_operation: Option<bool>,
    /// `ALIGN_MULTILINE_TEXT_BLOCKS` in `<JavaCodeStyleSettings>`; `ij_java_align_multiline_text_blocks` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_TEXT_BLOCKS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_text_blocks: Option<bool>,
    /// `ALIGN_MULTILINE_THROWS_LIST` in `<codeStyleSettings language="JAVA">`; `ij_java_align_multiline_throws_list` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_MULTILINE_THROWS_LIST",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_multiline_throws_list: Option<bool>,
    /// `ALIGN_SUBSEQUENT_SIMPLE_METHODS` in `<codeStyleSettings language="JAVA">`; `ij_java_align_subsequent_simple_methods` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_SUBSEQUENT_SIMPLE_METHODS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_subsequent_simple_methods: Option<bool>,
    /// `ALIGN_THROWS_KEYWORD` in `<codeStyleSettings language="JAVA">`; `ij_java_align_throws_keyword` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_THROWS_KEYWORD",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_throws_keyword: Option<bool>,
    /// `ALIGN_TYPES_IN_MULTI_CATCH` in `<JavaCodeStyleSettings>`; `ij_java_align_types_in_multi_catch` in `.editorconfig`.
    #[serde(
        rename = "ALIGN_TYPES_IN_MULTI_CATCH",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub align_types_in_multi_catch: Option<bool>,
    /// `ANNOTATION_NEW_LINE_IN_RECORD_COMPONENT` in `<JavaCodeStyleSettings>`; `ij_java_annotation_new_line_in_record_component` in `.editorconfig`.
    #[serde(
        rename = "ANNOTATION_NEW_LINE_IN_RECORD_COMPONENT",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub annotation_new_line_in_record_component: Option<bool>,
    /// `ANNOTATION_PARAMETER_WRAP` in `<JavaCodeStyleSettings>`; `ij_java_annotation_parameter_wrap` in `.editorconfig`.
    #[serde(
        rename = "ANNOTATION_PARAMETER_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub annotation_parameter_wrap: Option<IjWrap>,
    /// `ARRAY_INITIALIZER_LBRACE_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_array_initializer_new_line_after_left_brace` in `.editorconfig`.
    #[serde(
        rename = "ARRAY_INITIALIZER_LBRACE_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub array_initializer_lbrace_on_next_line: Option<bool>,
    /// `ARRAY_INITIALIZER_RBRACE_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_array_initializer_right_brace_on_new_line` in `.editorconfig`.
    #[serde(
        rename = "ARRAY_INITIALIZER_RBRACE_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub array_initializer_rbrace_on_next_line: Option<bool>,
    /// `ARRAY_INITIALIZER_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_array_initializer_wrap` in `.editorconfig`.
    #[serde(
        rename = "ARRAY_INITIALIZER_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub array_initializer_wrap: Option<IjWrap>,
    /// `ASSERT_STATEMENT_COLON_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_assert_statement_colon_on_next_line` in `.editorconfig`.
    #[serde(
        rename = "ASSERT_STATEMENT_COLON_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub assert_statement_colon_on_next_line: Option<bool>,
    /// `ASSERT_STATEMENT_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_assert_statement_wrap` in `.editorconfig`.
    #[serde(
        rename = "ASSERT_STATEMENT_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub assert_statement_wrap: Option<IjWrap>,
    /// `ASSIGNMENT_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_assignment_wrap` in `.editorconfig`.
    #[serde(
        rename = "ASSIGNMENT_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub assignment_wrap: Option<IjWrap>,
    /// `BINARY_OPERATION_SIGN_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_binary_operation_sign_on_next_line` in `.editorconfig`.
    #[serde(
        rename = "BINARY_OPERATION_SIGN_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub binary_operation_sign_on_next_line: Option<bool>,
    /// `BINARY_OPERATION_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_binary_operation_wrap` in `.editorconfig`.
    #[serde(
        rename = "BINARY_OPERATION_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub binary_operation_wrap: Option<IjWrap>,
    /// `BRACE_STYLE` in `<codeStyleSettings language="JAVA">`; `ij_java_block_brace_style` in `.editorconfig`.
    #[serde(
        rename = "BRACE_STYLE",
        deserialize_with = "IjBraceStyle::opt_deserialize"
    )]
    pub brace_style: Option<IjBraceStyle>,
    /// `CALL_PARAMETERS_LPAREN_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_call_parameters_new_line_after_left_paren` in `.editorconfig`.
    #[serde(
        rename = "CALL_PARAMETERS_LPAREN_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub call_parameters_lparen_on_next_line: Option<bool>,
    /// `CALL_PARAMETERS_RPAREN_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_call_parameters_right_paren_on_new_line` in `.editorconfig`.
    #[serde(
        rename = "CALL_PARAMETERS_RPAREN_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub call_parameters_rparen_on_next_line: Option<bool>,
    /// `CALL_PARAMETERS_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_call_parameters_wrap` in `.editorconfig`.
    #[serde(
        rename = "CALL_PARAMETERS_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub call_parameters_wrap: Option<IjWrap>,
    /// `CASE_STATEMENT_ON_NEW_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_case_statement_on_separate_line` in `.editorconfig`.
    #[serde(
        rename = "CASE_STATEMENT_ON_NEW_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub case_statement_on_new_line: Option<bool>,
    /// `CATCH_ON_NEW_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_catch_on_new_line` in `.editorconfig`.
    #[serde(rename = "CATCH_ON_NEW_LINE", deserialize_with = "serde_kv::opt_bool")]
    pub catch_on_new_line: Option<bool>,
    /// `CLASS_ANNOTATION_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_class_annotation_wrap` in `.editorconfig`.
    #[serde(
        rename = "CLASS_ANNOTATION_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub class_annotation_wrap: Option<IjWrap>,
    /// `CLASS_BRACE_STYLE` in `<codeStyleSettings language="JAVA">`; `ij_java_class_brace_style` in `.editorconfig`.
    #[serde(
        rename = "CLASS_BRACE_STYLE",
        deserialize_with = "IjBraceStyle::opt_deserialize"
    )]
    pub class_brace_style: Option<IjBraceStyle>,
    /// `DECONSTRUCTION_LIST_WRAP` in `<JavaCodeStyleSettings>`; `ij_java_deconstruction_list_wrap` in `.editorconfig`.
    #[serde(
        rename = "DECONSTRUCTION_LIST_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub deconstruction_list_wrap: Option<IjWrap>,
    /// `DOWHILE_BRACE_FORCE` in `<codeStyleSettings language="JAVA">`; `ij_java_do_while_brace_force` in `.editorconfig`.
    #[serde(
        rename = "DOWHILE_BRACE_FORCE",
        deserialize_with = "IjForceBraces::opt_deserialize"
    )]
    pub dowhile_brace_force: Option<IjForceBraces>,
    /// `DO_NOT_INDENT_TOP_LEVEL_CLASS_MEMBERS` in `<codeStyleSettings language="JAVA">`; `ij_java_do_not_indent_top_level_class_members` in `.editorconfig`.
    #[serde(
        rename = "DO_NOT_INDENT_TOP_LEVEL_CLASS_MEMBERS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub do_not_indent_top_level_class_members: Option<bool>,
    /// `DO_NOT_WRAP_AFTER_SINGLE_ANNOTATION` in `<JavaCodeStyleSettings>`; `ij_java_do_not_wrap_after_single_annotation` in `.editorconfig`.
    #[serde(
        rename = "DO_NOT_WRAP_AFTER_SINGLE_ANNOTATION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub do_not_wrap_after_single_annotation: Option<bool>,
    /// `DO_NOT_WRAP_AFTER_SINGLE_ANNOTATION_IN_PARAMETER` in `<JavaCodeStyleSettings>`; `ij_java_do_not_wrap_after_single_annotation_in_parameter` in `.editorconfig`.
    #[serde(
        rename = "DO_NOT_WRAP_AFTER_SINGLE_ANNOTATION_IN_PARAMETER",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub do_not_wrap_after_single_annotation_in_parameter: Option<bool>,
    /// `ELSE_ON_NEW_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_else_on_new_line` in `.editorconfig`.
    #[serde(rename = "ELSE_ON_NEW_LINE", deserialize_with = "serde_kv::opt_bool")]
    pub else_on_new_line: Option<bool>,
    /// `ENUM_CONSTANTS_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_enum_constants_wrap` in `.editorconfig`.
    #[serde(
        rename = "ENUM_CONSTANTS_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub enum_constants_wrap: Option<IjWrap>,
    /// `ENUM_FIELD_ANNOTATION_WRAP` in `<JavaCodeStyleSettings>`; `ij_java_enum_field_annotation_wrap` in `.editorconfig`.
    #[serde(
        rename = "ENUM_FIELD_ANNOTATION_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub enum_field_annotation_wrap: Option<IjWrap>,
    /// `EXTENDS_KEYWORD_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_extends_keyword_wrap` in `.editorconfig`.
    #[serde(
        rename = "EXTENDS_KEYWORD_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub extends_keyword_wrap: Option<IjWrap>,
    /// `EXTENDS_LIST_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_extends_list_wrap` in `.editorconfig`.
    #[serde(
        rename = "EXTENDS_LIST_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub extends_list_wrap: Option<IjWrap>,
    /// `FIELD_ANNOTATION_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_field_annotation_wrap` in `.editorconfig`.
    #[serde(
        rename = "FIELD_ANNOTATION_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub field_annotation_wrap: Option<IjWrap>,
    /// `FINALLY_ON_NEW_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_finally_on_new_line` in `.editorconfig`.
    #[serde(
        rename = "FINALLY_ON_NEW_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub finally_on_new_line: Option<bool>,
    /// `FORCE_REARRANGE_MODE` in `<codeStyleSettings language="JAVA">`; `ij_java_force_rearrange_mode` in `.editorconfig`.
    #[serde(
        rename = "FORCE_REARRANGE_MODE",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub force_rearrange_mode: Option<i64>,
    /// `FOR_BRACE_FORCE` in `<codeStyleSettings language="JAVA">`; `ij_java_for_brace_force` in `.editorconfig`.
    #[serde(
        rename = "FOR_BRACE_FORCE",
        deserialize_with = "IjForceBraces::opt_deserialize"
    )]
    pub for_brace_force: Option<IjForceBraces>,
    /// `FOR_STATEMENT_LPAREN_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_for_statement_new_line_after_left_paren` in `.editorconfig`.
    #[serde(
        rename = "FOR_STATEMENT_LPAREN_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub for_statement_lparen_on_next_line: Option<bool>,
    /// `FOR_STATEMENT_RPAREN_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_for_statement_right_paren_on_new_line` in `.editorconfig`.
    #[serde(
        rename = "FOR_STATEMENT_RPAREN_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub for_statement_rparen_on_next_line: Option<bool>,
    /// `FOR_STATEMENT_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_for_statement_wrap` in `.editorconfig`.
    #[serde(
        rename = "FOR_STATEMENT_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub for_statement_wrap: Option<IjWrap>,
    /// `IF_BRACE_FORCE` in `<codeStyleSettings language="JAVA">`; `ij_java_if_brace_force` in `.editorconfig`.
    #[serde(
        rename = "IF_BRACE_FORCE",
        deserialize_with = "IjForceBraces::opt_deserialize"
    )]
    pub if_brace_force: Option<IjForceBraces>,
    /// `INDENT_BREAK_FROM_CASE` in `<codeStyleSettings language="JAVA">`; `ij_java_indent_break_from_case` in `.editorconfig`.
    #[serde(
        rename = "INDENT_BREAK_FROM_CASE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_break_from_case: Option<bool>,
    /// `INDENT_CASE_FROM_SWITCH` in `<codeStyleSettings language="JAVA">`; `ij_java_indent_case_from_switch` in `.editorconfig`.
    #[serde(
        rename = "INDENT_CASE_FROM_SWITCH",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub indent_case_from_switch: Option<bool>,
    /// `KEEP_BUILDER_METHODS_INDENTS` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_builder_methods_indents` in `.editorconfig`.
    #[serde(
        rename = "KEEP_BUILDER_METHODS_INDENTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_builder_methods_indents: Option<bool>,
    /// `KEEP_CONTROL_STATEMENT_IN_ONE_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_control_statement_in_one_line` in `.editorconfig`.
    #[serde(
        rename = "KEEP_CONTROL_STATEMENT_IN_ONE_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_control_statement_in_one_line: Option<bool>,
    /// `KEEP_FIRST_COLUMN_COMMENT` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_first_column_comment` in `.editorconfig`.
    #[serde(
        rename = "KEEP_FIRST_COLUMN_COMMENT",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_first_column_comment: Option<bool>,
    /// `KEEP_LINE_BREAKS` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_line_breaks` in `.editorconfig`.
    #[serde(rename = "KEEP_LINE_BREAKS", deserialize_with = "serde_kv::opt_bool")]
    pub keep_line_breaks: Option<bool>,
    /// `KEEP_MULTIPLE_EXPRESSIONS_IN_ONE_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_multiple_expressions_in_one_line` in `.editorconfig`.
    #[serde(
        rename = "KEEP_MULTIPLE_EXPRESSIONS_IN_ONE_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_multiple_expressions_in_one_line: Option<bool>,
    /// `KEEP_SIMPLE_BLOCKS_IN_ONE_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_simple_blocks_in_one_line` in `.editorconfig`.
    #[serde(
        rename = "KEEP_SIMPLE_BLOCKS_IN_ONE_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_simple_blocks_in_one_line: Option<bool>,
    /// `KEEP_SIMPLE_CLASSES_IN_ONE_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_simple_classes_in_one_line` in `.editorconfig`.
    #[serde(
        rename = "KEEP_SIMPLE_CLASSES_IN_ONE_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_simple_classes_in_one_line: Option<bool>,
    /// `KEEP_SIMPLE_LAMBDAS_IN_ONE_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_simple_lambdas_in_one_line` in `.editorconfig`.
    #[serde(
        rename = "KEEP_SIMPLE_LAMBDAS_IN_ONE_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_simple_lambdas_in_one_line: Option<bool>,
    /// `KEEP_SIMPLE_METHODS_IN_ONE_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_simple_methods_in_one_line` in `.editorconfig`.
    #[serde(
        rename = "KEEP_SIMPLE_METHODS_IN_ONE_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub keep_simple_methods_in_one_line: Option<bool>,
    /// `LAMBDA_BRACE_STYLE` in `<codeStyleSettings language="JAVA">`; `ij_java_lambda_brace_style` in `.editorconfig`.
    #[serde(
        rename = "LAMBDA_BRACE_STYLE",
        deserialize_with = "IjBraceStyle::opt_deserialize"
    )]
    pub lambda_brace_style: Option<IjBraceStyle>,
    /// `METHOD_ANNOTATION_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_method_annotation_wrap` in `.editorconfig`.
    #[serde(
        rename = "METHOD_ANNOTATION_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub method_annotation_wrap: Option<IjWrap>,
    /// `METHOD_BRACE_STYLE` in `<codeStyleSettings language="JAVA">`; `ij_java_method_brace_style` in `.editorconfig`.
    #[serde(
        rename = "METHOD_BRACE_STYLE",
        deserialize_with = "IjBraceStyle::opt_deserialize"
    )]
    pub method_brace_style: Option<IjBraceStyle>,
    /// `METHOD_CALL_CHAIN_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_method_call_chain_wrap` in `.editorconfig`.
    #[serde(
        rename = "METHOD_CALL_CHAIN_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub method_call_chain_wrap: Option<IjWrap>,
    /// `METHOD_PARAMETERS_LPAREN_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_method_parameters_new_line_after_left_paren` in `.editorconfig`.
    #[serde(
        rename = "METHOD_PARAMETERS_LPAREN_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub method_parameters_lparen_on_next_line: Option<bool>,
    /// `METHOD_PARAMETERS_RPAREN_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_method_parameters_right_paren_on_new_line` in `.editorconfig`.
    #[serde(
        rename = "METHOD_PARAMETERS_RPAREN_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub method_parameters_rparen_on_next_line: Option<bool>,
    /// `METHOD_PARAMETERS_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_method_parameters_wrap` in `.editorconfig`.
    #[serde(
        rename = "METHOD_PARAMETERS_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub method_parameters_wrap: Option<IjWrap>,
    /// `MODIFIER_LIST_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_modifier_list_wrap` in `.editorconfig`.
    #[serde(rename = "MODIFIER_LIST_WRAP", deserialize_with = "serde_kv::opt_bool")]
    pub modifier_list_wrap: Option<bool>,
    /// `MULTI_CATCH_TYPES_WRAP` in `<JavaCodeStyleSettings>`; `ij_java_multi_catch_types_wrap` in `.editorconfig`.
    #[serde(
        rename = "MULTI_CATCH_TYPES_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub multi_catch_types_wrap: Option<IjWrap>,
    /// `NEW_LINE_AFTER_LPAREN_IN_ANNOTATION` in `<JavaCodeStyleSettings>`; `ij_java_new_line_after_lparen_in_annotation` in `.editorconfig`.
    #[serde(
        rename = "NEW_LINE_AFTER_LPAREN_IN_ANNOTATION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub new_line_after_lparen_in_annotation: Option<bool>,
    /// `NEW_LINE_AFTER_LPAREN_IN_DECONSTRUCTION_PATTERN` in `<JavaCodeStyleSettings>`; `ij_java_new_line_after_lparen_in_deconstruction_pattern` in `.editorconfig`.
    #[serde(
        rename = "NEW_LINE_AFTER_LPAREN_IN_DECONSTRUCTION_PATTERN",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub new_line_after_lparen_in_deconstruction_pattern: Option<bool>,
    /// `NEW_LINE_AFTER_LPAREN_IN_RECORD_HEADER` in `<JavaCodeStyleSettings>`; `ij_java_new_line_after_lparen_in_record_header` in `.editorconfig`.
    #[serde(
        rename = "NEW_LINE_AFTER_LPAREN_IN_RECORD_HEADER",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub new_line_after_lparen_in_record_header: Option<bool>,
    /// `NEW_LINE_WHEN_BODY_IS_PRESENTED` in `<JavaCodeStyleSettings>`; `ij_java_new_line_when_body_is_presented` in `.editorconfig`.
    #[serde(
        rename = "NEW_LINE_WHEN_BODY_IS_PRESENTED",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub new_line_when_body_is_presented: Option<bool>,
    /// `PARAMETER_ANNOTATION_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_parameter_annotation_wrap` in `.editorconfig`.
    #[serde(
        rename = "PARAMETER_ANNOTATION_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub parameter_annotation_wrap: Option<IjWrap>,
    /// `PARENTHESES_EXPRESSION_LPAREN_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_parentheses_expression_new_line_after_left_paren` in `.editorconfig`.
    #[serde(
        rename = "PARENTHESES_EXPRESSION_LPAREN_WRAP",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub parentheses_expression_lparen_wrap: Option<bool>,
    /// `PARENTHESES_EXPRESSION_RPAREN_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_parentheses_expression_right_paren_on_new_line` in `.editorconfig`.
    #[serde(
        rename = "PARENTHESES_EXPRESSION_RPAREN_WRAP",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub parentheses_expression_rparen_wrap: Option<bool>,
    /// `PLACE_ASSIGNMENT_SIGN_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_place_assignment_sign_on_next_line` in `.editorconfig`.
    #[serde(
        rename = "PLACE_ASSIGNMENT_SIGN_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub place_assignment_sign_on_next_line: Option<bool>,
    /// `PREFER_PARAMETERS_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_prefer_parameters_wrap` in `.editorconfig`.
    #[serde(
        rename = "PREFER_PARAMETERS_WRAP",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub prefer_parameters_wrap: Option<bool>,
    /// `RECORD_COMPONENTS_WRAP` in `<JavaCodeStyleSettings>`; `ij_java_record_components_wrap` in `.editorconfig`.
    #[serde(
        rename = "RECORD_COMPONENTS_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub record_components_wrap: Option<IjWrap>,
    /// `RESOURCE_LIST_LPAREN_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_resource_list_new_line_after_left_paren` in `.editorconfig`.
    #[serde(
        rename = "RESOURCE_LIST_LPAREN_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub resource_list_lparen_on_next_line: Option<bool>,
    /// `RESOURCE_LIST_RPAREN_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_resource_list_right_paren_on_new_line` in `.editorconfig`.
    #[serde(
        rename = "RESOURCE_LIST_RPAREN_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub resource_list_rparen_on_next_line: Option<bool>,
    /// `RESOURCE_LIST_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_resource_list_wrap` in `.editorconfig`.
    #[serde(
        rename = "RESOURCE_LIST_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub resource_list_wrap: Option<IjWrap>,
    /// `RPAREN_ON_NEW_LINE_IN_ANNOTATION` in `<JavaCodeStyleSettings>`; `ij_java_rparen_on_new_line_in_annotation` in `.editorconfig`.
    #[serde(
        rename = "RPAREN_ON_NEW_LINE_IN_ANNOTATION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub rparen_on_new_line_in_annotation: Option<bool>,
    /// `RPAREN_ON_NEW_LINE_IN_DECONSTRUCTION_PATTERN` in `<JavaCodeStyleSettings>`; `ij_java_rparen_on_new_line_in_deconstruction_pattern` in `.editorconfig`.
    #[serde(
        rename = "RPAREN_ON_NEW_LINE_IN_DECONSTRUCTION_PATTERN",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub rparen_on_new_line_in_deconstruction_pattern: Option<bool>,
    /// `RPAREN_ON_NEW_LINE_IN_RECORD_HEADER` in `<JavaCodeStyleSettings>`; `ij_java_rparen_on_new_line_in_record_header` in `.editorconfig`.
    #[serde(
        rename = "RPAREN_ON_NEW_LINE_IN_RECORD_HEADER",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub rparen_on_new_line_in_record_header: Option<bool>,
    /// `SPECIAL_ELSE_IF_TREATMENT` in `<codeStyleSettings language="JAVA">`; `ij_java_special_else_if_treatment` in `.editorconfig`.
    #[serde(
        rename = "SPECIAL_ELSE_IF_TREATMENT",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub special_else_if_treatment: Option<bool>,
    /// `SWITCH_EXPRESSIONS_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_switch_expressions_wrap` in `.editorconfig`.
    #[serde(
        rename = "SWITCH_EXPRESSIONS_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub switch_expressions_wrap: Option<IjWrap>,
    /// `TERNARY_OPERATION_SIGNS_ON_NEXT_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_ternary_operation_signs_on_next_line` in `.editorconfig`.
    #[serde(
        rename = "TERNARY_OPERATION_SIGNS_ON_NEXT_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub ternary_operation_signs_on_next_line: Option<bool>,
    /// `TERNARY_OPERATION_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_ternary_operation_wrap` in `.editorconfig`.
    #[serde(
        rename = "TERNARY_OPERATION_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub ternary_operation_wrap: Option<IjWrap>,
    /// `THROWS_KEYWORD_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_throws_keyword_wrap` in `.editorconfig`.
    #[serde(
        rename = "THROWS_KEYWORD_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub throws_keyword_wrap: Option<IjWrap>,
    /// `THROWS_LIST_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_throws_list_wrap` in `.editorconfig`.
    #[serde(
        rename = "THROWS_LIST_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub throws_list_wrap: Option<IjWrap>,
    /// `VARIABLE_ANNOTATION_WRAP` in `<codeStyleSettings language="JAVA">`; `ij_java_variable_annotation_wrap` in `.editorconfig`.
    #[serde(
        rename = "VARIABLE_ANNOTATION_WRAP",
        deserialize_with = "IjWrap::opt_deserialize"
    )]
    pub variable_annotation_wrap: Option<IjWrap>,
    /// `WHILE_BRACE_FORCE` in `<codeStyleSettings language="JAVA">`; `ij_java_while_brace_force` in `.editorconfig`.
    #[serde(
        rename = "WHILE_BRACE_FORCE",
        deserialize_with = "IjForceBraces::opt_deserialize"
    )]
    pub while_brace_force: Option<IjForceBraces>,
    /// `WHILE_ON_NEW_LINE` in `<codeStyleSettings language="JAVA">`; `ij_java_while_on_new_line` in `.editorconfig`.
    #[serde(rename = "WHILE_ON_NEW_LINE", deserialize_with = "serde_kv::opt_bool")]
    pub while_on_new_line: Option<bool>,
    /// `WRAP_COMMENTS` in `<codeStyleSettings language="JAVA">`; `ij_java_wrap_comments` in `.editorconfig`.
    #[serde(rename = "WRAP_COMMENTS", deserialize_with = "serde_kv::opt_bool")]
    pub wrap_comments: Option<bool>,
    /// `WRAP_FIRST_METHOD_IN_CALL_CHAIN` in `<codeStyleSettings language="JAVA">`; `ij_java_wrap_first_method_in_call_chain` in `.editorconfig`.
    #[serde(
        rename = "WRAP_FIRST_METHOD_IN_CALL_CHAIN",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub wrap_first_method_in_call_chain: Option<bool>,
    /// `WRAP_LONG_LINES` in `<codeStyleSettings language="JAVA">`; `ij_java_wrap_long_lines` in `.editorconfig`.
    #[serde(rename = "WRAP_LONG_LINES", deserialize_with = "serde_kv::opt_bool")]
    pub wrap_long_lines: Option<bool>,
    /// `WRAP_ON_TYPING` in `<codeStyleSettings language="JAVA">`; `ij_java_wrap_on_typing` in `.editorconfig`.
    #[serde(rename = "WRAP_ON_TYPING", deserialize_with = "serde_kv::opt_number")]
    pub wrap_on_typing: Option<i64>,
    /// `WRAP_SEMICOLON_AFTER_CALL_CHAIN` in `<JavaCodeStyleSettings>`; `ij_java_wrap_semicolon_after_call_chain` in `.editorconfig`.
    #[serde(
        rename = "WRAP_SEMICOLON_AFTER_CALL_CHAIN",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub wrap_semicolon_after_call_chain: Option<bool>,
}
