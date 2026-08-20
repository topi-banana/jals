//! IntelliJ — the `BLANK_LINES_*` (enforced) and `KEEP_BLANK_LINES_*` (preserved) counts.

use crate::import::serde_kv;
use serde::Deserialize;

/// The blank-line settings of a Java code style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijBlankLines {
    /// `BLANK_LINES_AFTER_ANONYMOUS_CLASS_HEADER` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_after_anonymous_class_header` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AFTER_ANONYMOUS_CLASS_HEADER",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_after_anonymous_class_header: Option<i64>,
    /// `BLANK_LINES_AFTER_CLASS_HEADER` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_after_class_header` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AFTER_CLASS_HEADER",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_after_class_header: Option<i64>,
    /// `BLANK_LINES_AFTER_IMPORTS` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_after_imports` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AFTER_IMPORTS",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_after_imports: Option<i64>,
    /// `BLANK_LINES_AFTER_PACKAGE` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_after_package` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AFTER_PACKAGE",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_after_package: Option<i64>,
    /// `BLANK_LINES_AROUND_CLASS` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_around_class` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AROUND_CLASS",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_around_class: Option<i64>,
    /// `BLANK_LINES_AROUND_FIELD` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_around_field` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AROUND_FIELD",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_around_field: Option<i64>,
    /// `BLANK_LINES_AROUND_FIELD_IN_INTERFACE` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_around_field_in_interface` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AROUND_FIELD_IN_INTERFACE",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_around_field_in_interface: Option<i64>,
    /// `BLANK_LINES_AROUND_FIELD_WITH_ANNOTATIONS` in `<JavaCodeStyleSettings>`; `ij_java_blank_lines_around_field_with_annotations` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AROUND_FIELD_WITH_ANNOTATIONS",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_around_field_with_annotations: Option<i64>,
    /// `BLANK_LINES_AROUND_INITIALIZER` in `<JavaCodeStyleSettings>`; `ij_java_blank_lines_around_initializer` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AROUND_INITIALIZER",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_around_initializer: Option<i64>,
    /// `BLANK_LINES_AROUND_METHOD` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_around_method` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AROUND_METHOD",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_around_method: Option<i64>,
    /// `BLANK_LINES_AROUND_METHOD_IN_INTERFACE` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_around_method_in_interface` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_AROUND_METHOD_IN_INTERFACE",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_around_method_in_interface: Option<i64>,
    /// `BLANK_LINES_BEFORE_CLASS_END` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_before_class_end` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_BEFORE_CLASS_END",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_class_end: Option<i64>,
    /// `BLANK_LINES_BEFORE_IMPORTS` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_before_imports` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_BEFORE_IMPORTS",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_imports: Option<i64>,
    /// `BLANK_LINES_BEFORE_METHOD_BODY` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_before_method_body` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_BEFORE_METHOD_BODY",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_method_body: Option<i64>,
    /// `BLANK_LINES_BEFORE_PACKAGE` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_before_package` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_BEFORE_PACKAGE",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_before_package: Option<i64>,
    /// `BLANK_LINES_BETWEEN_CASE_BLOCKS` in `<codeStyleSettings language="JAVA">`; `ij_java_blank_lines_between_case_blocks` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_BETWEEN_CASE_BLOCKS",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_between_case_blocks: Option<i64>,
    /// `BLANK_LINES_BETWEEN_RECORD_COMPONENTS` in `<JavaCodeStyleSettings>`; `ij_java_blank_lines_between_record_components` in `.editorconfig`.
    #[serde(
        rename = "BLANK_LINES_BETWEEN_RECORD_COMPONENTS",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub blank_lines_between_record_components: Option<i64>,
    /// `KEEP_BLANK_LINES_BEFORE_RBRACE` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_blank_lines_before_right_brace` in `.editorconfig`.
    #[serde(
        rename = "KEEP_BLANK_LINES_BEFORE_RBRACE",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub keep_blank_lines_before_rbrace: Option<i64>,
    /// `KEEP_BLANK_LINES_BETWEEN_PACKAGE_DECLARATION_AND_HEADER` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_blank_lines_between_package_declaration_and_header` in `.editorconfig`.
    #[serde(
        rename = "KEEP_BLANK_LINES_BETWEEN_PACKAGE_DECLARATION_AND_HEADER",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub keep_blank_lines_between_package_declaration_and_header: Option<i64>,
    /// `KEEP_BLANK_LINES_IN_CODE` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_blank_lines_in_code` in `.editorconfig`.
    #[serde(
        rename = "KEEP_BLANK_LINES_IN_CODE",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub keep_blank_lines_in_code: Option<i64>,
    /// `KEEP_BLANK_LINES_IN_DECLARATIONS` in `<codeStyleSettings language="JAVA">`; `ij_java_keep_blank_lines_in_declarations` in `.editorconfig`.
    #[serde(
        rename = "KEEP_BLANK_LINES_IN_DECLARATIONS",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub keep_blank_lines_in_declarations: Option<i64>,
}
