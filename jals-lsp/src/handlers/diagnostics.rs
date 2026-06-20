//! Builds LSP diagnostics from parser syntax errors and `jals-lint` rule findings.

use async_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString};
use jals_syntax::Parse;
use text_size::{TextRange, TextSize};

use crate::line_index::LineIndex;

/// Map each [`jals_syntax::SyntaxError`] in `parse` to an LSP diagnostic.
///
/// `parse` is the document's cached CST (so this never reparses); `text` is the source it was
/// built from, needed to convert byte ranges to UTF-16 positions.
pub(crate) fn compute_diagnostics(
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
) -> Vec<Diagnostic> {
    parse
        .errors()
        .iter()
        .map(|err| Diagnostic {
            range: line_index.range(text, err.range()),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("jals".to_string()),
            message: err.message().to_string(),
            ..Default::default()
        })
        .collect()
}

/// Run the enabled `jals-lint` rules over the cached CST in `parse` and map each finding to an
/// LSP diagnostic, tagged with its rule name (`code`) and the `jals` source.
///
/// Takes `&Parse` like the other handlers (and `compute_diagnostics`), materializing the syntax
/// tree internally. Parser errors are intentionally excluded — they are emitted by
/// [`compute_diagnostics`], the single source of syntax-error diagnostics, so the two never
/// duplicate each other.
pub(crate) fn compute_lint_diagnostics(
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
    config: &jals_lint::Config,
) -> Vec<Diagnostic> {
    jals_lint::lint_node(&parse.syntax(), config)
        .into_iter()
        .map(|finding| {
            let range = TextRange::new(
                TextSize::from(finding.range.start as u32),
                TextSize::from(finding.range.end as u32),
            );
            Diagnostic {
                range: line_index.range(text, range),
                severity: Some(lint_severity(finding.severity)),
                code: Some(NumberOrString::String(finding.rule.to_string())),
                source: Some("jals".to_string()),
                message: finding.message,
                ..Default::default()
            }
        })
        .collect()
}

/// Map a `jals-lint` severity to an LSP diagnostic severity. `Allow` rules are skipped inside
/// [`jals_lint::lint_node`], so they never reach here; map them alongside `Warn` defensively.
fn lint_severity(severity: jals_lint::Severity) -> DiagnosticSeverity {
    match severity {
        jals_lint::Severity::Error => DiagnosticSeverity::ERROR,
        jals_lint::Severity::Warn | jals_lint::Severity::Allow => DiagnosticSeverity::WARNING,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_source_has_no_diagnostics() {
        let text = "class A {}\n";
        let parse = jals_syntax::parse(text);
        let diags = compute_diagnostics(&parse, text, &LineIndex::new(text));
        assert!(diags.is_empty());
    }

    #[test]
    fn syntax_error_becomes_diagnostic() {
        let text = "class A { void m( {}";
        let parse = jals_syntax::parse(text);
        let diags = compute_diagnostics(&parse, text, &LineIndex::new(text));
        assert!(!diags.is_empty());
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].source.as_deref(), Some("jals"));
        assert!(!diags[0].message.is_empty());
    }

    #[test]
    fn wildcard_import_becomes_lint_warning() {
        let text = "import java.util.*;\nclass C {}\n";
        let parse = jals_syntax::parse(text);
        let diags = compute_lint_diagnostics(
            &parse,
            text,
            &LineIndex::new(text),
            &jals_lint::Config::default(),
        );
        let wildcard = diags
            .iter()
            .find(|d| d.code == Some(NumberOrString::String("wildcard-import".to_string())))
            .expect("a wildcard-import diagnostic");
        assert_eq!(wildcard.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(wildcard.source.as_deref(), Some("jals"));
    }

    #[test]
    fn clean_source_has_no_lint_diagnostics() {
        let text = "class C {}\n";
        let parse = jals_syntax::parse(text);
        let diags = compute_lint_diagnostics(
            &parse,
            text,
            &LineIndex::new(text),
            &jals_lint::Config::default(),
        );
        assert!(diags.is_empty());
    }
}
