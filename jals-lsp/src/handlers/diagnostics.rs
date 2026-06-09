//! Builds LSP diagnostics from parser syntax errors.

use async_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};

use crate::line_index::LineIndex;

/// Parse `text` and map each [`jals_syntax::SyntaxError`] to an LSP diagnostic.
pub(crate) fn compute_diagnostics(text: &str, line_index: &LineIndex) -> Vec<Diagnostic> {
    jals_syntax::parse(text)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_source_has_no_diagnostics() {
        let text = "class A {}\n";
        let diags = compute_diagnostics(text, &LineIndex::new(text));
        assert!(diags.is_empty());
    }

    #[test]
    fn syntax_error_becomes_diagnostic() {
        let text = "class A { void m( {}";
        let diags = compute_diagnostics(text, &LineIndex::new(text));
        assert!(!diags.is_empty());
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].source.as_deref(), Some("jals"));
        assert!(!diags[0].message.is_empty());
    }
}
