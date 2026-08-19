//! The result of linting: the rule findings, and nothing else.
//!
//! Every *semantic* diagnostic is a rule finding, `cannot-resolve` included — so a consumer reads
//! one list, and each entry names the rule that produced it and the `jalslint.toml` key that
//! configures it.
//!
//! Syntax errors are the one exception, and they are deliberately absent. They belong to the parse,
//! not to the rules, and a caller that wants them reads
//! [`Parse::errors`](jals_syntax::Parse::errors) — which is what `jals-editor`'s diagnostics
//! assembly does. Restating them here as a `syntax-error` rule was a second
//! [`SyntaxError`](jals_syntax::SyntaxError)-to-diagnostic conversion that no consumer outside this
//! crate's own tests ever read.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use crate::rules::{Finding, RuleMeta};

/// What a rule does when it fires, re-exported from the shared config crate. It *is* the per-rule
/// configuration value ([`jalslint.toml`](jals_config::lint::Config)): a rule set to
/// [`Allow`](LintLevel::Allow) is disabled and never runs.
pub use jals_config::LintLevel;

/// A single lint diagnostic: a rule firing at a byte range in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The name of the rule that produced this diagnostic (kebab-case, e.g. `wildcard-import`).
    /// It is also the rule's key inside its `jalslint.toml` section, so a reported finding names
    /// what silences it.
    /// A rule's [`Category`](jals_config::Category) — the section it is configured under — is
    /// deliberately **not** carried here. No destination draws it, and a copy on every diagnostic
    /// would be a second place the section lives; a host that wants it looks the rule name up in
    /// [`RuleInfo::all`](crate::RuleInfo::all), which is the registry that already answers.
    pub rule: &'static str,
    /// The resolved level for this diagnostic. Never [`Allow`](LintLevel::Allow): the engine skips
    /// a rule set to it.
    pub severity: LintLevel,
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
    /// Build a diagnostic from a rule's [`Finding`], stamping it with the rule's identity and the
    /// level resolved from configuration.
    pub(crate) fn new(rule: &RuleMeta, severity: LintLevel, finding: Finding) -> Self {
        Self {
            rule: rule.name,
            severity,
            message: finding.message,
            range: finding.range,
            unnecessary: finding.unnecessary,
            unnecessary_range: finding.unnecessary_range,
        }
    }
}

/// The output of [`LintOutput::lint_source`](crate::LintOutput::lint_source).
#[derive(Debug, Clone)]
pub struct LintOutput {
    /// Diagnostics produced by the enabled rules, sorted by start offset.
    pub diagnostics: Vec<Diagnostic>,
}
