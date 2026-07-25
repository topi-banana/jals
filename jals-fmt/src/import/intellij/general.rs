//! IntelliJ — the language-neutral settings, plus the EditorConfig core properties.
//!
//! `GeneralCodeStylePropertyMapper`'s `GENERAL_FIELDS` live on `<code_scheme>` itself and take
//! the bare `ij_` editorconfig domain rather than `ij_java_`. The three trailing entries have no
//! XML representation at all — they are EditorConfig core properties IntelliJ honors from
//! outside the code-style scheme, and they are keyed by their editorconfig name.

use alloc::string::String;

use serde::Deserialize;

use super::super::serde_kv::Kv;

/// The language-neutral scheme settings and EditorConfig core properties.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijGeneral {
    /// `LINE_SEPARATOR` in `<code_scheme>`; `end_of_line` in `.editorconfig`.
    #[serde(rename = "LINE_SEPARATOR")]
    pub line_separator: Option<String>,
    /// `FORMATTER_TAGS_ENABLED` in `<code_scheme>`; `ij_formatter_tags_enabled` in `.editorconfig`.
    #[serde(rename = "FORMATTER_TAGS_ENABLED", deserialize_with = "Kv::opt_bool")]
    pub formatter_tags_enabled: Option<bool>,
    /// `FORMATTER_OFF_TAG` in `<code_scheme>`; `ij_formatter_off_tag` in `.editorconfig`.
    #[serde(rename = "FORMATTER_OFF_TAG")]
    pub formatter_off_tag: Option<String>,
    /// `FORMATTER_ON_TAG` in `<code_scheme>`; `ij_formatter_on_tag` in `.editorconfig`.
    #[serde(rename = "FORMATTER_ON_TAG")]
    pub formatter_on_tag: Option<String>,
    /// `FORMATTER_TAGS_ACCEPT_REGEXP` in `<code_scheme>`; `ij_formatter_tags_accept_regexp` in `.editorconfig`.
    #[serde(
        rename = "FORMATTER_TAGS_ACCEPT_REGEXP",
        deserialize_with = "Kv::opt_bool"
    )]
    pub formatter_tags_accept_regexp: Option<bool>,
    /// `WRAP_WHEN_TYPING_REACHES_RIGHT_MARGIN` in `<code_scheme>`; `ij_wrap_on_typing` in `.editorconfig`.
    #[serde(
        rename = "WRAP_WHEN_TYPING_REACHES_RIGHT_MARGIN",
        deserialize_with = "Kv::opt_bool"
    )]
    pub wrap_when_typing_reaches_right_margin: Option<bool>,
    /// `insert_final_newline` in EditorConfig core; `insert_final_newline` in `.editorconfig`.
    #[serde(rename = "insert_final_newline", deserialize_with = "Kv::opt_bool")]
    pub insert_final_newline: Option<bool>,
    /// `trim_trailing_whitespace` in EditorConfig core; `trim_trailing_whitespace` in `.editorconfig`.
    #[serde(rename = "trim_trailing_whitespace", deserialize_with = "Kv::opt_bool")]
    pub trim_trailing_whitespace: Option<bool>,
    /// `charset` in EditorConfig core; `charset` in `.editorconfig`.
    #[serde(rename = "charset")]
    pub charset: Option<String>,
}
