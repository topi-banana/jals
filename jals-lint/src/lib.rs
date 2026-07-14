#![cfg_attr(not(test), no_std)]
//! A lint checker for JALS/Java source, driven by the `jals-syntax` CST.
//!
//! [`LintOutput::lint_source`] parses `src` and runs every enabled rule over the lossless CST,
//! returning a [`LintOutput`] of [`Diagnostic`]s. It never panics: a source with syntax errors is
//! still linted best-effort (the CST is lossless), and the parser's errors are surfaced under the
//! `syntax-error` rule.
//!
//! Each rule has a kebab-case name and a built-in default [`Severity`]; a `jalslint.toml` may
//! override any rule's severity, including `allow` to disable it. Rules are read-only and never
//! modify the source.

extern crate alloc;

mod diagnostic;
mod rules;

use alloc::vec::Vec;
use core::cell::OnceCell;

use jals_config::Severity;
use jals_config::lint::Config;
use jals_hir::{FileId, ProjectIndex, Resolved};
use jals_syntax::{Parse, SyntaxNode};

use rules::{Checker, FeatureGate, Finding};

pub use diagnostic::{Diagnostic, LintOutput};

/// The project context the index-aware rules resolve reference types against.
///
/// A project-wide symbol index plus the id of the file being linted within it. `None` selects the
/// file-local behavior. See [`LintOutput::lint_parse_with_index`].
pub type IndexCtx<'a> = (&'a ProjectIndex, FileId);

/// The kebab-case name of the `type-mismatch` rule.
///
/// Exposed so a consumer holding a project index (the language server) can suppress this file-local
/// rule and run its index-aware variant under the same config key instead of double-reporting.
pub const TYPE_MISMATCH_RULE: &str = "type-mismatch";

impl LintOutput {
    /// Lint `src` according to `config`.
    ///
    /// This is the file-local entry point: reference types resolve only by spelling. A caller
    /// holding a project-wide [`ProjectIndex`] (the CLI over a multi-file run, the language server)
    /// uses [`LintOutput::lint_parse_with_index`] instead to also catch cross-file type mismatches.
    pub fn lint_source(src: &str, config: &Config) -> Self {
        Self::lint_parse_with_index(&jals_syntax::Parse::parse(src), config, None)
    }

    /// Lint an already-parsed `parse`, optionally resolving reference types against a project
    /// `index` (this file being its [`FileId`] within that index).
    ///
    /// This is the project-aware counterpart of [`LintOutput::lint_source`], taking a cached
    /// [`Parse`] so a caller that built a [`ProjectIndex`] from every project source (and thus
    /// already parsed each file) does not reparse. With `index` `None` it is exactly
    /// [`LintOutput::lint_source`]; with `Some`, the `type-mismatch` rule additionally catches
    /// project-internal subtyping mismatches and cross-file call-argument errors. Name resolution
    /// is computed once and shared across the resolution-based rules.
    pub fn lint_parse_with_index(parse: &Parse, config: &Config, index: Option<IndexCtx>) -> Self {
        let diagnostics = Self::lint_node_with_index(&parse.syntax(), config, index);
        let parse_errors = parse
            .errors()
            .iter()
            .map(Diagnostic::from_syntax_error)
            .collect();

        Self {
            diagnostics,
            parse_errors,
        }
    }

    /// Run every enabled rule over an already-parsed CST `root`, returning the rule diagnostics
    /// sorted by start offset.
    ///
    /// This is the rule half of [`LintOutput::lint_source`], split out so a caller that already
    /// holds a parse tree (e.g. the language server, which caches it per document) can lint without
    /// reparsing. Parser errors are *not* included — they belong to the parse, not the rules.
    pub fn lint_node(root: &SyntaxNode, config: &Config) -> Vec<Diagnostic> {
        Self::lint_node_with_index(root, config, None)
    }

