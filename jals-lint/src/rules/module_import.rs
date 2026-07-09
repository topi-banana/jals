//! `module-import`: flag module import declarations (`import module M;`, JEP 511) when the
//! project's Java edition is below 25, where the feature is only a preview.
//!
//! Module import declarations are a *preview* feature in Java 23 (JEP 476) and Java 24 (JEP 494,
//! usable only with `--enable-preview`) and a permanent feature in Java 25 (JEP 511). When the
//! manifest's `[package] edition` targets Java 24, using the syntax is flagged; when it targets
//! Java 25 — or when no edition is declared — nothing is reported. The gate is threaded in as the
//! project's target feature version (see [`Config::target_java_version`](crate::Config::target_java_version)).

use alloc::format;
use alloc::vec::Vec;

use jals_syntax::SyntaxNode;
use jals_syntax::ast::AstNode;

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta, gated_source_file};

/// The Java feature release in which module import declarations became a permanent (non-preview)
/// feature. At or above this version the syntax is allowed.
const STABLE_VERSION: u32 = 25;

pub const RULE: RuleMeta = RuleMeta {
    name: "module-import",
    default: Severity::Error,
    check: Checker::Versioned(check),
};

fn check(root: &SyntaxNode, target_java_version: Option<u32>) -> Vec<Finding> {
    // Gate on the edition (report nothing when it is unset, Java 25+, or the root is not a source
    // file) and grab the source file to scan in one step.
    let Some((version, file)) = gated_source_file(target_java_version, STABLE_VERSION, root) else {
        return Vec::new();
    };
    // Import declarations only appear as direct children of the source file, so iterate them
    // directly (like the sibling `compact-source-file` rule) rather than walking the whole tree.
    // `is_module()` matches `import module M;` (JEP 511), distinct from an ordinary type import of a
    // package/type named `module` (which keeps `module` as an identifier, so `is_module()` is false).
    file.imports()
        .filter(jals_syntax::ast::ImportDecl::is_module)
        .map(|import| {
            Finding::at_node(
                import.syntax(),
                format!(
                    "module import declarations (`import module …;`) are a preview feature before \
                     Java {STABLE_VERSION}; this project targets Java {version}, so import the \
                     individual types or set `edition = \"java{STABLE_VERSION}\"`",
                ),
            )
        })
        .collect()
}
