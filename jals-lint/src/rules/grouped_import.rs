//! `grouped-import`: flag jals grouped imports (`import java.util.{HashMap, ArrayList};`) when the
//! project's `grouped-imports` dialect feature is not enabled.
//!
//! Grouped imports are a jals-specific dialect construct — not valid Java at any release — so the
//! [`Feature::GroupedImports`] capability guards them. The rule fires when the project's resolved
//! feature set (from `[package] features`) does *not* include it, and reports nothing when the
//! feature is enabled or no feature set is declared. The rule driver applies the gate (see
//! [`Checker::Gated`]) and stamps the "add it to `[package] features`" hint; this rule only detects
//! the syntax. The compile frontend desugars enabled grouped imports into plain imports.

use alloc::vec::Vec;

use jals_config::Feature;
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{AstNode, SourceFile};

use crate::diagnostic::Severity;
use crate::rules::{Checker, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "grouped-import",
    default: Severity::Error,
    check: Checker::Gated {
        feature: Feature::GroupedImports,
        subject: "grouped imports (`import a.b.{X, Y};`)",
        find: GroupedImport::find,
    },
};

/// The `grouped-import` rule.
struct GroupedImport;

impl GroupedImport {
    fn find(root: &SyntaxNode) -> Vec<SyntaxNode> {
        // The driver runs this only when `grouped-imports` is disabled and stamps the gate message,
        // so here we just locate the syntax (nothing when the root is not a source file).
        let Some(file) = SourceFile::cast(root.clone()) else {
            return Vec::new();
        };
        // Import declarations only appear as direct children of the source file. A grouped import is
        // exactly one that carries an `ImportGroup` (`.{ ... }`); an ordinary import has none.
        file.imports()
            .filter(|import| import.group().is_some())
            .map(|import| import.syntax().clone())
            .collect()
    }
}
