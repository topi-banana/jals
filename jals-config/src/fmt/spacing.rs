//! `[spacing]` — where a single space is emitted between two tokens.
//!
//! Eclipse spells this family as 219 `insert_space_*` settings (context × before/after) and
//! IntelliJ as 45 `SPACE_*` booleans. jals bundles them **by token role** rather than by
//! syntactic context, giving 49 keys that both vendors project onto (`jals-fmt/MAPPING.md` §5.5).
//!
//! The colon is the one place where the context split is kept: Java's five colon contexts —
//! ternary, enhanced `for`, labeled statement, `switch` label, and `assert` message — genuinely
//! disagree across vendors and across styles, so each gets its own before/after pair. This
//! replaces the former `space-before-colon` / `space-after-colon` /
//! `space-around-operator-colon` trio, whose "either one turns the space on" rule matched no
//! vendor at all.

use serde::{Deserialize, Serialize};

/// Inter-token spacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
#[allow(clippy::struct_excessive_bools)]
pub struct Spacing {
    /// Around `=` and the compound assignment operators. IntelliJ
    /// `SPACE_AROUND_ASSIGNMENT_OPERATORS`.
    pub around_assignment_operators: bool,
    /// Around `&&` / `||`. IntelliJ `SPACE_AROUND_LOGICAL_OPERATORS`.
    pub around_logical_operators: bool,
    /// Around `==` / `!=`. IntelliJ `SPACE_AROUND_EQUALITY_OPERATORS`.
    pub around_equality_operators: bool,
    /// Around `<` / `>` / `<=` / `>=` / `instanceof`. IntelliJ
    /// `SPACE_AROUND_RELATIONAL_OPERATORS`.
    pub around_relational_operators: bool,
    /// Around `&` / `|` / `^`. IntelliJ `SPACE_AROUND_BITWISE_OPERATORS`.
    pub around_bitwise_operators: bool,
    /// Around binary `+` / `-`. IntelliJ `SPACE_AROUND_ADDITIVE_OPERATORS`.
    pub around_additive_operators: bool,
    /// Around `*` / `/` / `%`. IntelliJ `SPACE_AROUND_MULTIPLICATIVE_OPERATORS`.
    pub around_multiplicative_operators: bool,
    /// Around `<<` / `>>` / `>>>`. IntelliJ `SPACE_AROUND_SHIFT_OPERATORS`.
    pub around_shift_operators: bool,
    /// After a prefix unary operator (`!x`, `-x`, `++x`). IntelliJ
    /// `SPACE_AROUND_UNARY_OPERATOR`.
    pub around_unary_operator: bool,
    /// Around a lambda's `->`. IntelliJ `SPACE_AROUND_LAMBDA_ARROW`.
    pub around_lambda_arrow: bool,
    /// Around a method reference's `::`. IntelliJ `SPACE_AROUND_METHOD_REF_DBL_COLON`.
    pub around_method_ref_double_colon: bool,
    /// Around the `&` of a type bound / cast intersection (`<T extends A & B>`). IntelliJ
    /// `SPACES_AROUND_TYPE_BOUNDS_IN_TYPE_PARAMETERS`.
    pub around_type_bounds: bool,
    /// Around the `=` of an annotation argument. IntelliJ `SPACES_AROUND_ANNOTATION_EQ`.
    pub around_annotation_eq: bool,
    /// Before a list separator comma. IntelliJ `SPACE_BEFORE_COMMA`.
    pub before_comma: bool,
    /// After a list separator comma. IntelliJ `SPACE_AFTER_COMMA`.
    pub after_comma: bool,
    /// Before a basic-`for` header semicolon. IntelliJ `SPACE_BEFORE_SEMICOLON`.
    pub before_semicolon: bool,
    /// After a basic-`for` header semicolon. IntelliJ `SPACE_AFTER_SEMICOLON`.
    pub after_semicolon: bool,
    /// Between a called method's name and its `(`. IntelliJ
    /// `SPACE_BEFORE_METHOD_CALL_PARENTHESES`.
    pub before_method_call_parentheses: bool,
    /// Between a declared method's name and its `(`. IntelliJ `SPACE_BEFORE_METHOD_PARENTHESES`.
    pub before_method_parentheses: bool,
    /// Between a control-flow keyword and its `(` — `if` / `while` / `for` / `switch` /
    /// `catch` / `synchronized` / `try`. IntelliJ's seven `SPACE_BEFORE_*_PARENTHESES`.
    pub before_keyword_parentheses: bool,
    /// Between an annotation name and its `(`. IntelliJ `SPACE_BEFORE_ANNOTATION_PARAMETER_LIST`.
    pub before_annotation_parentheses: bool,
    /// Inside a call's argument parentheses. IntelliJ
    /// `SPACES_WITHIN_METHOD_CALL_PARENTHESES`.
    pub within_method_call_parentheses: bool,
    /// Inside a declaration's parameter parentheses. IntelliJ `SPACES_WITHIN_METHOD_PARENTHESES`.
    pub within_method_parentheses: bool,
    /// Inside a control-flow header's parentheses. IntelliJ's `SPACES_WITHIN_IF_PARENTHESES` and
    /// its six siblings.
    pub within_keyword_parentheses: bool,
    /// Inside a cast's parentheses. IntelliJ `SPACES_WITHIN_CAST_PARENTHESES`.
    pub within_cast_parentheses: bool,
    /// Inside an annotation's argument parentheses. IntelliJ
    /// `SPACES_WITHIN_ANNOTATION_PARENTHESES`.
    pub within_annotation_parentheses: bool,
    /// Inside array-index brackets. IntelliJ `SPACES_WITHIN_BRACKETS`.
    pub within_brackets: bool,
    /// Inside an array initializer's braces. IntelliJ
    /// `SPACES_WITHIN_ARRAY_INITIALIZER_BRACES`.
    pub within_array_initializer_braces: bool,
    /// Inside type-argument / type-parameter angle brackets. IntelliJ
    /// `SPACES_WITHIN_ANGLE_BRACKETS`.
    pub within_angle_brackets: bool,
    /// Inside a record header's parentheses. IntelliJ `SPACES_WITHIN_RECORD_HEADER`.
    pub within_record_header: bool,
    /// Between the `(` and `)` of an *empty* parameter or argument list. IntelliJ
    /// `SPACE_WITHIN_EMPTY_METHOD_PARENTHESES` / `SPACE_WITHIN_EMPTY_METHOD_CALL_PARENTHESES`.
    pub within_empty_parentheses: bool,
    /// Between the `{` and `}` of an *empty* array initializer. IntelliJ
    /// `SPACE_WITHIN_EMPTY_ARRAY_INITIALIZER_BRACES`.
    pub within_empty_braces: bool,
    /// Before an opening `{` that follows a header on the same line. IntelliJ's twelve
    /// `SPACE_BEFORE_*_LBRACE` settings.
    pub before_left_brace: bool,
    /// Before an array initializer's `{`. IntelliJ `SPACE_BEFORE_ARRAY_INITIALIZER_LBRACE`.
    pub before_array_initializer_left_brace: bool,
    /// Before a continuation keyword that follows a closing `}` on the same line — `else` /
    /// `while` / `catch` / `finally`. IntelliJ's four `SPACE_BEFORE_*_KEYWORD` settings.
    pub before_continuation_keyword: bool,
    /// After a cast's closing `)`. IntelliJ `SPACE_AFTER_TYPE_CAST`.
    pub after_type_cast: bool,
    /// Before a type-parameter list's `<`. IntelliJ `SPACE_BEFORE_TYPE_PARAMETER_LIST`.
    pub before_type_parameter_list: bool,
    /// Before a ternary's `?`. IntelliJ `SPACE_BEFORE_QUEST`.
    pub before_ternary_question: bool,
    /// After a ternary's `?`. IntelliJ `SPACE_AFTER_QUEST`.
    pub after_ternary_question: bool,
    /// Before a ternary's `:`. Eclipse `insert_space_before_colon_in_conditional` /
    /// IntelliJ `SPACE_BEFORE_COLON`.
    pub before_ternary_colon: bool,
    /// After a ternary's `:`. Eclipse `insert_space_after_colon_in_conditional` /
    /// IntelliJ `SPACE_AFTER_COLON`.
    pub after_ternary_colon: bool,
    /// Before an enhanced `for`'s `:`. Eclipse `insert_space_before_colon_in_for` /
    /// IntelliJ `SPACE_BEFORE_COLON_IN_FOREACH`.
    pub before_foreach_colon: bool,
    /// After an enhanced `for`'s `:`. Eclipse `insert_space_after_colon_in_for`.
    pub after_foreach_colon: bool,
    /// Before a labeled statement's `:`. Eclipse
    /// `insert_space_before_colon_in_labeled_statement`.
    pub before_label_colon: bool,
    /// After a labeled statement's `:`. Eclipse `insert_space_after_colon_in_labeled_statement`.
    pub after_label_colon: bool,
    /// Before a `case` / `default` label's `:`. Eclipse `insert_space_before_colon_in_case`.
    pub before_case_colon: bool,
    /// After a `case` / `default` label's `:`. Eclipse `insert_space_after_colon_in_case`.
    pub after_case_colon: bool,
    /// Before an `assert` message's `:`. Eclipse `insert_space_before_colon_in_assert`.
    pub before_assert_colon: bool,
    /// After an `assert` message's `:`. Eclipse `insert_space_after_colon_in_assert`.
    pub after_assert_colon: bool,
}

