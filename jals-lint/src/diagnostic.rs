//! The result of linting: the diagnostics found, plus any parser errors.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

use jals_syntax::SyntaxError;
use serde::Deserialize;

use crate::rules::Finding;

/// How serious a lint finding is. Doubles as the per-rule configuration value: a rule set
/// to [`Allow`](Severity::Allow) is disabled and never runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The rule is disabled; it produces no diagnostics.
    Allow,
    /// The finding is a warning.
    Warn,
    /// The finding is an error.
    Error,
}

impl Severity {
    /// The lowercase name (`"allow"` / `"warn"` / `"error"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Allow => "allow",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

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
}

impl Diagnostic {
    /// Build a diagnostic from a rule's [`Finding`], stamping it with the rule name and the
    /// severity resolved from configuration.
    pub(crate) fn new(rule: &'static str, severity: Severity, finding: Finding) -> Diagnostic {
        Diagnostic {
            rule,
            severity,
            message: finding.message,
            range: finding.range,
        }
    }

    /// Build an `Error` diagnostic from a parser [`SyntaxError`], under the `syntax-error` rule.
    pub(crate) fn from_syntax_error(err: &SyntaxError) -> Diagnostic {
        let range = err.range();
        Diagnostic {
            rule: "syntax-error",
            severity: Severity::Error,
            message: err.message().to_string(),
            range: usize::from(range.start())..usize::from(range.end()),
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
    pub fn has_diagnostics(&self) -> bool {
        !self.diagnostics.is_empty() || !self.parse_errors.is_empty()
    }
}
