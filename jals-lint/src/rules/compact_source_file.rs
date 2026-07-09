//! `compact-source-file`: flag top-level members of a compact source file (a top-level `main`,
//! or any top-level field / method — the implicit-class members of JEP 512) when the project's
//! Java edition is below 25, where the feature is only a preview.
//!
//! Compact source files and instance main methods are a *preview* feature in Java 24 (usable only
//! with `--enable-preview`) and a permanent feature in Java 25. When the manifest's
//! `[package] edition` targets Java 24, using the syntax is flagged; when it targets Java 25 — or
//! when no edition is declared — nothing is reported. The gate is threaded in as the project's
//! target feature version (see [`Config::target_java_version`](crate::Config::target_java_version)).

use alloc::format;
use alloc::vec::Vec;

use jals_syntax::SyntaxNode;
use jals_syntax::ast::{AstNode, Decl};

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta, gated_source_file};

/// The Java feature release in which compact source files / instance main methods became a
/// permanent (non-preview) feature. At or above this version the syntax is allowed.
const STABLE_VERSION: u32 = 25;

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "compact-source-file",
    default: Severity::Error,
    check: Checker::Versioned(check),
};

fn check(root: &SyntaxNode, target_java_version: Option<u32>) -> Vec<Finding> {
    // Gate on the edition (report nothing when it is unset, Java 25+, or the root is not a source
    // file) and grab the source file to scan in one step.
    let Some((version, file)) = gated_source_file(target_java_version, STABLE_VERSION, root) else {
        return Vec::new();
    };
    file.decls()
        // A field or method declared directly at the top level is a compact source file's
        // implicit-class member (JEP 512); a type declaration (class/interface/enum/record) is
        // ordinary Java and never flagged.
        .filter(|decl| matches!(decl, Decl::Method(_) | Decl::Field(_)))
        .map(|decl| {
            Finding::at_node(
                decl.syntax(),
                format!(
                    "top-level declarations like `main` are a preview feature before Java \
                     {STABLE_VERSION}; this project targets Java {version}, so declare it inside a \
                     class or set `edition = \"java{STABLE_VERSION}\"`",
                ),
            )
        })
        .collect()
}
