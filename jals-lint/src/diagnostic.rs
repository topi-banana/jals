//! The result of linting: the diagnostics found, plus any parser errors.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_syntax::SyntaxError;

use crate::rules::Finding;

/// How serious a lint finding is, re-exported from the shared config crate. Doubles as the per-rule
/// configuration value ([`jalslint.toml`](jals_config::lint::Config)): a rule set to
/// [`Allow`](Severity::Allow) is disabled and never runs.
pub use jals_config::Severity;

/// A single lint diagnostic: a rule firing at a byte range in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The name of the rule that produced this diagnostic (kebab-case, e.g. `wildcard-import`).
    pub rule: &'static str,
    /// The resolved severity for this diagnostic.
    pub severity: Severity,
    /// Human-readable message.
    pub message: String,
    /// Byte range in the original source.
    pub range: Range<usize>,
    /// Whether the diagnostic's own range is unnecessary code (e.g. an unused local) — a consumer
    /// may render it faded in place. The LSP tags it `Unnecessary`; the CLI ignores it. `false`
    /// for nearly every rule.
    pub unnecessary: bool,
    /// A secondary unnecessary-code range with its own message — e.g. the dead branch of a
    /// constant `if`. The LSP renders it as a hint diagnostic tagged `Unnecessary`; the CLI
    /// ignores it. `None` for nearly every rule.
    pub unnecessary_range: Option<(Range<usize>, String)>,
}

impl Diagnostic {
    /// Build a diagnostic from a rule's [`Finding`], stamping it with the rule name and the
    /// severity resolved from configuration.
    pub(crate) fn new(rule: &'static str, severity: Severity, finding: Finding) -> Self {
        Self {
            rule,
            severity,
            message: finding.message,
            range: finding.range,
            unnecessary: finding.unnecessary,
            unnecessary_range: finding.unnecessary_range,
        }
    }

    /// Build an `Error` diagnostic from a parser [`SyntaxError`], under the `syntax-error` rule.
    pub(crate) fn from_syntax_error(err: &SyntaxError) -> Self {
        let range = err.range();
        Self {
            rule: "syntax-error",
            severity: Severity::Error,
            message: err.message().to_owned(),
            range: usize::from(range.start())..usize::from(range.end()),
            unnecessary: false,
            unnecessary_range: None,
        }
    }
}

/// The output of [`lint_source`](crate::lint_source).
#[derive(Debug, Clone)]
pub struct LintOutput {
    /// Diagnostics produced by the enabled rules, sorted by start offset.
    pub diagnostics: Vec<Diagnostic>,
    /// Syntax errors recorded by the parser (reported under the `syntax-error` rule).
    pub parse_errors: Vec<Diagnostic>,
}

impl LintOutput {
    /// Whether any diagnostic or parser error was produced.
    pub const fn has_diagnostics(&self) -> bool {
        !self.diagnostics.is_empty() || !self.parse_errors.is_empty()
    }
}
