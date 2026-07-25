//! IntelliJ — the code-generation and refactoring preferences of `JavaCodeStyleSettings`.
//!
//! Like [`super::naming`], these live in the `ij_java_*` surface without being formatter
//! rules; they are modeled for completeness and not projected (`MAPPING.md` §7).

use alloc::string::String;
use alloc::vec::Vec;

use serde::Deserialize;

use super::super::serde_kv::Kv;

/// The code-generation preferences of a Java code style (not formatter rules).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijCodegen {
    /// `GENERATE_FINAL_LOCALS` in `<JavaCodeStyleSettings>`; `ij_java_generate_final_locals` in `.editorconfig`.
    #[serde(rename = "GENERATE_FINAL_LOCALS", deserialize_with = "Kv::opt_bool")]
    pub generate_final_locals: Option<bool>,
    /// `GENERATE_FINAL_PARAMETERS` in `<JavaCodeStyleSettings>`; `ij_java_generate_final_parameters` in `.editorconfig`.
    #[serde(
        rename = "GENERATE_FINAL_PARAMETERS",
        deserialize_with = "Kv::opt_bool"
    )]
    pub generate_final_parameters: Option<bool>,
    /// `GENERATE_USE_TYPE_ANNOTATION_BEFORE_TYPE` in `<JavaCodeStyleSettings>`; `ij_java_generate_use_type_annotation_before_type` in `.editorconfig`.
    #[serde(
        rename = "GENERATE_USE_TYPE_ANNOTATION_BEFORE_TYPE",
        deserialize_with = "Kv::opt_bool"
    )]
    pub generate_use_type_annotation_before_type: Option<bool>,
    /// `INSERT_OVERRIDE_ANNOTATION` in `<JavaCodeStyleSettings>`; `ij_java_insert_override_annotation` in `.editorconfig`.
    #[serde(
        rename = "INSERT_OVERRIDE_ANNOTATION",
        deserialize_with = "Kv::opt_bool"
    )]
    pub insert_override_annotation: Option<bool>,
    /// `REPEAT_ANNOTATIONS` in `<JavaCodeStyleSettings>`; `ij_java_repeat_annotations` in `.editorconfig`.
    #[serde(rename = "REPEAT_ANNOTATIONS", deserialize_with = "Kv::opt_list")]
    pub repeat_annotations: Option<Vec<String>>,
    /// `REPEAT_SYNCHRONIZED` in `<JavaCodeStyleSettings>`; `ij_java_repeat_synchronized` in `.editorconfig`.
    #[serde(rename = "REPEAT_SYNCHRONIZED", deserialize_with = "Kv::opt_bool")]
    pub repeat_synchronized: Option<bool>,
    /// `REPLACE_CAST` in `<JavaCodeStyleSettings>`; `ij_java_replace_cast` in `.editorconfig`.
    #[serde(rename = "REPLACE_CAST", deserialize_with = "Kv::opt_bool")]
    pub replace_cast: Option<bool>,
    /// `REPLACE_INSTANCEOF` in `<JavaCodeStyleSettings>`; `ij_java_replace_instanceof` in `.editorconfig`.
    #[serde(rename = "REPLACE_INSTANCEOF", deserialize_with = "Kv::opt_bool")]
    pub replace_instanceof: Option<bool>,
    /// `REPLACE_INSTANCEOF_AND_CAST` in `<JavaCodeStyleSettings>`; `ij_java_replace_instanceof_and_cast` in `.editorconfig`.
    #[serde(
        rename = "REPLACE_INSTANCEOF_AND_CAST",
        deserialize_with = "Kv::opt_bool"
    )]
    pub replace_instanceof_and_cast: Option<bool>,
    /// `REPLACE_NULL_CHECK` in `<JavaCodeStyleSettings>`; `ij_java_replace_null_check` in `.editorconfig`.
    #[serde(rename = "REPLACE_NULL_CHECK", deserialize_with = "Kv::opt_bool")]
    pub replace_null_check: Option<bool>,
    /// `REPLACE_SUM` in `<JavaCodeStyleSettings>`; `ij_java_replace_sum_lambda_with_method_ref` in `.editorconfig`.
    #[serde(rename = "REPLACE_SUM", deserialize_with = "Kv::opt_bool")]
    pub replace_sum: Option<bool>,
    /// `USE_EXTERNAL_ANNOTATIONS` in `<JavaCodeStyleSettings>`; `ij_java_use_external_annotations` in `.editorconfig`.
    #[serde(rename = "USE_EXTERNAL_ANNOTATIONS", deserialize_with = "Kv::opt_bool")]
    pub use_external_annotations: Option<bool>,
}
