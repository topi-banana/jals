//! IntelliJ — the import-layout settings of `JavaCodeStyleSettings`.
//!
//! `IMPORT_LAYOUT_TABLE` and `PACKAGES_TO_USE_IMPORT_ON_DEMAND` are ordered
//! [`PackageEntryTable`]s, spelled as `<package>` / `<emptyLine/>` elements in XML and as a
//! comma-separated mini-list in `.editorconfig`. The on-demand thresholds need name resolution
//! and are therefore carried but not projected (`MAPPING.md` §7).

use crate::import::serde_kv;
use serde::Deserialize;

use super::values::PackageEntryTable;

/// The import-layout settings of a Java code style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijImports {
    /// `CLASS_COUNT_TO_USE_IMPORT_ON_DEMAND` in `<JavaCodeStyleSettings>`; `ij_java_class_count_to_use_import_on_demand` in `.editorconfig`.
    #[serde(
        rename = "CLASS_COUNT_TO_USE_IMPORT_ON_DEMAND",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub class_count_to_use_import_on_demand: Option<i64>,
    /// `DELETE_UNUSED_MODULE_IMPORTS` in `<JavaCodeStyleSettings>`; `ij_java_delete_unused_module_imports` in `.editorconfig`.
    #[serde(
        rename = "DELETE_UNUSED_MODULE_IMPORTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub delete_unused_module_imports: Option<bool>,
    /// `IMPORT_LAYOUT_TABLE` in `<JavaCodeStyleSettings>`; `ij_java_imports_layout` in `.editorconfig`.
    #[serde(
        rename = "IMPORT_LAYOUT_TABLE",
        deserialize_with = "PackageEntryTable::opt_deserialize"
    )]
    pub import_layout_table: Option<PackageEntryTable>,
    /// `INSERT_INNER_CLASS_IMPORTS` in `<JavaCodeStyleSettings>`; `ij_java_insert_inner_class_imports` in `.editorconfig`.
    #[serde(
        rename = "INSERT_INNER_CLASS_IMPORTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub insert_inner_class_imports: Option<bool>,
    /// `LAYOUT_ON_DEMAND_IMPORT_FROM_SAME_PACKAGE_FIRST` in `<JavaCodeStyleSettings>`; `ij_java_layout_on_demand_import_from_same_package_first` in `.editorconfig`.
    #[serde(
        rename = "LAYOUT_ON_DEMAND_IMPORT_FROM_SAME_PACKAGE_FIRST",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub layout_on_demand_import_from_same_package_first: Option<bool>,
    /// `LAYOUT_STATIC_IMPORTS_SEPARATELY` in `<JavaCodeStyleSettings>`; `ij_java_layout_static_imports_separately` in `.editorconfig`.
    #[serde(
        rename = "LAYOUT_STATIC_IMPORTS_SEPARATELY",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub layout_static_imports_separately: Option<bool>,
    /// `NAMES_COUNT_TO_USE_IMPORT_ON_DEMAND` in `<JavaCodeStyleSettings>`; `ij_java_names_count_to_use_import_on_demand` in `.editorconfig`.
    #[serde(
        rename = "NAMES_COUNT_TO_USE_IMPORT_ON_DEMAND",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub names_count_to_use_import_on_demand: Option<i64>,
    /// `PACKAGES_TO_USE_IMPORT_ON_DEMAND` in `<JavaCodeStyleSettings>`; `ij_java_packages_to_use_import_on_demand` in `.editorconfig`.
    #[serde(
        rename = "PACKAGES_TO_USE_IMPORT_ON_DEMAND",
        deserialize_with = "PackageEntryTable::opt_deserialize"
    )]
    pub packages_to_use_import_on_demand: Option<PackageEntryTable>,
    /// `PRESERVE_MODULE_IMPORTS` in `<JavaCodeStyleSettings>`; `ij_java_preserve_module_imports` in `.editorconfig`.
    #[serde(
        rename = "PRESERVE_MODULE_IMPORTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub preserve_module_imports: Option<bool>,
    /// `USE_FQ_CLASS_NAMES` in `<JavaCodeStyleSettings>`; `ij_java_use_fq_class_names` in `.editorconfig`.
    #[serde(rename = "USE_FQ_CLASS_NAMES", deserialize_with = "serde_kv::opt_bool")]
    pub use_fq_class_names: Option<bool>,
    /// `USE_SINGLE_CLASS_IMPORTS` in `<JavaCodeStyleSettings>`; `ij_java_use_single_class_imports` in `.editorconfig`.
    #[serde(
        rename = "USE_SINGLE_CLASS_IMPORTS",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub use_single_class_imports: Option<bool>,
}
