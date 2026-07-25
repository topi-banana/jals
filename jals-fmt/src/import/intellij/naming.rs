//! IntelliJ — the naming-convention settings of `JavaCodeStyleSettings`.
//!
//! Part of the `ij_java_*` surface, but they drive code *generation* and inspections, not the
//! formatter, so they are modeled for completeness and deliberately not projected
//! (`MAPPING.md` §7).

use alloc::string::String;

use serde::Deserialize;

use super::super::serde_kv::Kv;

/// The naming-convention settings of a Java code style (not formatter rules).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijNaming {
    /// `FIELD_NAME_PREFIX` in `<JavaCodeStyleSettings>`; `ij_java_field_name_prefix` in `.editorconfig`.
    #[serde(rename = "FIELD_NAME_PREFIX")]
    pub field_name_prefix: Option<String>,
    /// `FIELD_NAME_SUFFIX` in `<JavaCodeStyleSettings>`; `ij_java_field_name_suffix` in `.editorconfig`.
    #[serde(rename = "FIELD_NAME_SUFFIX")]
    pub field_name_suffix: Option<String>,
    /// `LOCAL_VARIABLE_NAME_PREFIX` in `<JavaCodeStyleSettings>`; `ij_java_local_variable_name_prefix` in `.editorconfig`.
    #[serde(rename = "LOCAL_VARIABLE_NAME_PREFIX")]
    pub local_variable_name_prefix: Option<String>,
    /// `LOCAL_VARIABLE_NAME_SUFFIX` in `<JavaCodeStyleSettings>`; `ij_java_local_variable_name_suffix` in `.editorconfig`.
    #[serde(rename = "LOCAL_VARIABLE_NAME_SUFFIX")]
    pub local_variable_name_suffix: Option<String>,
    /// `PARAMETER_NAME_PREFIX` in `<JavaCodeStyleSettings>`; `ij_java_parameter_name_prefix` in `.editorconfig`.
    #[serde(rename = "PARAMETER_NAME_PREFIX")]
    pub parameter_name_prefix: Option<String>,
    /// `PARAMETER_NAME_SUFFIX` in `<JavaCodeStyleSettings>`; `ij_java_parameter_name_suffix` in `.editorconfig`.
    #[serde(rename = "PARAMETER_NAME_SUFFIX")]
    pub parameter_name_suffix: Option<String>,
    /// `PREFER_LONGER_NAMES` in `<JavaCodeStyleSettings>`; `ij_java_prefer_longer_names` in `.editorconfig`.
    #[serde(rename = "PREFER_LONGER_NAMES", deserialize_with = "Kv::opt_bool")]
    pub prefer_longer_names: Option<bool>,
    /// `STATIC_FIELD_NAME_PREFIX` in `<JavaCodeStyleSettings>`; `ij_java_static_field_name_prefix` in `.editorconfig`.
    #[serde(rename = "STATIC_FIELD_NAME_PREFIX")]
    pub static_field_name_prefix: Option<String>,
    /// `STATIC_FIELD_NAME_SUFFIX` in `<JavaCodeStyleSettings>`; `ij_java_static_field_name_suffix` in `.editorconfig`.
    #[serde(rename = "STATIC_FIELD_NAME_SUFFIX")]
    pub static_field_name_suffix: Option<String>,
    /// `SUBCLASS_NAME_PREFIX` in `<JavaCodeStyleSettings>`; `ij_java_subclass_name_prefix` in `.editorconfig`.
    #[serde(rename = "SUBCLASS_NAME_PREFIX")]
    pub subclass_name_prefix: Option<String>,
    /// `SUBCLASS_NAME_SUFFIX` in `<JavaCodeStyleSettings>`; `ij_java_subclass_name_suffix` in `.editorconfig`.
    #[serde(rename = "SUBCLASS_NAME_SUFFIX")]
    pub subclass_name_suffix: Option<String>,
    /// `TEST_NAME_PREFIX` in `<JavaCodeStyleSettings>`; `ij_java_test_name_prefix` in `.editorconfig`.
    #[serde(rename = "TEST_NAME_PREFIX")]
    pub test_name_prefix: Option<String>,
    /// `TEST_NAME_SUFFIX` in `<JavaCodeStyleSettings>`; `ij_java_test_name_suffix` in `.editorconfig`.
    #[serde(rename = "TEST_NAME_SUFFIX")]
    pub test_name_suffix: Option<String>,
    /// `VISIBILITY` in `<JavaCodeStyleSettings>`; `ij_java_visibility` in `.editorconfig`.
    #[serde(rename = "VISIBILITY")]
    pub visibility: Option<String>,
}
