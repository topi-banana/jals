//! Builds LSP diagnostics from parser syntax errors, `jals-lint` rule findings, and unresolved
//! cross-file type references.

use async_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, DiagnosticTag, NumberOrString};
use jals_hir::{FileId, ProjectIndex, Resolved};
use jals_syntax::Parse;

use crate::line_index::LineIndex;

/// Map each [`jals_syntax::SyntaxError`] in `parse` to an LSP diagnostic.
///
/// `parse` is the document's cached CST (so this never reparses); `text` is the source it was
/// built from, needed to convert byte ranges to UTF-16 positions.
pub fn compute_diagnostics(parse: &Parse, text: &str, line_index: &LineIndex) -> Vec<Diagnostic> {
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
///
/// Two finding shapes render as faded code (the [`DiagnosticTag::UNNECESSARY`] tag): a finding
/// marked [`unnecessary`](jals_lint::Diagnostic::unnecessary) (e.g. `unused-local`) carries the
/// tag on its own diagnostic, and a finding with an
/// [`unnecessary_range`](jals_lint::Diagnostic::unnecessary_range) (the dead branch of a constant
/// `if`) additionally emits a hint diagnostic covering that range, with the message the rule
/// supplied for it.
pub fn compute_lint_diagnostics(
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
    config: &jals_config::lint::Config,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for finding in jals_lint::lint_node(&parse.syntax(), config) {
        let code = NumberOrString::String(finding.rule.to_string());
        // A secondary unnecessary range renders as a hint (kept out of the problems list) tagged
        // `Unnecessary` so the editor fades it.
        let hint = finding
            .unnecessary_range
            .map(|(range, message)| Diagnostic {
                range: line_index.byte_range(text, &range),
                severity: Some(DiagnosticSeverity::HINT),
                code: Some(code.clone()),
                source: Some("jals".to_string()),
                message,
                tags: Some(vec![DiagnosticTag::UNNECESSARY]),
                ..Default::default()
            });
        out.push(Diagnostic {
            range: line_index.byte_range(text, &finding.range),
            severity: Some(lint_severity(finding.severity)),
            code: Some(code),
            source: Some("jals".to_string()),
            message: finding.message,
            // The finding names code that is itself unnecessary, so the editor fades it in place.
            tags: finding
                .unnecessary
                .then(|| vec![DiagnosticTag::UNNECESSARY]),
            ..Default::default()
        });
        out.extend(hint);
    }
    out
}

/// Build "cannot resolve symbol" diagnostics for `file`'s type-name references that resolve to
/// nothing — neither file-locally nor anywhere in the project index.
///
/// `parse`/`text`/`line_index` are the document's cached CST and its coordinate map; `resolved` is
/// its file-local name resolution (shared with [`compute_type_mismatch_diagnostics`] so the tree is
/// resolved once per publish); `index` is the project-wide symbol index and `file` this document's
/// id within it. Diagnostics are suppressed entirely when the document has parse errors: a broken
/// tree yields spurious unresolved names, and the syntax errors themselves are already reported by
/// [`compute_diagnostics`].
pub fn compute_type_diagnostics(
    index: &ProjectIndex,
    file: FileId,
    parse: &Parse,
    resolved: &Resolved,
    text: &str,
    line_index: &LineIndex,
) -> Vec<Diagnostic> {
    if !parse.errors().is_empty() {
        return Vec::new();
    }
    index
        .unresolved_types(file, resolved)
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

/// Build index-aware type-mismatch diagnostics for `file`: a variable initializer or simple `=`
/// assignment whose value type is not assignable to its slot, resolving reference types against the
/// project `index` (so a `Sub`/`Base` confusion is caught, which the file-local `type-mismatch` lint
/// rule cannot see).
///
/// This is the project-wide counterpart of the `jals-lint` `type-mismatch` rule, sharing the same
/// `jals_hir::type_mismatches` core and the same config key — so the server suppresses the
/// file-local rule when it runs this, and the user's `type-mismatch` severity (`allow` to disable,
/// `error` to escalate) governs both. `resolved` is the document's file-local name resolution,
/// shared with [`compute_type_diagnostics`]. Suppressed on parse errors, like
/// [`compute_type_diagnostics`].
pub fn compute_type_mismatch_diagnostics(
    index: &ProjectIndex,
    file: FileId,
    parse: &Parse,
    resolved: &Resolved,
    text: &str,
    line_index: &LineIndex,
    config: &jals_config::lint::Config,
) -> Vec<Diagnostic> {
    let severity = config.severity(jals_lint::TYPE_MISMATCH_RULE, jals_config::Severity::Warn);
    if severity == jals_config::Severity::Allow || !parse.errors().is_empty() {
        return Vec::new();
    }
    jals_hir::type_mismatches(&parse.syntax(), resolved, Some((index, file)))
        .into_iter()
        .map(|m| Diagnostic {
            range: line_index.byte_range(text, &m.range),
            severity: Some(lint_severity(severity)),
            code: Some(NumberOrString::String(
                jals_lint::TYPE_MISMATCH_RULE.to_string(),
            )),
            source: Some("jals".to_string()),
            message: m.message(),
            ..Default::default()
        })
        .collect()
}

/// Map a `jals-lint` severity to an LSP diagnostic severity. `Allow` rules are skipped inside
/// [`jals_lint::lint_node`], so they never reach here; map them alongside `Warn` defensively.
const fn lint_severity(severity: jals_config::Severity) -> DiagnosticSeverity {
    match severity {
        jals_config::Severity::Error => DiagnosticSeverity::ERROR,
        jals_config::Severity::Warn | jals_config::Severity::Allow => DiagnosticSeverity::WARNING,
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
            &jals_config::lint::Config::default(),
        );
        let wildcard = diags
            .iter()
            .find(|d| d.code == Some(NumberOrString::String("wildcard-import".to_string())))
            .expect("a wildcard-import diagnostic");
        assert_eq!(wildcard.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(wildcard.source.as_deref(), Some("jals"));
    }

    #[test]
    fn compact_source_file_flagged_when_edition_is_java24() {
        // A top-level `main` is a preview feature before Java 25; the host injects the project's
        // edition as `target_java_version`, and the rule reports an ERROR for Java 24.
        let text = "void main() {}\n";
        let parse = jals_syntax::parse(text);
        let mut config = jals_config::lint::Config {
            target_java_version: Some(24),
            ..Default::default()
        };
        let diags = compute_lint_diagnostics(&parse, text, &LineIndex::new(text), &config);
        let d = diags
            .iter()
            .find(|d| d.code == Some(NumberOrString::String("compact-source-file".to_string())))
            .expect("a compact-source-file diagnostic");
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.source.as_deref(), Some("jals"));

        // Java 25 (or no edition) allows the syntax: nothing is reported.
        config.target_java_version = Some(25);
        assert!(
            compute_lint_diagnostics(&parse, text, &LineIndex::new(text), &config)
                .iter()
                .all(|d| d.code != Some(NumberOrString::String("compact-source-file".to_string())))
        );
    }

    /// Lint diagnostics for `text` under the default config, filtered to `rule`.
    fn lint_diags_for_rule(text: &str, rule: &str) -> Vec<Diagnostic> {
        let parse = jals_syntax::parse(text);
        let diags = compute_lint_diagnostics(
            &parse,
            text,
            &LineIndex::new(text),
            &jals_config::lint::Config::default(),
        );
        let code = Some(NumberOrString::String(rule.to_string()));
        diags.into_iter().filter(|d| d.code == code).collect()
    }

    #[test]
    fn constant_condition_fades_the_dead_branch() {
        let text = "class C { void m() { if (true) { a(); } else { b(); } } }\n";
        let line_index = LineIndex::new(text);
        let constant = lint_diags_for_rule(text, "constant-condition");
        assert_eq!(
            constant.len(),
            2,
            "warning + dead-branch hint: {constant:?}"
        );

        let warning = &constant[0];
        assert_eq!(warning.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(warning.source.as_deref(), Some("jals"));
        assert_eq!(warning.tags, None);
        let cond = text.find("true").unwrap();
        assert_eq!(
            warning.range,
            line_index.byte_range(text, &(cond..cond + "true".len()))
        );

        let hint = &constant[1];
        assert_eq!(hint.severity, Some(DiagnosticSeverity::HINT));
        assert_eq!(hint.tags, Some(vec![DiagnosticTag::UNNECESSARY]));
        assert_eq!(hint.message, "this code is never executed");
        let dead = text.find("{ b(); }").unwrap();
        assert_eq!(
            hint.range,
            line_index.byte_range(text, &(dead..dead + "{ b(); }".len()))
        );
    }

    #[test]
    fn always_true_without_else_emits_no_hint() {
        let text = "class C { void m() { if (true) { a(); } } }\n";
        let constant = lint_diags_for_rule(text, "constant-condition");
        assert_eq!(constant.len(), 1, "the warning only: {constant:?}");
        assert_eq!(constant[0].severity, Some(DiagnosticSeverity::WARNING));
    }

    #[test]
    fn unused_local_is_tagged_unnecessary_in_place() {
        let text = "class C { void m() { int unused = 1; } }\n";
        let unused = lint_diags_for_rule(text, "unused-local");
        assert_eq!(unused.len(), 1, "one tagged warning, no extra diagnostic");
        assert_eq!(unused[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(unused[0].tags, Some(vec![DiagnosticTag::UNNECESSARY]));
    }

    #[test]
    fn clean_source_has_no_lint_diagnostics() {
        let text = "class C {}\n";
        let parse = jals_syntax::parse(text);
        let diags = compute_lint_diagnostics(
            &parse,
            text,
            &LineIndex::new(text),
            &jals_config::lint::Config::default(),
        );
        assert!(diags.is_empty());
    }

    /// Build a two-file project index: `file0` is `text`, `file1` declares `package a; class Foo`.
    fn index_with_sibling_foo(text: &str) -> (ProjectIndex, Parse) {
        let parse = jals_syntax::parse(text);
        let sibling = jals_syntax::parse("package a; class Foo { }");
        let index =
            ProjectIndex::builder(&[(FileId(0), parse.syntax()), (FileId(1), sibling.syntax())])
                .build();
        (index, parse)
    }

    #[test]
    fn type_diagnostics_flag_only_genuine_unknowns() {
        // `Nope` is nameable from nowhere; `String` is java.lang; `Foo` is a same-package project
        // type. Only `Nope` is reported.
        let text = "package a; class Bar { Nope n; String s; Foo f; }";
        let (index, parse) = index_with_sibling_foo(text);
        let resolved = jals_hir::resolve_node(&parse.syntax());
        let diags = compute_type_diagnostics(
            &index,
            FileId(0),
            &parse,
            &resolved,
            text,
            &LineIndex::new(text),
        );
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
        let index = ProjectIndex::builder(&[(FileId(0), parse.syntax())]).build();
        let resolved = jals_hir::resolve_node(&parse.syntax());
        let diags = compute_type_diagnostics(
            &index,
            FileId(0),
            &parse,
            &resolved,
            text,
            &LineIndex::new(text),
        );
        assert!(diags.is_empty());
    }

    /// A single-file index with `Base`, `Sub extends Base`, and a `Sub s = new Base();` slot.
    const SUBTYPING_SRC: &str =
        "class Base {} class Sub extends Base {} class C { void m() { Sub s = new Base(); } }";

    #[test]
    fn type_mismatch_diagnostics_flag_project_subtyping() {
        let parse = jals_syntax::parse(SUBTYPING_SRC);
        let index = ProjectIndex::builder(&[(FileId(0), parse.syntax())]).build();
        let resolved = jals_hir::resolve_node(&parse.syntax());
        let diags = compute_type_mismatch_diagnostics(
            &index,
            FileId(0),
            &parse,
            &resolved,
            SUBTYPING_SRC,
            &LineIndex::new(SUBTYPING_SRC),
            &jals_config::lint::Config::default(),
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("type-mismatch".to_string()))
        );
        assert_eq!(diags[0].source.as_deref(), Some("jals"));
        assert!(diags[0].message.contains("Base") && diags[0].message.contains("Sub"));
    }

    #[test]
    fn type_mismatch_diagnostics_flag_a_bad_call_argument() {
        let text = "class C { void f(int x) {} void g() { f(1.0); } }";
        let parse = jals_syntax::parse(text);
        let index = ProjectIndex::builder(&[(FileId(0), parse.syntax())]).build();
        let resolved = jals_hir::resolve_node(&parse.syntax());
        let diags = compute_type_mismatch_diagnostics(
            &index,
            FileId(0),
            &parse,
            &resolved,
            text,
            &LineIndex::new(text),
            &jals_config::lint::Config::default(),
        );
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("double") && diags[0].message.contains("int"));
    }

    #[test]
    fn type_mismatch_diagnostics_respect_allow_config() {
        let parse = jals_syntax::parse(SUBTYPING_SRC);
        let index = ProjectIndex::builder(&[(FileId(0), parse.syntax())]).build();
        let mut config = jals_config::lint::Config::default();
        config
            .rules
            .insert("type-mismatch".to_string(), jals_config::Severity::Allow);
        let resolved = jals_hir::resolve_node(&parse.syntax());
        let diags = compute_type_mismatch_diagnostics(
            &index,
            FileId(0),
            &parse,
            &resolved,
            SUBTYPING_SRC,
            &LineIndex::new(SUBTYPING_SRC),
            &config,
        );
        assert!(diags.is_empty());
    }
}
