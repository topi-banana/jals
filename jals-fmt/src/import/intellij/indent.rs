//! IntelliJ — `<indentOptions>`: the 14 `CommonCodeStyleSettings.IndentOptions` fields.
//!
//! Java exposes only six of them to `.editorconfig` (the `SmartIndentOptionsEditor` subset);
//! the other eight are reachable through the XML scheme alone, which is why the model is keyed
//! by the XML option name rather than by the editorconfig key.

use serde::Deserialize;

use super::super::serde_kv::Kv;

/// The `<indentOptions>` block of a Java code style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijIndent {
    /// `ARRAY_ELEMENT_INDENT` in `<indentOptions>`; `XML only` in `.editorconfig`.
    #[serde(rename = "ARRAY_ELEMENT_INDENT", deserialize_with = "Kv::opt_number")]
    pub array_element_indent: Option<i64>,
    /// `CALL_PARAMETER_INDENT` in `<indentOptions>`; `XML only` in `.editorconfig`.
    #[serde(rename = "CALL_PARAMETER_INDENT", deserialize_with = "Kv::opt_number")]
    pub call_parameter_indent: Option<i64>,
    /// `CHAINED_CALL_INDENT` in `<indentOptions>`; `XML only` in `.editorconfig`.
    #[serde(rename = "CHAINED_CALL_INDENT", deserialize_with = "Kv::opt_number")]
    pub chained_call_indent: Option<i64>,
    /// `CONTINUATION_INDENT_SIZE` in `<indentOptions>`; `ij_continuation_indent_size` in `.editorconfig`.
    #[serde(
        rename = "CONTINUATION_INDENT_SIZE",
        deserialize_with = "Kv::opt_number"
    )]
    pub continuation_indent_size: Option<i64>,
    /// `DECLARATION_PARAMETER_INDENT` in `<indentOptions>`; `XML only` in `.editorconfig`.
    #[serde(
        rename = "DECLARATION_PARAMETER_INDENT",
        deserialize_with = "Kv::opt_number"
    )]
    pub declaration_parameter_indent: Option<i64>,
    /// `GENERIC_TYPE_PARAMETER_INDENT` in `<indentOptions>`; `XML only` in `.editorconfig`.
    #[serde(
        rename = "GENERIC_TYPE_PARAMETER_INDENT",
        deserialize_with = "Kv::opt_number"
    )]
    pub generic_type_parameter_indent: Option<i64>,
    /// `INDENT_SIZE` in `<indentOptions>`; `indent_size` in `.editorconfig`.
    #[serde(rename = "INDENT_SIZE", deserialize_with = "Kv::opt_number")]
    pub indent_size: Option<i64>,
    /// `KEEP_INDENTS_ON_EMPTY_LINES` in `<indentOptions>`; `ij_java_keep_indents_on_empty_lines` in `.editorconfig`.
    #[serde(
        rename = "KEEP_INDENTS_ON_EMPTY_LINES",
        deserialize_with = "Kv::opt_bool"
    )]
    pub keep_indents_on_empty_lines: Option<bool>,
    /// `LABEL_INDENT_ABSOLUTE` in `<indentOptions>`; `XML only` in `.editorconfig`.
    #[serde(rename = "LABEL_INDENT_ABSOLUTE", deserialize_with = "Kv::opt_bool")]
    pub label_indent_absolute: Option<bool>,
    /// `LABEL_INDENT_SIZE` in `<indentOptions>`; `XML only` in `.editorconfig`.
    #[serde(rename = "LABEL_INDENT_SIZE", deserialize_with = "Kv::opt_number")]
    pub label_indent_size: Option<i64>,
    /// `SMART_TABS` in `<indentOptions>`; `ij_smart_tabs` in `.editorconfig`.
    #[serde(rename = "SMART_TABS", deserialize_with = "Kv::opt_bool")]
    pub smart_tabs: Option<bool>,
    /// `TAB_SIZE` in `<indentOptions>`; `tab_width` in `.editorconfig`.
    #[serde(rename = "TAB_SIZE", deserialize_with = "Kv::opt_number")]
    pub tab_size: Option<i64>,
    /// `USE_RELATIVE_INDENTS` in `<indentOptions>`; `XML only` in `.editorconfig`.
    #[serde(rename = "USE_RELATIVE_INDENTS", deserialize_with = "Kv::opt_bool")]
    pub use_relative_indents: Option<bool>,
    /// `USE_TAB_CHARACTER` in `<indentOptions>`; `indent_style` in `.editorconfig`.
    #[serde(rename = "USE_TAB_CHARACTER", deserialize_with = "Kv::opt_tab_flag")]
    pub use_tab_character: Option<bool>,
}