    /// The rule engine, shared by [`LintOutput::lint_node`] (with `index` `None`) and
    /// [`LintOutput::lint_parse_with_index`].
    ///
    /// File-local name resolution is shared across every resolution-based rule and computed lazily,
    /// so a configuration that enables only syntactic rules (or disables the resolution-based ones)
    /// never pays for it, and one that enables several resolves just once. The `index`, when
    /// present, is threaded only into [`Checker::Indexed`] rules.
    fn lint_node_with_index(
        root: &SyntaxNode,
        config: &Config,
        index: Option<IndexCtx>,
    ) -> Vec<Diagnostic> {
        let resolved = OnceCell::new();
        let mut diagnostics = Vec::new();
        for rule in rules::RULES {
            let severity = config.severity(rule.name, rule.default);
            if severity == Severity::Allow {
                continue;
            }
            let findings = match rule.check {
                Checker::Syntactic(check) => check(root),
                Checker::Resolved(check) => {
                    check(root, resolved.get_or_init(|| Resolved::resolve_node(root)))
                }
                Checker::Indexed(check) => check(
                    root,
                    resolved.get_or_init(|| Resolved::resolve_node(root)),
                    index,
                ),
                // Run a feature-gated rule's detector only when the project's set does not permit
                // its guarded feature (`FeatureSet::permits` owns the empty-set exemption),
                // stamping the shared gate message on each node the detector located.
                Checker::Gated {
                    feature,
                    subject,
                    find,
                } => {
                    if config.features.permits(feature) {
                        Vec::new()
                    } else {
                        let message = FeatureGate::preview_message(feature, subject);
                        find(root)
                            .iter()
                            .map(|node| Finding::at_node(node, message.clone()))
                            .collect()
                    }
                }
            };
            for finding in findings {
                diagnostics.push(Diagnostic::new(rule.name, severity, finding));
            }
        }
        diagnostics.sort_by_key(|d| d.range.start);
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_node_reports_rule_findings_without_parse_errors() {
        // `import java.util.*;` is well-formed but trips the `wildcard-import` rule.
        let root = jals_syntax::Parse::parse("import java.util.*;\nclass C {}\n").syntax();
        let diagnostics = LintOutput::lint_node(&root, &Config::default());
        assert!(
            diagnostics.iter().any(|d| d.rule == "wildcard-import"),
            "expected a wildcard-import finding: {diagnostics:?}"
        );
        // `lint_node` is the rule half only: parser's `syntax-error` rule never appears here.
        assert!(diagnostics.iter().all(|d| d.rule != "syntax-error"));
    }

    #[test]
    fn lint_node_matches_lint_source_rule_diagnostics() {
        let src = "import java.util.*;\nclass C {}\n";
        let cfg = Config::default();
        let root = jals_syntax::Parse::parse(src).syntax();
        assert_eq!(
            LintOutput::lint_node(&root, &cfg),
            LintOutput::lint_source(src, &cfg).diagnostics
        );
    }

    #[test]
    fn lint_parse_with_index_without_index_matches_lint_source() {
        // The `None` path is exactly `lint_source` — the delegation must not drift.
        let src = "import java.util.*;\nclass C { int x = 1.0; }\n";
        let cfg = Config::default();
        let parse = jals_syntax::Parse::parse(src);
        let with_none = LintOutput::lint_parse_with_index(&parse, &cfg, None);
        let file_local = LintOutput::lint_source(src, &cfg);
        assert_eq!(with_none.diagnostics, file_local.diagnostics);
        assert_eq!(with_none.parse_errors, file_local.parse_errors);
    }

    #[test]
    fn lint_parse_with_index_catches_project_subtyping() {
        // `Base` is not assignable to `Sub`. Reference subtyping resolves only against a project
        // index, so the file-local `lint_source` cannot see this, but `lint_parse_with_index` can.
        // A field initializer keeps `unused-local` out of the way, isolating `type-mismatch`.
        let src = "class Base {} class Sub extends Base {} class C { Sub f = new Base(); }";
        let cfg = Config::default();
        let parse = jals_syntax::Parse::parse(src);

        // File-local: the subtyping mismatch is invisible.
        assert!(
            LintOutput::lint_source(src, &cfg)
                .diagnostics
                .iter()
                .all(|d| d.rule != TYPE_MISMATCH_RULE)
        );

        // Index-aware: it is flagged.
        let index =
            jals_hir::ProjectIndex::builder(&[(jals_hir::FileId(0), parse.syntax())]).build();
        let out =
            LintOutput::lint_parse_with_index(&parse, &cfg, Some((&index, jals_hir::FileId(0))));
        assert!(
            out.diagnostics.iter().any(|d| d.rule == TYPE_MISMATCH_RULE
                && d.message.contains("Base")
                && d.message.contains("Sub")),
            "expected an index-aware type-mismatch: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn constant_condition_carries_the_dead_branch_range() {
        let src = "class C { void m() { if (true) { a(); } else { b(); } } }";
        let root = jals_syntax::Parse::parse(src).syntax();
        let diagnostics = LintOutput::lint_node(&root, &Config::default());
        let constant = diagnostics
            .iter()
            .find(|d| d.rule == "constant-condition")
            .expect("a constant-condition diagnostic");
        let else_start = src.find("{ b(); }").unwrap();
        assert_eq!(
            constant.unnecessary_range,
            Some((
                else_start..else_start + "{ b(); }".len(),
                "this code is never executed".to_owned()
            ))
        );
        // Every other rule leaves the secondary range empty.
        let wildcard_root = jals_syntax::Parse::parse("import java.util.*;\nclass C {}\n").syntax();
        let diagnostics = LintOutput::lint_node(&wildcard_root, &Config::default());
        let wildcard = diagnostics
            .iter()
            .find(|d| d.rule == "wildcard-import")
            .expect("a wildcard-import diagnostic");
        assert_eq!(wildcard.unnecessary_range, None);
    }

    #[test]
    fn unreported_exception_needs_the_index() {
        // A checked exception thrown but not declared. Classifying it as checked and finding it
        // undeclared needs the project index (with stdlib), so `lint_source` cannot see it.
        let src = "class MyEx extends Exception {} class C { void f() { throw new MyEx(); } }";
        let cfg = Config::default();
        let parse = jals_syntax::Parse::parse(src);

        // File-local: nothing to report without the hierarchy.
        assert!(
            LintOutput::lint_source(src, &cfg)
                .diagnostics
                .iter()
                .all(|d| d.rule != "unreported-exception")
        );

        // Index-aware (with stdlib): the undeclared checked exception is flagged.
        let index = jals_hir::ProjectIndex::builder(&[(jals_hir::FileId(0), parse.syntax())])
            .with_stdlib()
            .build();
        let out =
            LintOutput::lint_parse_with_index(&parse, &cfg, Some((&index, jals_hir::FileId(0))));
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.rule == "unreported-exception" && d.message.contains("MyEx")),
            "expected an unreported-exception finding: {:?}",
            out.diagnostics
        );
    }
}
