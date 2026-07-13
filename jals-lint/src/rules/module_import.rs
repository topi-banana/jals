//! `module-import`: flag module import declarations (`import module M;`, JEP 511) when the project's
//! `module-imports` language feature is not enabled.
//!
//! Module import declarations are a *preview* feature in Java 23 (JEP 476) and Java 24 (JEP 494,
//! usable only with `--enable-preview`) and a permanent feature in Java 25 (JEP 511). The rule guards
//! the [`Feature::ModuleImports`] capability: it fires when the project's resolved feature set (from
//! `[package] features`) does *not* include it — a release preset below `java25` with no explicit
//! opt-in — and reports nothing when the feature is enabled or no feature set is declared. The rule
//! driver applies the gate (see [`Checker::Gated`]); this rule only detects the syntax.

use alloc::vec::Vec;

use jals_config::Feature;
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{AstNode, SourceFile};

use crate::diagnostic::Severity;
use crate::rules::{Checker, FeatureGate, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "module-import",
    default: Severity::Error,
    check: Checker::Gated {
        feature: Feature::ModuleImports,
        find: ModuleImport::find,
    },
};

/// The `module-import` rule.
struct ModuleImport;

impl ModuleImport {
    fn find(root: &SyntaxNode) -> Vec<Finding> {
        // The driver runs this only when `module-imports` is disabled, so here we just locate the
        // syntax (reporting nothing when the root is not a source file).
        let Some(file) = SourceFile::cast(root.clone()) else {
            return Vec::new();
        };
        let message = FeatureGate::preview_message(
            Feature::ModuleImports,
            "module import declarations (`import module …;`)",
        );
        // Import declarations only appear as direct children of the source file, so iterate them
        // directly (like the sibling `compact-source-file` rule) rather than walking the whole tree.
        // `is_module()` matches `import module M;` (JEP 511), distinct from an ordinary type import
        // of a package/type named `module` (which keeps `module` as an identifier, so `is_module()`
        // is false).
        file.imports()
            .filter(jals_syntax::ast::ImportDecl::is_module)
            .map(|import| Finding::at_node(import.syntax(), message.clone()))
            .collect()
    }
}
