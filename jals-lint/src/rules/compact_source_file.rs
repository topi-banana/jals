//! `compact-source-file`: flag top-level members of a compact source file (a top-level `main`, or any
//! top-level field / method — the implicit-class members of JEP 512) when the project's
//! `compact-source-files` language feature is not enabled.
//!
//! Compact source files and instance main methods are a *preview* feature in Java 24 (usable only with
//! `--enable-preview`) and a permanent feature in Java 25. The rule guards the
//! [`Feature::CompactSourceFiles`] capability: it fires when the project's resolved feature set (from
//! `[package] features`) does *not* include it, and reports nothing when the feature is enabled or no
//! feature set is declared. The rule driver applies the gate (see [`Checker::Gated`]); this rule only
//! detects the syntax.

use alloc::vec::Vec;

use jals_config::Feature;
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{AstNode, Decl, SourceFile};

use crate::diagnostic::Severity;
use crate::rules::{Checker, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "compact-source-file",
    default: Severity::Error,
    check: Checker::Gated {
        feature: Feature::CompactSourceFiles,
        subject: "top-level declarations like `main`",
        find: CompactSourceFile::find,
    },
};

/// The `compact-source-file` rule.
struct CompactSourceFile;

impl CompactSourceFile {
    fn find(root: &SyntaxNode) -> Vec<SyntaxNode> {
        // The driver runs this only when `compact-source-files` is disabled and stamps the gate
        // message, so here we just locate the syntax (nothing when the root is not a source file).
        let Some(file) = SourceFile::cast(root.clone()) else {
            return Vec::new();
        };
        file.decls()
            // A field or method declared directly at the top level is a compact source file's
            // implicit-class member (JEP 512); a type declaration (class/interface/enum/record) is
            // ordinary Java and never flagged.
            .filter(|decl| matches!(decl, Decl::Method(_) | Decl::Field(_)))
            .map(|decl| decl.syntax().clone())
            .collect()
    }
}
