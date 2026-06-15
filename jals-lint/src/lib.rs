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

pub use config::{Config, ConfigError};
pub use diagnostic::{Diagnostic, LintOutput, Severity};

/// Lint `src` according to `config`.
pub fn lint_source(src: &str, config: &Config) -> LintOutput {
    let parse = jals_syntax::parse(src);
    let root = parse.syntax();

    let mut diagnostics = Vec::new();
    for rule in rules::RULES {
        let severity = config.severity(rule.name, rule.default);
        if severity == Severity::Allow {
            continue;
        }
        for finding in (rule.check)(&root) {
            diagnostics.push(Diagnostic::new(rule.name, severity, finding));
        }
    }
    diagnostics.sort_by_key(|d| d.range.start);

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
