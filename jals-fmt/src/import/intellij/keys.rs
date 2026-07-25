//! The `.editorconfig` key → code-style setting name table.
//!
//! IntelliJ writes the same setting under two spellings: an `UPPER_SNAKE` XML option name and
//! a lowercase `.editorconfig` key derived from it (`PropertyNameUtil`, plus per-field
//! `@Property(externalName = ...)` overrides, plus four universal EditorConfig names and two
//! `ij_`-domain ones). The models are keyed by the XML name because it is total — eight
//! `<indentOptions>` settings have no editorconfig key at all — so the editorconfig reader
//! translates through this table.
//!
//! Generated from `inventory.tsv`; the coverage test checks the two stay in step.

/// `(editorconfig key, setting name)`, sorted by key for binary search.
pub(crate) const EDITORCONFIG_KEYS: &[(&str, &str)] = &[
    ("charset", "charset"),
    ("end_of_line", "LINE_SEPARATOR"),
    ("ij_continuation_indent_size", "CONTINUATION_INDENT_SIZE"),
    ("ij_formatter_off_tag", "FORMATTER_OFF_TAG"),
    ("ij_formatter_on_tag", "FORMATTER_ON_TAG"),
    (
        "ij_formatter_tags_accept_regexp",
        "FORMATTER_TAGS_ACCEPT_REGEXP",
    ),
    ("ij_formatter_tags_enabled", "FORMATTER_TAGS_ENABLED"),
    (
        "ij_java_align_consecutive_assignments",
        "ALIGN_CONSECUTIVE_ASSIGNMENTS",
    ),
    (
        "ij_java_align_consecutive_variable_declarations",
        "ALIGN_CONSECUTIVE_VARIABLE_DECLARATIONS",
    ),
    (
        "ij_java_align_group_field_declarations",
        "ALIGN_GROUP_FIELD_DECLARATIONS",
    ),
    (
        "ij_java_align_multiline_annotation_parameters",
        "ALIGN_MULTILINE_ANNOTATION_PARAMETERS",
    ),
    (
        "ij_java_align_multiline_array_initializer_expression",
        "ALIGN_MULTILINE_ARRAY_INITIALIZER_EXPRESSION",
    ),
    (
        "ij_java_align_multiline_assignment",
        "ALIGN_MULTILINE_ASSIGNMENT",
    ),
    (
        "ij_java_align_multiline_binary_operation",
        "ALIGN_MULTILINE_BINARY_OPERATION",
    ),
    (
        "ij_java_align_multiline_chained_methods",
        "ALIGN_MULTILINE_CHAINED_METHODS",
    ),
    (
        "ij_java_align_multiline_deconstruction_list_components",
        "ALIGN_MULTILINE_DECONSTRUCTION_LIST_COMPONENTS",
    ),
    (
        "ij_java_align_multiline_extends_list",
        "ALIGN_MULTILINE_EXTENDS_LIST",
    ),
    ("ij_java_align_multiline_for", "ALIGN_MULTILINE_FOR"),
    (
        "ij_java_align_multiline_method_parentheses",
        "ALIGN_MULTILINE_METHOD_BRACKETS",
    ),
    (
        "ij_java_align_multiline_parameters",
        "ALIGN_MULTILINE_PARAMETERS",
    ),
    (
        "ij_java_align_multiline_parameters_in_calls",
        "ALIGN_MULTILINE_PARAMETERS_IN_CALLS",
    ),
    (
        "ij_java_align_multiline_parenthesized_expression",
        "ALIGN_MULTILINE_PARENTHESIZED_EXPRESSION",
    ),
    ("ij_java_align_multiline_records", "ALIGN_MULTILINE_RECORDS"),
    (
        "ij_java_align_multiline_resources",
        "ALIGN_MULTILINE_RESOURCES",
    ),
    (
        "ij_java_align_multiline_ternary_operation",
        "ALIGN_MULTILINE_TERNARY_OPERATION",
    ),
    (
        "ij_java_align_multiline_text_blocks",
        "ALIGN_MULTILINE_TEXT_BLOCKS",
    ),
    (
        "ij_java_align_multiline_throws_list",
        "ALIGN_MULTILINE_THROWS_LIST",
    ),
    (
        "ij_java_align_subsequent_simple_methods",
        "ALIGN_SUBSEQUENT_SIMPLE_METHODS",
    ),
    ("ij_java_align_throws_keyword", "ALIGN_THROWS_KEYWORD"),
    (
        "ij_java_align_types_in_multi_catch",
        "ALIGN_TYPES_IN_MULTI_CATCH",
    ),
    (
        "ij_java_annotation_new_line_in_record_component",
        "ANNOTATION_NEW_LINE_IN_RECORD_COMPONENT",
    ),
    (
        "ij_java_annotation_parameter_wrap",
        "ANNOTATION_PARAMETER_WRAP",
    ),
    (
        "ij_java_array_initializer_new_line_after_left_brace",
        "ARRAY_INITIALIZER_LBRACE_ON_NEXT_LINE",
    ),
    (
        "ij_java_array_initializer_right_brace_on_new_line",
        "ARRAY_INITIALIZER_RBRACE_ON_NEXT_LINE",
    ),
    ("ij_java_array_initializer_wrap", "ARRAY_INITIALIZER_WRAP"),
    (
        "ij_java_assert_statement_colon_on_next_line",
        "ASSERT_STATEMENT_COLON_ON_NEXT_LINE",
    ),
    ("ij_java_assert_statement_wrap", "ASSERT_STATEMENT_WRAP"),
    ("ij_java_assignment_wrap", "ASSIGNMENT_WRAP"),
    (
        "ij_java_binary_operation_sign_on_next_line",
        "BINARY_OPERATION_SIGN_ON_NEXT_LINE",
    ),
    ("ij_java_binary_operation_wrap", "BINARY_OPERATION_WRAP"),
    (
        "ij_java_blank_lines_after_anonymous_class_header",
        "BLANK_LINES_AFTER_ANONYMOUS_CLASS_HEADER",
    ),
    (
        "ij_java_blank_lines_after_class_header",
        "BLANK_LINES_AFTER_CLASS_HEADER",
    ),
    (
        "ij_java_blank_lines_after_imports",
        "BLANK_LINES_AFTER_IMPORTS",
    ),
    (
        "ij_java_blank_lines_after_package",
        "BLANK_LINES_AFTER_PACKAGE",
    ),
    (
        "ij_java_blank_lines_around_class",
        "BLANK_LINES_AROUND_CLASS",
    ),
    (
        "ij_java_blank_lines_around_field",
        "BLANK_LINES_AROUND_FIELD",
    ),
    (
        "ij_java_blank_lines_around_field_in_interface",
        "BLANK_LINES_AROUND_FIELD_IN_INTERFACE",
    ),
    (
        "ij_java_blank_lines_around_field_with_annotations",
        "BLANK_LINES_AROUND_FIELD_WITH_ANNOTATIONS",
    ),
    (
        "ij_java_blank_lines_around_initializer",
        "BLANK_LINES_AROUND_INITIALIZER",
    ),
    (
        "ij_java_blank_lines_around_method",
        "BLANK_LINES_AROUND_METHOD",
    ),
    (
        "ij_java_blank_lines_around_method_in_interface",
        "BLANK_LINES_AROUND_METHOD_IN_INTERFACE",
    ),
    (
        "ij_java_blank_lines_before_class_end",
        "BLANK_LINES_BEFORE_CLASS_END",
    ),
    (
        "ij_java_blank_lines_before_imports",
        "BLANK_LINES_BEFORE_IMPORTS",
    ),
    (
        "ij_java_blank_lines_before_method_body",
        "BLANK_LINES_BEFORE_METHOD_BODY",
    ),
    (
        "ij_java_blank_lines_before_package",
        "BLANK_LINES_BEFORE_PACKAGE",
    ),
    (
        "ij_java_blank_lines_between_case_blocks",
        "BLANK_LINES_BETWEEN_CASE_BLOCKS",
    ),
    (
        "ij_java_blank_lines_between_record_components",
        "BLANK_LINES_BETWEEN_RECORD_COMPONENTS",
    ),
    ("ij_java_block_brace_style", "BRACE_STYLE"),
    ("ij_java_block_comment_add_space", "BLOCK_COMMENT_ADD_SPACE"),
    (
        "ij_java_block_comment_at_first_column",
        "BLOCK_COMMENT_AT_FIRST_COLUMN",
    ),
    (
        "ij_java_call_parameters_new_line_after_left_paren",
        "CALL_PARAMETERS_LPAREN_ON_NEXT_LINE",
    ),
    (
        "ij_java_call_parameters_right_paren_on_new_line",
        "CALL_PARAMETERS_RPAREN_ON_NEXT_LINE",
    ),
    ("ij_java_call_parameters_wrap", "CALL_PARAMETERS_WRAP"),
    (
        "ij_java_case_statement_on_separate_line",
        "CASE_STATEMENT_ON_NEW_LINE",
    ),
    ("ij_java_catch_on_new_line", "CATCH_ON_NEW_LINE"),
    ("ij_java_class_annotation_wrap", "CLASS_ANNOTATION_WRAP"),
    ("ij_java_class_brace_style", "CLASS_BRACE_STYLE"),
    (
        "ij_java_class_count_to_use_import_on_demand",
        "CLASS_COUNT_TO_USE_IMPORT_ON_DEMAND",
    ),
    ("ij_java_class_names_in_javadoc", "CLASS_NAMES_IN_JAVADOC"),
    (
        "ij_java_deconstruction_list_wrap",
        "DECONSTRUCTION_LIST_WRAP",
    ),
    (
        "ij_java_delete_unused_module_imports",
        "DELETE_UNUSED_MODULE_IMPORTS",
    ),
    (
        "ij_java_do_not_indent_top_level_class_members",
        "DO_NOT_INDENT_TOP_LEVEL_CLASS_MEMBERS",
    ),
    (
        "ij_java_do_not_wrap_after_single_annotation",
        "DO_NOT_WRAP_AFTER_SINGLE_ANNOTATION",
    ),
    (
        "ij_java_do_not_wrap_after_single_annotation_in_parameter",
        "DO_NOT_WRAP_AFTER_SINGLE_ANNOTATION_IN_PARAMETER",
    ),
    ("ij_java_do_while_brace_force", "DOWHILE_BRACE_FORCE"),
    (
        "ij_java_doc_add_blank_line_after_description",
        "JD_ADD_BLANK_AFTER_DESCRIPTION",
    ),
    (
        "ij_java_doc_add_blank_line_after_param_comments",
        "JD_ADD_BLANK_AFTER_PARM_COMMENTS",
    ),
    (
        "ij_java_doc_add_blank_line_after_return",
        "JD_ADD_BLANK_AFTER_RETURN",
    ),
    (
        "ij_java_doc_add_p_tag_on_empty_lines",
        "JD_P_AT_EMPTY_LINES",
    ),
    (
        "ij_java_doc_align_exception_comments",
        "JD_ALIGN_EXCEPTION_COMMENTS",
    ),
    (
        "ij_java_doc_align_param_comments",
        "JD_ALIGN_PARAM_COMMENTS",
    ),
    (
        "ij_java_doc_do_not_wrap_if_one_line",
        "JD_DO_NOT_WRAP_ONE_LINE_COMMENTS",
    ),
    ("ij_java_doc_enable_formatting", "ENABLE_JAVADOC_FORMATTING"),
    (
        "ij_java_doc_enable_leading_asterisks",
        "JD_LEADING_ASTERISKS_ARE_ENABLED",
    ),
    (
        "ij_java_doc_indent_on_continuation",
        "JD_INDENT_ON_CONTINUATION",
    ),
    ("ij_java_doc_keep_empty_lines", "JD_KEEP_EMPTY_LINES"),
    (
        "ij_java_doc_keep_empty_parameter_tag",
        "JD_KEEP_EMPTY_PARAMETER",
    ),
    ("ij_java_doc_keep_empty_return_tag", "JD_KEEP_EMPTY_RETURN"),
    (
        "ij_java_doc_keep_empty_throws_tag",
        "JD_KEEP_EMPTY_EXCEPTION",
    ),
    ("ij_java_doc_keep_invalid_tags", "JD_KEEP_INVALID_TAGS"),
    (
        "ij_java_doc_param_description_on_new_line",
        "JD_PARAM_DESCRIPTION_ON_NEW_LINE",
    ),
    ("ij_java_doc_preserve_line_breaks", "JD_PRESERVE_LINE_FEEDS"),
    (
        "ij_java_doc_use_throws_not_exception_tag",
        "JD_USE_THROWS_NOT_EXCEPTION",
    ),
    (
        "ij_java_documentation_line_comment_preferred",
        "DOCUMENTATION_LINE_COMMENT_PREFERRED",
    ),
    ("ij_java_else_on_new_line", "ELSE_ON_NEW_LINE"),
    ("ij_java_enum_constants_wrap", "ENUM_CONSTANTS_WRAP"),
    (
        "ij_java_enum_field_annotation_wrap",
        "ENUM_FIELD_ANNOTATION_WRAP",
    ),
    ("ij_java_extends_keyword_wrap", "EXTENDS_KEYWORD_WRAP"),
    ("ij_java_extends_list_wrap", "EXTENDS_LIST_WRAP"),
    ("ij_java_field_annotation_wrap", "FIELD_ANNOTATION_WRAP"),
    ("ij_java_field_name_prefix", "FIELD_NAME_PREFIX"),
    ("ij_java_field_name_suffix", "FIELD_NAME_SUFFIX"),
    ("ij_java_finally_on_new_line", "FINALLY_ON_NEW_LINE"),
    ("ij_java_for_brace_force", "FOR_BRACE_FORCE"),
    (
        "ij_java_for_statement_new_line_after_left_paren",
        "FOR_STATEMENT_LPAREN_ON_NEXT_LINE",
    ),
    (
        "ij_java_for_statement_right_paren_on_new_line",
        "FOR_STATEMENT_RPAREN_ON_NEXT_LINE",
    ),
    ("ij_java_for_statement_wrap", "FOR_STATEMENT_WRAP"),
    ("ij_java_force_rearrange_mode", "FORCE_REARRANGE_MODE"),
    ("ij_java_generate_final_locals", "GENERATE_FINAL_LOCALS"),
    (
        "ij_java_generate_final_parameters",
        "GENERATE_FINAL_PARAMETERS",
    ),
    (
        "ij_java_generate_use_type_annotation_before_type",
        "GENERATE_USE_TYPE_ANNOTATION_BEFORE_TYPE",
    ),
    ("ij_java_if_brace_force", "IF_BRACE_FORCE"),
    ("ij_java_imports_layout", "IMPORT_LAYOUT_TABLE"),
    ("ij_java_indent_break_from_case", "INDENT_BREAK_FROM_CASE"),
    ("ij_java_indent_case_from_switch", "INDENT_CASE_FROM_SWITCH"),
    (
        "ij_java_insert_inner_class_imports",
        "INSERT_INNER_CLASS_IMPORTS",
    ),
    (
        "ij_java_insert_override_annotation",
        "INSERT_OVERRIDE_ANNOTATION",
    ),
    (
        "ij_java_keep_blank_lines_before_right_brace",
        "KEEP_BLANK_LINES_BEFORE_RBRACE",
    ),
    (
        "ij_java_keep_blank_lines_between_package_declaration_and_header",
        "KEEP_BLANK_LINES_BETWEEN_PACKAGE_DECLARATION_AND_HEADER",
    ),
    (
        "ij_java_keep_blank_lines_in_code",
        "KEEP_BLANK_LINES_IN_CODE",
    ),
    (
        "ij_java_keep_blank_lines_in_declarations",
        "KEEP_BLANK_LINES_IN_DECLARATIONS",
    ),
    (
        "ij_java_keep_builder_methods_indents",
        "KEEP_BUILDER_METHODS_INDENTS",
    ),
    (
        "ij_java_keep_control_statement_in_one_line",
        "KEEP_CONTROL_STATEMENT_IN_ONE_LINE",
    ),
    (
        "ij_java_keep_first_column_comment",
        "KEEP_FIRST_COLUMN_COMMENT",
    ),
    (
        "ij_java_keep_indents_on_empty_lines",
        "KEEP_INDENTS_ON_EMPTY_LINES",
    ),
    ("ij_java_keep_line_breaks", "KEEP_LINE_BREAKS"),
    (
        "ij_java_keep_multiple_expressions_in_one_line",
        "KEEP_MULTIPLE_EXPRESSIONS_IN_ONE_LINE",
    ),
    (
        "ij_java_keep_simple_blocks_in_one_line",
        "KEEP_SIMPLE_BLOCKS_IN_ONE_LINE",
    ),
    (
        "ij_java_keep_simple_classes_in_one_line",
        "KEEP_SIMPLE_CLASSES_IN_ONE_LINE",
    ),
    (
        "ij_java_keep_simple_lambdas_in_one_line",
        "KEEP_SIMPLE_LAMBDAS_IN_ONE_LINE",
    ),
    (
        "ij_java_keep_simple_methods_in_one_line",
        "KEEP_SIMPLE_METHODS_IN_ONE_LINE",
    ),
    ("ij_java_lambda_brace_style", "LAMBDA_BRACE_STYLE"),
    (
        "ij_java_layout_on_demand_import_from_same_package_first",
        "LAYOUT_ON_DEMAND_IMPORT_FROM_SAME_PACKAGE_FIRST",
    ),
    (
        "ij_java_layout_static_imports_separately",
        "LAYOUT_STATIC_IMPORTS_SEPARATELY",
    ),
    ("ij_java_line_comment_add_space", "LINE_COMMENT_ADD_SPACE"),
    (
        "ij_java_line_comment_add_space_in_suppression",
        "LINE_COMMENT_ADD_SPACE_IN_SUPPRESSION",
    ),
    (
        "ij_java_line_comment_add_space_on_reformat",
        "LINE_COMMENT_ADD_SPACE_ON_REFORMAT",
    ),
    (
        "ij_java_line_comment_at_first_column",
        "LINE_COMMENT_AT_FIRST_COLUMN",
    ),
    (
        "ij_java_local_variable_name_prefix",
        "LOCAL_VARIABLE_NAME_PREFIX",
    ),
    (
        "ij_java_local_variable_name_suffix",
        "LOCAL_VARIABLE_NAME_SUFFIX",
    ),
    ("ij_java_method_annotation_wrap", "METHOD_ANNOTATION_WRAP"),
    ("ij_java_method_brace_style", "METHOD_BRACE_STYLE"),
    ("ij_java_method_call_chain_wrap", "METHOD_CALL_CHAIN_WRAP"),
    (
        "ij_java_method_parameters_new_line_after_left_paren",
        "METHOD_PARAMETERS_LPAREN_ON_NEXT_LINE",
    ),
    (
        "ij_java_method_parameters_right_paren_on_new_line",
        "METHOD_PARAMETERS_RPAREN_ON_NEXT_LINE",
    ),
    ("ij_java_method_parameters_wrap", "METHOD_PARAMETERS_WRAP"),
    ("ij_java_modifier_list_wrap", "MODIFIER_LIST_WRAP"),
    ("ij_java_multi_catch_types_wrap", "MULTI_CATCH_TYPES_WRAP"),
    (
        "ij_java_names_count_to_use_import_on_demand",
        "NAMES_COUNT_TO_USE_IMPORT_ON_DEMAND",
    ),
    (
        "ij_java_new_line_after_lparen_in_annotation",
        "NEW_LINE_AFTER_LPAREN_IN_ANNOTATION",
    ),
    (
        "ij_java_new_line_after_lparen_in_deconstruction_pattern",
        "NEW_LINE_AFTER_LPAREN_IN_DECONSTRUCTION_PATTERN",
    ),
    (
        "ij_java_new_line_after_lparen_in_record_header",
        "NEW_LINE_AFTER_LPAREN_IN_RECORD_HEADER",
    ),
    (
        "ij_java_new_line_when_body_is_presented",
        "NEW_LINE_WHEN_BODY_IS_PRESENTED",
    ),
    (
        "ij_java_packages_to_use_import_on_demand",
        "PACKAGES_TO_USE_IMPORT_ON_DEMAND",
    ),
    (
        "ij_java_parameter_annotation_wrap",
        "PARAMETER_ANNOTATION_WRAP",
    ),
    ("ij_java_parameter_name_prefix", "PARAMETER_NAME_PREFIX"),
    ("ij_java_parameter_name_suffix", "PARAMETER_NAME_SUFFIX"),
    (
        "ij_java_parentheses_expression_new_line_after_left_paren",
        "PARENTHESES_EXPRESSION_LPAREN_WRAP",
    ),
    (
        "ij_java_parentheses_expression_right_paren_on_new_line",
        "PARENTHESES_EXPRESSION_RPAREN_WRAP",
    ),
    (
        "ij_java_place_assignment_sign_on_next_line",
        "PLACE_ASSIGNMENT_SIGN_ON_NEXT_LINE",
    ),
    ("ij_java_prefer_longer_names", "PREFER_LONGER_NAMES"),
    ("ij_java_prefer_parameters_wrap", "PREFER_PARAMETERS_WRAP"),
    ("ij_java_preserve_module_imports", "PRESERVE_MODULE_IMPORTS"),
    ("ij_java_record_components_wrap", "RECORD_COMPONENTS_WRAP"),
    ("ij_java_repeat_annotations", "REPEAT_ANNOTATIONS"),
    ("ij_java_repeat_synchronized", "REPEAT_SYNCHRONIZED"),
    ("ij_java_replace_cast", "REPLACE_CAST"),
    ("ij_java_replace_instanceof", "REPLACE_INSTANCEOF"),
    (
        "ij_java_replace_instanceof_and_cast",
        "REPLACE_INSTANCEOF_AND_CAST",
    ),
    ("ij_java_replace_null_check", "REPLACE_NULL_CHECK"),
    ("ij_java_replace_sum_lambda_with_method_ref", "REPLACE_SUM"),
    (
        "ij_java_resource_list_new_line_after_left_paren",
        "RESOURCE_LIST_LPAREN_ON_NEXT_LINE",
    ),
    (
        "ij_java_resource_list_right_paren_on_new_line",
        "RESOURCE_LIST_RPAREN_ON_NEXT_LINE",
    ),
    ("ij_java_resource_list_wrap", "RESOURCE_LIST_WRAP"),
    (
        "ij_java_rparen_on_new_line_in_annotation",
        "RPAREN_ON_NEW_LINE_IN_ANNOTATION",
    ),
    (
        "ij_java_rparen_on_new_line_in_deconstruction_pattern",
        "RPAREN_ON_NEW_LINE_IN_DECONSTRUCTION_PATTERN",
    ),
    (
        "ij_java_rparen_on_new_line_in_record_header",
        "RPAREN_ON_NEW_LINE_IN_RECORD_HEADER",
    ),
    (
        "ij_java_space_after_closing_angle_bracket_in_type_argument",
        "SPACE_AFTER_CLOSING_ANGLE_BRACKET_IN_TYPE_ARGUMENT",
    ),
    ("ij_java_space_after_colon", "SPACE_AFTER_COLON"),
    ("ij_java_space_after_comma", "SPACE_AFTER_COMMA"),
    (
        "ij_java_space_after_comma_in_type_arguments",
        "SPACE_AFTER_COMMA_IN_TYPE_ARGUMENTS",
    ),
    ("ij_java_space_after_for_semicolon", "SPACE_AFTER_SEMICOLON"),
    ("ij_java_space_after_quest", "SPACE_AFTER_QUEST"),
    ("ij_java_space_after_type_cast", "SPACE_AFTER_TYPE_CAST"),
    (
        "ij_java_space_before_annotation_array_initializer_left_brace",
        "SPACE_BEFORE_ANNOTATION_ARRAY_INITIALIZER_LBRACE",
    ),
    (
        "ij_java_space_before_annotation_parameter_list",
        "SPACE_BEFORE_ANOTATION_PARAMETER_LIST",
    ),
    (
        "ij_java_space_before_array_initializer_left_brace",
        "SPACE_BEFORE_ARRAY_INITIALIZER_LBRACE",
    ),
    (
        "ij_java_space_before_catch_keyword",
        "SPACE_BEFORE_CATCH_KEYWORD",
    ),
    (
        "ij_java_space_before_catch_left_brace",
        "SPACE_BEFORE_CATCH_LBRACE",
    ),
    (
        "ij_java_space_before_catch_parentheses",
        "SPACE_BEFORE_CATCH_PARENTHESES",
    ),
    (
        "ij_java_space_before_class_left_brace",
        "SPACE_BEFORE_CLASS_LBRACE",
    ),
    ("ij_java_space_before_colon", "SPACE_BEFORE_COLON"),
    (
        "ij_java_space_before_colon_in_foreach",
        "SPACE_BEFORE_COLON_IN_FOREACH",
    ),
    ("ij_java_space_before_comma", "SPACE_BEFORE_COMMA"),
    (
        "ij_java_space_before_deconstruction_list",
        "SPACE_BEFORE_DECONSTRUCTION_LIST",
    ),
    (
        "ij_java_space_before_do_left_brace",
        "SPACE_BEFORE_DO_LBRACE",
    ),
    (
        "ij_java_space_before_else_keyword",
        "SPACE_BEFORE_ELSE_KEYWORD",
    ),
    (
        "ij_java_space_before_else_left_brace",
        "SPACE_BEFORE_ELSE_LBRACE",
    ),
    (
        "ij_java_space_before_finally_keyword",
        "SPACE_BEFORE_FINALLY_KEYWORD",
    ),
    (
        "ij_java_space_before_finally_left_brace",
        "SPACE_BEFORE_FINALLY_LBRACE",
    ),
    (
        "ij_java_space_before_for_left_brace",
        "SPACE_BEFORE_FOR_LBRACE",
    ),
    (
        "ij_java_space_before_for_parentheses",
        "SPACE_BEFORE_FOR_PARENTHESES",
    ),
    (
        "ij_java_space_before_for_semicolon",
        "SPACE_BEFORE_SEMICOLON",
    ),
    (
        "ij_java_space_before_if_left_brace",
        "SPACE_BEFORE_IF_LBRACE",
    ),
    (
        "ij_java_space_before_if_parentheses",
        "SPACE_BEFORE_IF_PARENTHESES",
    ),
    (
        "ij_java_space_before_method_call_parentheses",
        "SPACE_BEFORE_METHOD_CALL_PARENTHESES",
    ),
    (
        "ij_java_space_before_method_left_brace",
        "SPACE_BEFORE_METHOD_LBRACE",
    ),
    (
        "ij_java_space_before_method_parentheses",
        "SPACE_BEFORE_METHOD_PARENTHESES",
    ),
    (
        "ij_java_space_before_opening_angle_bracket_in_type_parameter",
        "SPACE_BEFORE_OPENING_ANGLE_BRACKET_IN_TYPE_PARAMETER",
    ),
    ("ij_java_space_before_quest", "SPACE_BEFORE_QUEST"),
    (
        "ij_java_space_before_switch_left_brace",
        "SPACE_BEFORE_SWITCH_LBRACE",
    ),
    (
        "ij_java_space_before_switch_parentheses",
        "SPACE_BEFORE_SWITCH_PARENTHESES",
    ),
    (
        "ij_java_space_before_synchronized_left_brace",
        "SPACE_BEFORE_SYNCHRONIZED_LBRACE",
    ),
    (
        "ij_java_space_before_synchronized_parentheses",
        "SPACE_BEFORE_SYNCHRONIZED_PARENTHESES",
    ),
    (
        "ij_java_space_before_try_left_brace",
        "SPACE_BEFORE_TRY_LBRACE",
    ),
    (
        "ij_java_space_before_try_parentheses",
        "SPACE_BEFORE_TRY_PARENTHESES",
    ),
    (
        "ij_java_space_before_type_parameter_list",
        "SPACE_BEFORE_TYPE_PARAMETER_LIST",
    ),
    (
        "ij_java_space_before_while_keyword",
        "SPACE_BEFORE_WHILE_KEYWORD",
    ),
    (
        "ij_java_space_before_while_left_brace",
        "SPACE_BEFORE_WHILE_LBRACE",
    ),
    (
        "ij_java_space_before_while_parentheses",
        "SPACE_BEFORE_WHILE_PARENTHESES",
    ),
    (
        "ij_java_space_inside_one_line_enum_braces",
        "SPACE_INSIDE_ONE_LINE_ENUM_BRACES",
    ),
    (
        "ij_java_space_within_empty_array_initializer_braces",
        "SPACE_WITHIN_EMPTY_ARRAY_INITIALIZER_BRACES",
    ),
    (
        "ij_java_space_within_empty_method_call_parentheses",
        "SPACE_WITHIN_EMPTY_METHOD_CALL_PARENTHESES",
    ),
    (
        "ij_java_space_within_empty_method_parentheses",
        "SPACE_WITHIN_EMPTY_METHOD_PARENTHESES",
    ),
    (
        "ij_java_spaces_around_additive_operators",
        "SPACE_AROUND_ADDITIVE_OPERATORS",
    ),
    (
        "ij_java_spaces_around_annotation_eq",
        "SPACE_AROUND_ANNOTATION_EQ",
    ),
    (
        "ij_java_spaces_around_assignment_operators",
        "SPACE_AROUND_ASSIGNMENT_OPERATORS",
    ),
    (
        "ij_java_spaces_around_bitwise_operators",
        "SPACE_AROUND_BITWISE_OPERATORS",
    ),
    (
        "ij_java_spaces_around_equality_operators",
        "SPACE_AROUND_EQUALITY_OPERATORS",
    ),
    (
        "ij_java_spaces_around_lambda_arrow",
        "SPACE_AROUND_LAMBDA_ARROW",
    ),
    (
        "ij_java_spaces_around_logical_operators",
        "SPACE_AROUND_LOGICAL_OPERATORS",
    ),
    (
        "ij_java_spaces_around_method_ref_dbl_colon",
        "SPACE_AROUND_METHOD_REF_DBL_COLON",
    ),
    (
        "ij_java_spaces_around_multiplicative_operators",
        "SPACE_AROUND_MULTIPLICATIVE_OPERATORS",
    ),
    (
        "ij_java_spaces_around_relational_operators",
        "SPACE_AROUND_RELATIONAL_OPERATORS",
    ),
    (
        "ij_java_spaces_around_shift_operators",
        "SPACE_AROUND_SHIFT_OPERATORS",
    ),
    (
        "ij_java_spaces_around_type_bounds_in_type_parameters",
        "SPACE_AROUND_TYPE_BOUNDS_IN_TYPE_PARAMETERS",
    ),
    (
        "ij_java_spaces_around_unary_operator",
        "SPACE_AROUND_UNARY_OPERATOR",
    ),
    (
        "ij_java_spaces_inside_block_braces_when_body_is_present",
        "SPACES_INSIDE_BLOCK_BRACES_WHEN_BODY_IS_PRESENT",
    ),
    (
        "ij_java_spaces_within_angle_brackets",
        "SPACES_WITHIN_ANGLE_BRACKETS",
    ),
    (
        "ij_java_spaces_within_annotation_parentheses",
        "SPACE_WITHIN_ANNOTATION_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_array_initializer_braces",
        "SPACE_WITHIN_ARRAY_INITIALIZER_BRACES",
    ),
    ("ij_java_spaces_within_braces", "SPACE_WITHIN_BRACES"),
    ("ij_java_spaces_within_brackets", "SPACE_WITHIN_BRACKETS"),
    (
        "ij_java_spaces_within_cast_parentheses",
        "SPACE_WITHIN_CAST_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_catch_parentheses",
        "SPACE_WITHIN_CATCH_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_deconstruction_list",
        "SPACE_WITHIN_DECONSTRUCTION_LIST",
    ),
    (
        "ij_java_spaces_within_for_parentheses",
        "SPACE_WITHIN_FOR_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_if_parentheses",
        "SPACE_WITHIN_IF_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_method_call_parentheses",
        "SPACE_WITHIN_METHOD_CALL_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_method_parentheses",
        "SPACE_WITHIN_METHOD_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_parentheses",
        "SPACE_WITHIN_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_record_header",
        "SPACE_WITHIN_RECORD_HEADER",
    ),
    (
        "ij_java_spaces_within_switch_parentheses",
        "SPACE_WITHIN_SWITCH_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_synchronized_parentheses",
        "SPACE_WITHIN_SYNCHRONIZED_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_try_parentheses",
        "SPACE_WITHIN_TRY_PARENTHESES",
    ),
    (
        "ij_java_spaces_within_while_parentheses",
        "SPACE_WITHIN_WHILE_PARENTHESES",
    ),
    (
        "ij_java_special_else_if_treatment",
        "SPECIAL_ELSE_IF_TREATMENT",
    ),
    (
        "ij_java_static_field_name_prefix",
        "STATIC_FIELD_NAME_PREFIX",
    ),
    (
        "ij_java_static_field_name_suffix",
        "STATIC_FIELD_NAME_SUFFIX",
    ),
    (
        "ij_java_strip_whitespace_from_blank_lines_in_text_blocks",
        "STRIP_WHITESPACE_FROM_BLANK_LINES_IN_TEXT_BLOCKS",
    ),
    ("ij_java_subclass_name_prefix", "SUBCLASS_NAME_PREFIX"),
    ("ij_java_subclass_name_suffix", "SUBCLASS_NAME_SUFFIX"),
    ("ij_java_switch_expressions_wrap", "SWITCH_EXPRESSIONS_WRAP"),
    (
        "ij_java_ternary_operation_signs_on_next_line",
        "TERNARY_OPERATION_SIGNS_ON_NEXT_LINE",
    ),
    ("ij_java_ternary_operation_wrap", "TERNARY_OPERATION_WRAP"),
    ("ij_java_test_name_prefix", "TEST_NAME_PREFIX"),
    ("ij_java_test_name_suffix", "TEST_NAME_SUFFIX"),
    ("ij_java_throws_keyword_wrap", "THROWS_KEYWORD_WRAP"),
    ("ij_java_throws_list_wrap", "THROWS_LIST_WRAP"),
    (
        "ij_java_use_external_annotations",
        "USE_EXTERNAL_ANNOTATIONS",
    ),
    ("ij_java_use_fq_class_names", "USE_FQ_CLASS_NAMES"),
    (
        "ij_java_use_single_class_imports",
        "USE_SINGLE_CLASS_IMPORTS",
    ),
    (
        "ij_java_variable_annotation_wrap",
        "VARIABLE_ANNOTATION_WRAP",
    ),
    ("ij_java_visibility", "VISIBILITY"),
    ("ij_java_while_brace_force", "WHILE_BRACE_FORCE"),
    ("ij_java_while_on_new_line", "WHILE_ON_NEW_LINE"),
    ("ij_java_wrap_comments", "WRAP_COMMENTS"),
    (
        "ij_java_wrap_first_method_in_call_chain",
        "WRAP_FIRST_METHOD_IN_CALL_CHAIN",
    ),
    ("ij_java_wrap_long_lines", "WRAP_LONG_LINES"),
    ("ij_java_wrap_on_typing", "WRAP_ON_TYPING"),
    (
        "ij_java_wrap_semicolon_after_call_chain",
        "WRAP_SEMICOLON_AFTER_CALL_CHAIN",
    ),
    ("ij_smart_tabs", "SMART_TABS"),
    ("ij_wrap_on_typing", "WRAP_WHEN_TYPING_REACHES_RIGHT_MARGIN"),
    ("indent_size", "INDENT_SIZE"),
    ("indent_style", "USE_TAB_CHARACTER"),
    ("insert_final_newline", "insert_final_newline"),
    ("max_line_length", "RIGHT_MARGIN"),
    ("tab_width", "TAB_SIZE"),
    ("trim_trailing_whitespace", "trim_trailing_whitespace"),
];
