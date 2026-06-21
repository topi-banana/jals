//! Builds LSP diagnostics from parser syntax errors, `jals-lint` rule findings, and unresolved
//! cross-file type references.

use async_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString};
use jals_hir::{FileId, ProjectIndex};
use jals_syntax::Parse;

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
        .map(|finding| Diagnostic {
            range: line_index.byte_range(text, &finding.range),
            severity: Some(lint_severity(finding.severity)),
            code: Some(NumberOrString::String(finding.rule.to_string())),
            source: Some("jals".to_string()),
            message: finding.message,
            ..Default::default()
        })
        .collect()
}

/// Build "cannot resolve symbol" diagnostics for `file`'s type-name references that resolve to
/// nothing — neither file-locally nor anywhere in the project index.
///
/// `parse`/`text`/`line_index` are the document's cached CST and its coordinate map; `index` is the
/// project-wide symbol index and `file` this document's id within it. Diagnostics are suppressed
/// entirely when the document has parse errors: a broken tree yields spurious unresolved names, and
/// the syntax errors themselves are already reported by [`compute_diagnostics`].
pub(crate) fn compute_type_diagnostics(
    index: &ProjectIndex,
    file: FileId,
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
) -> Vec<Diagnostic> {
    if !parse.errors().is_empty() {
        return Vec::new();
    }
    let resolved = jals_hir::resolve_node(&parse.syntax());
    index
        .unresolved_types(file, &resolved)
        .into_iter()
        .map(|range| Diagnostic {
            range: line_index.byte_range(text, &range),
            severity: Some(DiagnosticSeverity::ERROR),
            code: Some(NumberOrString::String("cannot-resolve".to_string())),
            source: Some("jals".to_string()),
            message: format!("cannot resolve symbol `{}`", &text[range]),
            ..Default::default()
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

    /// Build a two-file project index: `file0` is `text`, `file1` declares `package a; class Foo`.
    fn index_with_sibling_foo(text: &str) -> (ProjectIndex, Parse) {
        let parse = jals_syntax::parse(text);
        let sibling = jals_syntax::parse("package a; class Foo { }");
        let index =
            ProjectIndex::build(&[(FileId(0), parse.syntax()), (FileId(1), sibling.syntax())]);
        (index, parse)
    }

    #[test]
    fn type_diagnostics_flag_only_genuine_unknowns() {
        // `Nope` is nameable from nowhere; `String` is java.lang; `Foo` is a same-package project
        // type. Only `Nope` is reported.
        let text = "package a; class Bar { Nope n; String s; Foo f; }";
        let (index, parse) = index_with_sibling_foo(text);
        let diags =
            compute_type_diagnostics(&index, FileId(0), &parse, text, &LineIndex::new(text));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "cannot resolve symbol `Nope`");
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("cannot-resolve".to_string()))
        );
        assert_eq!(diags[0].source.as_deref(), Some("jals"));
    }

    #[test]
    fn type_diagnostics_suppressed_on_parse_errors() {
        // A broken tree yields spurious unresolved names, so the whole pass is skipped.
        let text = "package a; class Bar { Nope n; ";
        let parse = jals_syntax::parse(text);
        assert!(!parse.errors().is_empty());
        let index = ProjectIndex::build(&[(FileId(0), parse.syntax())]);
        let diags =
            compute_type_diagnostics(&index, FileId(0), &parse, text, &LineIndex::new(text));
        assert!(diags.is_empty());
    }
}
