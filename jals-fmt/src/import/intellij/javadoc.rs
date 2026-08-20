//! IntelliJ — the Javadoc settings of `JavaCodeStyleSettings`.
//!
//! Their fields are named `JD_*` but every one carries an `@Property(externalName = "doc_*")`,
//! so the XML name and the editorconfig key differ by more than case here.

use crate::import::serde_kv;
use serde::Deserialize;

/// IDEA's own default for `ENABLE_JAVADOC_FORMATTING`: the Javadoc pass is on in a stock IDE.
///
/// Recorded here rather than at the lowering, because what a scheme *omits* is a fact about the
/// product and belongs beside the field that models it.
pub(crate) const ENABLE_JAVADOC_FORMATTING_DEFAULT: bool = true;

/// The Javadoc settings of a Java code style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijJavadoc {
    /// `CLASS_NAMES_IN_JAVADOC` in `<JavaCodeStyleSettings>`; `ij_java_class_names_in_javadoc` in `.editorconfig`.
    #[serde(
        rename = "CLASS_NAMES_IN_JAVADOC",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub class_names_in_javadoc: Option<i64>,
    /// `ENABLE_JAVADOC_FORMATTING` in `<JavaCodeStyleSettings>`; `ij_java_doc_enable_formatting` in `.editorconfig`.
    #[serde(
        rename = "ENABLE_JAVADOC_FORMATTING",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub enable_javadoc_formatting: Option<bool>,
    /// `JD_ADD_BLANK_AFTER_DESCRIPTION` in `<JavaCodeStyleSettings>`; `ij_java_doc_add_blank_line_after_description` in `.editorconfig`.
    #[serde(
        rename = "JD_ADD_BLANK_AFTER_DESCRIPTION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_add_blank_after_description: Option<bool>,
    /// `JD_ADD_BLANK_AFTER_PARM_COMMENTS` in `<JavaCodeStyleSettings>`; `ij_java_doc_add_blank_line_after_param_comments` in `.editorconfig`.
    #[serde(
        rename = "JD_ADD_BLANK_AFTER_PARM_COMMENTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_add_blank_after_parm_comments: Option<bool>,
    /// `JD_ADD_BLANK_AFTER_RETURN` in `<JavaCodeStyleSettings>`; `ij_java_doc_add_blank_line_after_return` in `.editorconfig`.
    #[serde(
        rename = "JD_ADD_BLANK_AFTER_RETURN",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_add_blank_after_return: Option<bool>,
    /// `JD_ALIGN_EXCEPTION_COMMENTS` in `<JavaCodeStyleSettings>`; `ij_java_doc_align_exception_comments` in `.editorconfig`.
    #[serde(
        rename = "JD_ALIGN_EXCEPTION_COMMENTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_align_exception_comments: Option<bool>,
    /// `JD_ALIGN_PARAM_COMMENTS` in `<JavaCodeStyleSettings>`; `ij_java_doc_align_param_comments` in `.editorconfig`.
    #[serde(
        rename = "JD_ALIGN_PARAM_COMMENTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_align_param_comments: Option<bool>,
    /// `JD_DO_NOT_WRAP_ONE_LINE_COMMENTS` in `<JavaCodeStyleSettings>`; `ij_java_doc_do_not_wrap_if_one_line` in `.editorconfig`.
    #[serde(
        rename = "JD_DO_NOT_WRAP_ONE_LINE_COMMENTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_do_not_wrap_one_line_comments: Option<bool>,
    /// `JD_INDENT_ON_CONTINUATION` in `<JavaCodeStyleSettings>`; `ij_java_doc_indent_on_continuation` in `.editorconfig`.
    #[serde(
        rename = "JD_INDENT_ON_CONTINUATION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_indent_on_continuation: Option<bool>,
    /// `JD_KEEP_EMPTY_EXCEPTION` in `<JavaCodeStyleSettings>`; `ij_java_doc_keep_empty_throws_tag` in `.editorconfig`.
    #[serde(
        rename = "JD_KEEP_EMPTY_EXCEPTION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_keep_empty_exception: Option<bool>,
    /// `JD_KEEP_EMPTY_LINES` in `<JavaCodeStyleSettings>`; `ij_java_doc_keep_empty_lines` in `.editorconfig`.
    #[serde(
        rename = "JD_KEEP_EMPTY_LINES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_keep_empty_lines: Option<bool>,
    /// `JD_KEEP_EMPTY_PARAMETER` in `<JavaCodeStyleSettings>`; `ij_java_doc_keep_empty_parameter_tag` in `.editorconfig`.
    #[serde(
        rename = "JD_KEEP_EMPTY_PARAMETER",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_keep_empty_parameter: Option<bool>,
    /// `JD_KEEP_EMPTY_RETURN` in `<JavaCodeStyleSettings>`; `ij_java_doc_keep_empty_return_tag` in `.editorconfig`.
    #[serde(
        rename = "JD_KEEP_EMPTY_RETURN",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_keep_empty_return: Option<bool>,
    /// `JD_KEEP_INVALID_TAGS` in `<JavaCodeStyleSettings>`; `ij_java_doc_keep_invalid_tags` in `.editorconfig`.
    #[serde(
        rename = "JD_KEEP_INVALID_TAGS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_keep_invalid_tags: Option<bool>,
    /// `JD_LEADING_ASTERISKS_ARE_ENABLED` in `<JavaCodeStyleSettings>`; `ij_java_doc_enable_leading_asterisks` in `.editorconfig`.
    #[serde(
        rename = "JD_LEADING_ASTERISKS_ARE_ENABLED",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_leading_asterisks_are_enabled: Option<bool>,
    /// `JD_PARAM_DESCRIPTION_ON_NEW_LINE` in `<JavaCodeStyleSettings>`; `ij_java_doc_param_description_on_new_line` in `.editorconfig`.
    #[serde(
        rename = "JD_PARAM_DESCRIPTION_ON_NEW_LINE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_param_description_on_new_line: Option<bool>,
    /// `JD_PRESERVE_LINE_FEEDS` in `<JavaCodeStyleSettings>`; `ij_java_doc_preserve_line_breaks` in `.editorconfig`.
    #[serde(
        rename = "JD_PRESERVE_LINE_FEEDS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_preserve_line_feeds: Option<bool>,
    /// `JD_P_AT_EMPTY_LINES` in `<JavaCodeStyleSettings>`; `ij_java_doc_add_p_tag_on_empty_lines` in `.editorconfig`.
    #[serde(
        rename = "JD_P_AT_EMPTY_LINES",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_p_at_empty_lines: Option<bool>,
    /// `JD_USE_THROWS_NOT_EXCEPTION` in `<JavaCodeStyleSettings>`; `ij_java_doc_use_throws_not_exception_tag` in `.editorconfig`.
    #[serde(
        rename = "JD_USE_THROWS_NOT_EXCEPTION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub jd_use_throws_not_exception: Option<bool>,
}