impl Default for Spacing {
    fn default() -> Self {
        Self {
            around_assignment_operators: true,
            around_logical_operators: true,
            around_equality_operators: true,
            around_relational_operators: true,
            around_bitwise_operators: true,
            around_additive_operators: true,
            around_multiplicative_operators: true,
            around_shift_operators: true,
            around_unary_operator: false,
            around_lambda_arrow: true,
            around_method_ref_double_colon: false,
            around_type_bounds: true,
            around_annotation_eq: true,
            before_comma: false,
            after_comma: true,
            before_semicolon: false,
            after_semicolon: true,
            before_method_call_parentheses: false,
            before_method_parentheses: false,
            before_keyword_parentheses: true,
            before_annotation_parentheses: false,
            within_method_call_parentheses: false,
            within_method_parentheses: false,
            within_keyword_parentheses: false,
            within_cast_parentheses: false,
            within_annotation_parentheses: false,
            within_brackets: false,
            within_array_initializer_braces: false,
            within_angle_brackets: false,
            within_record_header: false,
            within_empty_parentheses: false,
            within_empty_braces: false,
            before_left_brace: true,
            before_array_initializer_left_brace: false,
            before_continuation_keyword: true,
            after_type_cast: true,
            before_type_parameter_list: false,
            before_ternary_question: true,
            after_ternary_question: true,
            before_ternary_colon: true,
            after_ternary_colon: true,
            before_foreach_colon: true,
            after_foreach_colon: true,
            before_label_colon: false,
            after_label_colon: true,
            before_case_colon: false,
            after_case_colon: true,
            before_assert_colon: true,
            after_assert_colon: true,
        }
    }
}
