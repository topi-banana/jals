//! A lint checker for JALS/Java source, driven by the `jals-syntax` CST.
//!
//! [`lint_source`] parses `src` and runs every enabled rule over the lossless CST, returning a
//! [`LintOutput`] of [`Diagnostic`]s. It never panics: a source with syntax errors is still
//! linted best-effort (the CST is lossless), and the parser's errors are surfaced under the
//! `syntax-error` rule.
//!
//! Each rule has a kebab-case name and a built-in default [`Severity`]; a `jalslint.toml` may
//! override any rule's severity, including `allow` to disable it. Rules are read-only and never
//! modify the source.

mod config;
mod diagnostic;
mod rules;

use jals_syntax::SyntaxNode;

pub use config::{Config, ConfigError};
pub use diagnostic::{Diagnostic, LintOutput, Severity};

/// Lint `src` according to `config`.
pub fn lint_source(src: &str, config: &Config) -> LintOutput {
    let parse = jals_syntax::parse(src);
    let diagnostics = lint_node(&parse.syntax(), config);

    let parse_errors = parse
        .errors()
        .iter()
        .map(Diagnostic::from_syntax_error)
        .collect();

    LintOutput {
        diagnostics,
        parse_errors,
    }
}

/// Run every enabled rule over an already-parsed CST `root`, returning the rule diagnostics
/// sorted by start offset.
///
/// This is the rule half of [`lint_source`], split out so a caller that already holds a parse
/// tree (e.g. the language server, which caches it per document) can lint without reparsing.
/// Parser errors are *not* included — they belong to the parse, not the rules.
pub fn lint_node(root: &SyntaxNode, config: &Config) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for rule in rules::RULES {
        let severity = config.severity(rule.name, rule.default);
        if severity == Severity::Allow {
            continue;
        }
        for finding in (rule.check)(root) {
            diagnostics.push(Diagnostic::new(rule.name, severity, finding));
        }
    }
    diagnostics.sort_by_key(|d| d.range.start);
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_node_reports_rule_findings_without_parse_errors() {
        // `import java.util.*;` is well-formed but trips the `wildcard-import` rule.
        let root = jals_syntax::parse("import java.util.*;\nclass C {}\n").syntax();
        let diagnostics = lint_node(&root, &Config::default());
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
        let root = jals_syntax::parse(src).syntax();
        assert_eq!(lint_node(&root, &cfg), lint_source(src, &cfg).diagnostics);
    }
}
