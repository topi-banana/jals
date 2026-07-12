//! The lint rules and the registry that drives them.
//!
//! Each rule is a pure function `fn(&SyntaxNode) -> Vec<Finding>` paired with metadata
//! ([`RuleMeta`]): a kebab-case name and a built-in default [`Severity`]. The library walks the
//! parsed CST, runs every enabled rule, and stamps each [`Finding`] with the rule name and the
//! severity resolved from configuration. Rules never mutate the tree and never panic.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_hir::Resolved;
use jals_syntax::ast::{AstNode, SourceFile};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::IndexCtx;

use crate::diagnostic::Severity;

mod compact_source_file;
mod constant_condition;
mod empty_catch;
mod missing_braces;
mod module_import;
mod naming;
mod type_mismatch;
mod unreported_exception;
mod unused_local;
mod wildcard_import;

/// A potential problem reported by a rule, before it is tagged with a rule name / severity.
#[derive(Default)]
pub(crate) struct Finding {
    /// Byte range in the original source.
    pub range: Range<usize>,
    /// Human-readable message.
    pub message: String,
    /// Whether the finding's own range is unnecessary code (e.g. an unused local) — a consumer may
    /// render it faded in place. `false` for nearly every rule.
    pub unnecessary: bool,
    /// A secondary unnecessary-code range with its own message — e.g. the dead branch of a
    /// constant `if` (the LSP renders it as a faded hint diagnostic). `None` for nearly every
    /// rule.
    pub unnecessary_range: Option<(Range<usize>, String)>,
}

impl Finding {
    /// A finding spanning `node`.
    pub(crate) fn at_node(node: &SyntaxNode, message: impl Into<String>) -> Self {
        let range = node.text_range();
        Self {
            range: usize::from(range.start())..usize::from(range.end()),
            message: message.into(),
            ..Self::default()
        }
    }

    /// A finding spanning `token`.
    pub(crate) fn at_token(token: &SyntaxToken, message: impl Into<String>) -> Self {
        let range = token.text_range();
        Self {
            range: usize::from(range.start())..usize::from(range.end()),
            message: message.into(),
            ..Self::default()
        }
    }
}

/// The version gate shared by the edition-gated ([`Checker::Versioned`]) rules.
pub(crate) struct VersionGate;

impl VersionGate {
    /// The shared preamble of an edition-gated ([`Checker::Versioned`]) rule: gate on the project's
    /// target Java version, then cast the root to a [`SourceFile`]. Returns `Some((version, file))`
    /// — the target feature version to name in the diagnostic and the file to scan — only when a
    /// feature that stabilized in `stable_in` is still a *preview* at the target
    /// (`target_java_version` is declared and *below* `stable_in`). Returns `None` (so the rule
    /// reports nothing) when no edition is declared, the target is already at/above `stable_in`, or
    /// the root is not a source file.
    pub(crate) fn source_file(
        target_java_version: Option<u32>,
        stable_in: u32,
        root: &SyntaxNode,
    ) -> Option<(u32, SourceFile)> {
        let version = target_java_version.filter(|&v| v < stable_in)?;
        let file = SourceFile::cast(root.clone())?;
        Some((version, file))
    }
}

/// How a rule is invoked. Most rules need only the CST; resolution-based rules additionally take
/// the file-local name resolution, which the library computes at most once per lint (see
/// [`crate::lint_node`]) and shares across every [`Checker::Resolved`] / [`Checker::Indexed`] rule.
#[derive(Clone, Copy)]
pub(crate) enum Checker {
    /// A pure syntactic rule: given the CST root, return every finding.
    Syntactic(fn(&SyntaxNode) -> Vec<Finding>),
    /// A rule that also consumes `jals-hir` file-local name resolution.
    Resolved(fn(&SyntaxNode, &Resolved) -> Vec<Finding>),
    /// A rule that, in addition to name resolution, may resolve reference types against a
    /// project-wide symbol index when the caller supplies one ([`IndexCtx`]); with no index it
    /// falls back to the file-local behavior. The basis for cross-file type checking.
    Indexed(fn(&SyntaxNode, &Resolved, Option<IndexCtx>) -> Vec<Finding>),
    /// A syntactic rule gated on the project's target Java version (feature release), threaded from
    /// the host via [`Config::target_java_version`](crate::Config::target_java_version). `None`
    /// disables the gate (the rule reports nothing), so an edition-specific check never fires for a
    /// project that did not declare its edition.
    Versioned(fn(&SyntaxNode, Option<u32>) -> Vec<Finding>),
}

/// A rule: its identity and its checker.
pub(crate) struct RuleMeta {
    /// Stable kebab-case name, used as the config key and shown in diagnostics.
    pub name: &'static str,
    /// Severity used when the rule is not configured.
    pub default: Severity,
    /// The checker, syntactic or resolution-based.
    pub check: Checker,
}

/// Every rule, in a stable order.
pub(crate) const RULES: &[RuleMeta] = &[
    naming::RULE,
    wildcard_import::RULE,
    empty_catch::RULE,
    missing_braces::RULE,
    compact_source_file::RULE,
    module_import::RULE,
    constant_condition::RULE,
    unused_local::RULE,
    type_mismatch::RULE,
    unreported_exception::RULE,
];
