//! IntelliJ — the `SPACE_*` / `SPACES_*` settings.
//!
//! Note the editorconfig spelling rewrite `PropertyNameUtil` applies here and nowhere else:
//! a `SPACE_WITHIN*` field becomes `spaces_within*` and `SPACE_AROUND*` becomes `spaces_around*`.

use crate::import::serde_kv;
use serde::Deserialize;

/// The spacing settings of a Java code style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijSpacing {
    /// `SPACES_INSIDE_BLOCK_BRACES_WHEN_BODY_IS_PRESENT` in `<JavaCodeStyleSettings>`; `ij_java_spaces_inside_block_braces_when_body_is_present` in `.editorconfig`.
    #[serde(
        rename = "SPACES_INSIDE_BLOCK_BRACES_WHEN_BODY_IS_PRESENT",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub spaces_inside_block_braces_when_body_is_present: Option<bool>,
    /// `SPACES_WITHIN_ANGLE_BRACKETS` in `<JavaCodeStyleSettings>`; `ij_java_spaces_within_angle_brackets` in `.editorconfig`.
    #[serde(
        rename = "SPACES_WITHIN_ANGLE_BRACKETS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub spaces_within_angle_brackets: Option<bool>,
    /// `SPACE_AFTER_CLOSING_ANGLE_BRACKET_IN_TYPE_ARGUMENT` in `<JavaCodeStyleSettings>`; `ij_java_space_after_closing_angle_bracket_in_type_argument` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AFTER_CLOSING_ANGLE_BRACKET_IN_TYPE_ARGUMENT",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_after_closing_angle_bracket_in_type_argument: Option<bool>,
    /// `SPACE_AFTER_COLON` in `<codeStyleSettings language="JAVA">`; `ij_java_space_after_colon` in `.editorconfig`.
    #[serde(rename = "SPACE_AFTER_COLON", deserialize_with = "serde_kv::opt_bool")]
    pub space_after_colon: Option<bool>,
    /// `SPACE_AFTER_COMMA` in `<codeStyleSettings language="JAVA">`; `ij_java_space_after_comma` in `.editorconfig`.
    #[serde(rename = "SPACE_AFTER_COMMA", deserialize_with = "serde_kv::opt_bool")]
    pub space_after_comma: Option<bool>,
    /// `SPACE_AFTER_COMMA_IN_TYPE_ARGUMENTS` in `<codeStyleSettings language="JAVA">`; `ij_java_space_after_comma_in_type_arguments` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AFTER_COMMA_IN_TYPE_ARGUMENTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_after_comma_in_type_arguments: Option<bool>,
    /// `SPACE_AFTER_QUEST` in `<codeStyleSettings language="JAVA">`; `ij_java_space_after_quest` in `.editorconfig`.
    #[serde(rename = "SPACE_AFTER_QUEST", deserialize_with = "serde_kv::opt_bool")]
    pub space_after_quest: Option<bool>,
    /// `SPACE_AFTER_SEMICOLON` in `<codeStyleSettings language="JAVA">`; `ij_java_space_after_for_semicolon` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AFTER_SEMICOLON",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_after_semicolon: Option<bool>,
    /// `SPACE_AFTER_TYPE_CAST` in `<codeStyleSettings language="JAVA">`; `ij_java_space_after_type_cast` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AFTER_TYPE_CAST",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_after_type_cast: Option<bool>,
    /// `SPACE_AROUND_ADDITIVE_OPERATORS` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_additive_operators` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_ADDITIVE_OPERATORS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_additive_operators: Option<bool>,
    /// `SPACE_AROUND_ANNOTATION_EQ` in `<JavaCodeStyleSettings>`; `ij_java_spaces_around_annotation_eq` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_ANNOTATION_EQ",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_annotation_eq: Option<bool>,
    /// `SPACE_AROUND_ASSIGNMENT_OPERATORS` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_assignment_operators` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_ASSIGNMENT_OPERATORS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_assignment_operators: Option<bool>,
    /// `SPACE_AROUND_BITWISE_OPERATORS` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_bitwise_operators` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_BITWISE_OPERATORS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_bitwise_operators: Option<bool>,
    /// `SPACE_AROUND_EQUALITY_OPERATORS` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_equality_operators` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_EQUALITY_OPERATORS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_equality_operators: Option<bool>,
    /// `SPACE_AROUND_LAMBDA_ARROW` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_lambda_arrow` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_LAMBDA_ARROW",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_lambda_arrow: Option<bool>,
    /// `SPACE_AROUND_LOGICAL_OPERATORS` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_logical_operators` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_LOGICAL_OPERATORS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_logical_operators: Option<bool>,
    /// `SPACE_AROUND_METHOD_REF_DBL_COLON` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_method_ref_dbl_colon` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_METHOD_REF_DBL_COLON",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_method_ref_dbl_colon: Option<bool>,
    /// `SPACE_AROUND_MULTIPLICATIVE_OPERATORS` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_multiplicative_operators` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_MULTIPLICATIVE_OPERATORS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_multiplicative_operators: Option<bool>,
    /// `SPACE_AROUND_RELATIONAL_OPERATORS` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_relational_operators` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_RELATIONAL_OPERATORS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_relational_operators: Option<bool>,
    /// `SPACE_AROUND_SHIFT_OPERATORS` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_shift_operators` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_SHIFT_OPERATORS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_shift_operators: Option<bool>,
    /// `SPACE_AROUND_TYPE_BOUNDS_IN_TYPE_PARAMETERS` in `<JavaCodeStyleSettings>`; `ij_java_spaces_around_type_bounds_in_type_parameters` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_TYPE_BOUNDS_IN_TYPE_PARAMETERS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_type_bounds_in_type_parameters: Option<bool>,
    /// `SPACE_AROUND_UNARY_OPERATOR` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_around_unary_operator` in `.editorconfig`.
    #[serde(
        rename = "SPACE_AROUND_UNARY_OPERATOR",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_around_unary_operator: Option<bool>,
    /// `SPACE_BEFORE_ANNOTATION_ARRAY_INITIALIZER_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_annotation_array_initializer_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_ANNOTATION_ARRAY_INITIALIZER_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_annotation_array_initializer_lbrace: Option<bool>,
    /// `SPACE_BEFORE_ANOTATION_PARAMETER_LIST` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_annotation_parameter_list` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_ANOTATION_PARAMETER_LIST",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_anotation_parameter_list: Option<bool>,
    /// `SPACE_BEFORE_ARRAY_INITIALIZER_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_array_initializer_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_ARRAY_INITIALIZER_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_array_initializer_lbrace: Option<bool>,
    /// `SPACE_BEFORE_CATCH_KEYWORD` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_catch_keyword` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_CATCH_KEYWORD",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_catch_keyword: Option<bool>,
    /// `SPACE_BEFORE_CATCH_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_catch_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_CATCH_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_catch_lbrace: Option<bool>,
    /// `SPACE_BEFORE_CATCH_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_catch_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_CATCH_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_catch_parentheses: Option<bool>,
    /// `SPACE_BEFORE_CLASS_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_class_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_CLASS_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_class_lbrace: Option<bool>,
    /// `SPACE_BEFORE_COLON` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_colon` in `.editorconfig`.
    #[serde(rename = "SPACE_BEFORE_COLON", deserialize_with = "serde_kv::opt_bool")]
    pub space_before_colon: Option<bool>,
    /// `SPACE_BEFORE_COLON_IN_FOREACH` in `<JavaCodeStyleSettings>`; `ij_java_space_before_colon_in_foreach` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_COLON_IN_FOREACH",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_colon_in_foreach: Option<bool>,
    /// `SPACE_BEFORE_COMMA` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_comma` in `.editorconfig`.
    #[serde(rename = "SPACE_BEFORE_COMMA", deserialize_with = "serde_kv::opt_bool")]
    pub space_before_comma: Option<bool>,
    /// `SPACE_BEFORE_DECONSTRUCTION_LIST` in `<JavaCodeStyleSettings>`; `ij_java_space_before_deconstruction_list` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_DECONSTRUCTION_LIST",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_deconstruction_list: Option<bool>,
    /// `SPACE_BEFORE_DO_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_do_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_DO_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_do_lbrace: Option<bool>,
    /// `SPACE_BEFORE_ELSE_KEYWORD` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_else_keyword` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_ELSE_KEYWORD",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_else_keyword: Option<bool>,
    /// `SPACE_BEFORE_ELSE_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_else_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_ELSE_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_else_lbrace: Option<bool>,
    /// `SPACE_BEFORE_FINALLY_KEYWORD` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_finally_keyword` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_FINALLY_KEYWORD",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_finally_keyword: Option<bool>,
    /// `SPACE_BEFORE_FINALLY_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_finally_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_FINALLY_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_finally_lbrace: Option<bool>,
    /// `SPACE_BEFORE_FOR_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_for_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_FOR_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_for_lbrace: Option<bool>,
    /// `SPACE_BEFORE_FOR_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_for_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_FOR_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_for_parentheses: Option<bool>,
    /// `SPACE_BEFORE_IF_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_if_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_IF_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_if_lbrace: Option<bool>,
    /// `SPACE_BEFORE_IF_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_if_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_IF_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_if_parentheses: Option<bool>,
    /// `SPACE_BEFORE_METHOD_CALL_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_method_call_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_METHOD_CALL_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_method_call_parentheses: Option<bool>,
    /// `SPACE_BEFORE_METHOD_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_method_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_METHOD_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_method_lbrace: Option<bool>,
    /// `SPACE_BEFORE_METHOD_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_method_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_METHOD_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_method_parentheses: Option<bool>,
    /// `SPACE_BEFORE_OPENING_ANGLE_BRACKET_IN_TYPE_PARAMETER` in `<JavaCodeStyleSettings>`; `ij_java_space_before_opening_angle_bracket_in_type_parameter` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_OPENING_ANGLE_BRACKET_IN_TYPE_PARAMETER",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_opening_angle_bracket_in_type_parameter: Option<bool>,
    /// `SPACE_BEFORE_QUEST` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_quest` in `.editorconfig`.
    #[serde(rename = "SPACE_BEFORE_QUEST", deserialize_with = "serde_kv::opt_bool")]
    pub space_before_quest: Option<bool>,
    /// `SPACE_BEFORE_SEMICOLON` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_for_semicolon` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_SEMICOLON",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_semicolon: Option<bool>,
    /// `SPACE_BEFORE_SWITCH_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_switch_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_SWITCH_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_switch_lbrace: Option<bool>,
    /// `SPACE_BEFORE_SWITCH_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_switch_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_SWITCH_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_switch_parentheses: Option<bool>,
    /// `SPACE_BEFORE_SYNCHRONIZED_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_synchronized_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_SYNCHRONIZED_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_synchronized_lbrace: Option<bool>,
    /// `SPACE_BEFORE_SYNCHRONIZED_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_synchronized_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_SYNCHRONIZED_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_synchronized_parentheses: Option<bool>,
    /// `SPACE_BEFORE_TRY_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_try_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_TRY_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_try_lbrace: Option<bool>,
    /// `SPACE_BEFORE_TRY_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_try_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_TRY_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_try_parentheses: Option<bool>,
    /// `SPACE_BEFORE_TYPE_PARAMETER_LIST` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_type_parameter_list` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_TYPE_PARAMETER_LIST",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_type_parameter_list: Option<bool>,
    /// `SPACE_BEFORE_WHILE_KEYWORD` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_while_keyword` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_WHILE_KEYWORD",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_while_keyword: Option<bool>,
    /// `SPACE_BEFORE_WHILE_LBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_while_left_brace` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_WHILE_LBRACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_while_lbrace: Option<bool>,
    /// `SPACE_BEFORE_WHILE_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_before_while_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_BEFORE_WHILE_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_before_while_parentheses: Option<bool>,
    /// `SPACE_INSIDE_ONE_LINE_ENUM_BRACES` in `<JavaCodeStyleSettings>`; `ij_java_space_inside_one_line_enum_braces` in `.editorconfig`.
    #[serde(
        rename = "SPACE_INSIDE_ONE_LINE_ENUM_BRACES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_inside_one_line_enum_braces: Option<bool>,
    /// `SPACE_WITHIN_ANNOTATION_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_annotation_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_ANNOTATION_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_annotation_parentheses: Option<bool>,
    /// `SPACE_WITHIN_ARRAY_INITIALIZER_BRACES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_array_initializer_braces` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_ARRAY_INITIALIZER_BRACES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_array_initializer_braces: Option<bool>,
    /// `SPACE_WITHIN_BRACES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_braces` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_BRACES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_braces: Option<bool>,
    /// `SPACE_WITHIN_BRACKETS` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_brackets` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_BRACKETS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_brackets: Option<bool>,
    /// `SPACE_WITHIN_CAST_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_cast_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_CAST_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_cast_parentheses: Option<bool>,
    /// `SPACE_WITHIN_CATCH_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_catch_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_CATCH_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_catch_parentheses: Option<bool>,
    /// `SPACE_WITHIN_DECONSTRUCTION_LIST` in `<JavaCodeStyleSettings>`; `ij_java_spaces_within_deconstruction_list` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_DECONSTRUCTION_LIST",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_deconstruction_list: Option<bool>,
    /// `SPACE_WITHIN_EMPTY_ARRAY_INITIALIZER_BRACES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_within_empty_array_initializer_braces` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_EMPTY_ARRAY_INITIALIZER_BRACES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_empty_array_initializer_braces: Option<bool>,
    /// `SPACE_WITHIN_EMPTY_METHOD_CALL_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_within_empty_method_call_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_EMPTY_METHOD_CALL_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_empty_method_call_parentheses: Option<bool>,
    /// `SPACE_WITHIN_EMPTY_METHOD_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_space_within_empty_method_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_EMPTY_METHOD_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_empty_method_parentheses: Option<bool>,
    /// `SPACE_WITHIN_FOR_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_for_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_FOR_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_for_parentheses: Option<bool>,
    /// `SPACE_WITHIN_IF_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_if_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_IF_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_if_parentheses: Option<bool>,
    /// `SPACE_WITHIN_METHOD_CALL_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_method_call_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_METHOD_CALL_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_method_call_parentheses: Option<bool>,
    /// `SPACE_WITHIN_METHOD_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_method_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_METHOD_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_method_parentheses: Option<bool>,
    /// `SPACE_WITHIN_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_parentheses: Option<bool>,
    /// `SPACE_WITHIN_RECORD_HEADER` in `<JavaCodeStyleSettings>`; `ij_java_spaces_within_record_header` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_RECORD_HEADER",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_record_header: Option<bool>,
    /// `SPACE_WITHIN_SWITCH_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_switch_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_SWITCH_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_switch_parentheses: Option<bool>,
    /// `SPACE_WITHIN_SYNCHRONIZED_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_synchronized_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_SYNCHRONIZED_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_synchronized_parentheses: Option<bool>,
    /// `SPACE_WITHIN_TRY_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_try_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_TRY_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_try_parentheses: Option<bool>,
    /// `SPACE_WITHIN_WHILE_PARENTHESES` in `<codeStyleSettings language="JAVA">`; `ij_java_spaces_within_while_parentheses` in `.editorconfig`.
    #[serde(
        rename = "SPACE_WITHIN_WHILE_PARENTHESES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub space_within_while_parentheses: Option<bool>,
}
