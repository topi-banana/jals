//! `[imports]` and modifier ordering — the two passes that reorder significant tokens.
//!
//! Everything here changes the token *sequence* (never the multiset). [`Imports::order`] defaults
//! to [`Sort`](ImportOrder::Sort) — rustfmt's `reorder_imports = true` — so the default already
//! reorders; `granularity` / `reorder-modifiers` / `remove-unused` stay off/`preserve`. The Eclipse
//! JDT formatter deliberately owns none of this (Organize Imports is a separate IDE action), so the
//! vendors that do are IntelliJ, Spotless, and google-java-format
//! (`jals-fmt/MAPPING.md` §5.6).

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

/// How `import` declarations are ordered.
///
/// Replaces the former `reorder-imports` / `group-imports` pair, where the second implied and
/// overrode the first — a three-valued choice spelled as two booleans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportOrder {
    /// Leave the import block exactly as written.
    Preserve,
    /// Sort every import alphabetically as one block, non-static before static. The default —
    /// rustfmt's `reorder_imports = true`. `group_imports = Preserve` has no jals mode; [`Group`]
    /// would apply `java.` / `javax.` prefixes rustfmt does not.
    Sort,
    /// Sort into the blocks named by [`groups`](Imports::groups), separated by
    /// `blank-lines.between-import-groups`. IntelliJ `IMPORT_LAYOUT_TABLE` /
    /// Spotless `importOrder(...)` / google-java-format's `ImportOrderer`.
    Group,
}

/// How many types one `import` declaration names.
///
/// Mirrors rustfmt's `imports_granularity`, over the jals dialect's grouped import
/// (`import java.util.{HashMap, List};` — the Java analogue of a `use` tree), plus a
/// [`Preserve`](Self::Preserve) default that leaves every declaration exactly as written. No Java
/// formatter has this: the construct is jals's own syntax, so `[literals]` is its only company in
/// the rule set (`jals-fmt/MAPPING-rustfmt.md` §6).
///
/// rustfmt's `Crate` and `Module` collapse into the single [`Package`](Self::Package) value —
/// Java has no crate, and its module is the package — and its `One` has no Java spelling at all,
/// because a group shares one package prefix and two packages cannot join in one declaration.
///
/// # `package` requires the dialect
///
/// [`Package`](Self::Package) *writes* grouped imports, which only compile when the project
/// enables the `grouped-imports` feature — the frontend keys its desugaring off
/// [`FeatureSet::permits`](crate::FeatureSet::permits), so an undeclared group reaches
/// `javac` verbatim. The formatter is therefore given the project's feature set and rounds this
/// value back to [`Preserve`](Self::Preserve) with a warning when the dialect is off. Splitting
/// ([`Item`](Self::Item)) only ever *removes* dialect syntax and is safe either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportGranularity {
    /// Leave each declaration naming exactly what it already named. The default; preserves the
    /// significant-token sequence.
    Preserve,
    /// Merge every import sharing a package into one grouped import
    /// (`import a.B; import a.C;` → `import a.{B, C};`). Requires the `grouped-imports` feature.
    Package,
    /// Split every grouped import into one plain declaration per member
    /// (`import a.{B, C};` → `import a.B; import a.C;`).
    Item,
}

/// Import ordering and modifier ordering.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Imports {
    /// How imports are ordered.
    pub order: ImportOrder,
    /// How many types one declaration names — whether grouped imports are merged or split.
    pub granularity: ImportGranularity,
    /// The ordered groups consulted under [`ImportOrder::Group`]: a list of name prefixes. A
    /// non-static import joins the group of its *longest* matching prefix (ties broken by list
    /// order); `"*"` is the catch-all and `"static"` collects every static import. A missing
    /// `"*"` / `"static"` becomes an implicit trailing group.
    pub groups: Vec<String>,
    /// Put the static group first rather than last when [`groups`](Self::groups) does not pin
    /// its position. IntelliJ `LAYOUT_STATIC_IMPORTS_SEPARATELY` /
    /// google-java-format (static block first).
    pub static_first: bool,
    /// Reorder each declaration's keyword modifiers into the canonical JLS order and hoist its
    /// annotations to the front. google-java-format's `ModifierOrderer`, which it always runs;
    /// no Eclipse or IntelliJ equivalent.
    pub reorder_modifiers: bool,
    /// Delete an `import` whose simple name appears nowhere else in the file.
    /// google-java-format's `RemoveUnusedImports` — its `--skip-removing-unused-imports`
    /// inverted — which it always runs.
    ///
    /// The name test is a *syntactic* one: every identifier in the file, plus the reference
    /// names of Javadoc's `@link` / `@see` / `@throws`, form the used set. No type resolution
    /// is involved, so a shadowed name keeps its import alive — the same blind spot
    /// google-java-format has. IntelliJ's optimize-imports resolves the classpath instead and
    /// therefore does not project here (`jals-fmt/MAPPING.md` §7).
    ///
    /// Defaults to `false`. Token-removing operations are enumerated in `jals-fmt`'s own table
    /// (`jals-fmt/DESIGN.md` §20) rather than being read off this section's keys: the dialect
    /// also drops a grouped import's trailing comma unconditionally, and
    /// `[wrapping] remove-nested-parens` is on by default.
    pub remove_unused: bool,
}

impl Default for Imports {
    fn default() -> Self {
        Self {
            order: ImportOrder::Sort,
            granularity: ImportGranularity::Preserve,
            groups: vec![
                "java.".to_owned(),
                "javax.".to_owned(),
                "*".to_owned(),
                "static".to_owned(),
            ],
            static_first: false,
            reorder_modifiers: false,
            remove_unused: false,
        }
    }
}
