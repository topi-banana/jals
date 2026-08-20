//! IntelliJ — the `CommonCodeStyleSettings` fields belonging to no larger family:
//! the column limit, the line-comment / block-comment commenter settings, and `WRAP_COMMENTS`.

use crate::import::serde_kv;
use serde::Deserialize;

/// The remaining language-common settings of a Java code style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijCommon {
    /// `BLOCK_COMMENT_ADD_SPACE` in `<codeStyleSettings language="JAVA">`; `ij_java_block_comment_add_space` in `.editorconfig`.
    #[serde(
        rename = "BLOCK_COMMENT_ADD_SPACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub block_comment_add_space: Option<bool>,
    /// `BLOCK_COMMENT_AT_FIRST_COLUMN` in `<codeStyleSettings language="JAVA">`; `ij_java_block_comment_at_first_column` in `.editorconfig`.
    #[serde(
        rename = "BLOCK_COMMENT_AT_FIRST_COLUMN",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub block_comment_at_first_column: Option<bool>,
    /// `DOCUMENTATION_LINE_COMMENT_PREFERRED` in `<codeStyleSettings language="JAVA">`; `ij_java_documentation_line_comment_preferred` in `.editorconfig`.
    #[serde(
        rename = "DOCUMENTATION_LINE_COMMENT_PREFERRED",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub documentation_line_comment_preferred: Option<bool>,
    /// `LINE_COMMENT_ADD_SPACE` in `<codeStyleSettings language="JAVA">`; `ij_java_line_comment_add_space` in `.editorconfig`.
    #[serde(
        rename = "LINE_COMMENT_ADD_SPACE",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub line_comment_add_space: Option<bool>,
    /// `LINE_COMMENT_ADD_SPACE_IN_SUPPRESSION` in `<codeStyleSettings language="JAVA">`; `ij_java_line_comment_add_space_in_suppression` in `.editorconfig`.
    #[serde(
        rename = "LINE_COMMENT_ADD_SPACE_IN_SUPPRESSION",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub line_comment_add_space_in_suppression: Option<bool>,
    /// `LINE_COMMENT_ADD_SPACE_ON_REFORMAT` in `<codeStyleSettings language="JAVA">`; `ij_java_line_comment_add_space_on_reformat` in `.editorconfig`.
    #[serde(
        rename = "LINE_COMMENT_ADD_SPACE_ON_REFORMAT",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub line_comment_add_space_on_reformat: Option<bool>,
    /// `LINE_COMMENT_AT_FIRST_COLUMN` in `<codeStyleSettings language="JAVA">`; `ij_java_line_comment_at_first_column` in `.editorconfig`.
    #[serde(
        rename = "LINE_COMMENT_AT_FIRST_COLUMN",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub line_comment_at_first_column: Option<bool>,
    /// `RIGHT_MARGIN` in `<codeStyleSettings language="JAVA">`; `max_line_length` in `.editorconfig`.
    #[serde(rename = "RIGHT_MARGIN", deserialize_with = "serde_kv::opt_number")]
    pub right_margin: Option<i64>,
    /// `STRIP_WHITESPACE_FROM_BLANK_LINES_IN_TEXT_BLOCKS` in `<JavaCodeStyleSettings>`; `ij_java_strip_whitespace_from_blank_lines_in_text_blocks` in `.editorconfig`.
    #[serde(
        rename = "STRIP_WHITESPACE_FROM_BLANK_LINES_IN_TEXT_BLOCKS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub strip_whitespace_from_blank_lines_in_text_blocks: Option<bool>,
}
