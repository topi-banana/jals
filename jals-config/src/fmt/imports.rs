//! `[imports]` and modifier ordering — the two passes that reorder significant tokens.
//!
//! Everything here changes the token *sequence* (never the multiset), so every key is off or
//! `preserve` by default and the strict invariant holds unless opted into. The Eclipse JDT
//! formatter deliberately owns none of this (Organize Imports is a separate IDE action), so the
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
    /// Leave the import block exactly as written. The default.
    Preserve,
    /// Sort every import alphabetically as one block, non-static before static.
    Sort,
    /// Sort into the blocks named by [`groups`](Imports::groups), separated by
    /// `blank-lines.between-import-groups`. IntelliJ `IMPORT_LAYOUT_TABLE` /
    /// Spotless `importOrder(...)` / google-java-format's `ImportOrderer`.
    Group,
}

/// Import ordering and modifier ordering.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Imports {
    /// How imports are ordered.
    pub order: ImportOrder,
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
    /// The only *configurable* rule in this crate that removes significant tokens, so it defaults
    /// to `false`. It is not the only operation that removes one: the jals dialect drops a grouped
    /// import's trailing comma unconditionally, which is why the formatter's exemptions are
    /// enumerated in `jals-fmt`'s own table (`jals-fmt/DESIGN.md` §20) rather than being read off
    /// this section's keys.
    pub remove_unused: bool,
}

impl Default for Imports {
    fn default() -> Self {
        Self {
            order: ImportOrder::Preserve,
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
