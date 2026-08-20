//! The canonical, protocol-neutral diagnostics assembly for one file.
//!
//! Every editor host used to sequence its own passes (syntax errors, lint rules, cross-file
//! resolution) with subtly different ordering and suppression. The one policy lives here now;
//! hosts only map each [`FileDiagnostic`] to their protocol's shape (LSP `Diagnostic`, Monaco
//! marker).
//!
//! The policy, in order:
//! 1. **Syntax errors** — always reported, as [`DiagnosticSeverity::Error`] with no code. They
//!    belong to the parse rather than to any rule, which is why they are the one thing assembled
//!    here rather than produced by `jals-lint`.
//! 2. **`cfg`-disabled regions** — each as a faded hint. Not a finding: nothing is wrong with code
//!    the current feature selection excludes, so this is a rendering of inactive code and belongs to
//!    presentation.
//! 3. **Lint rules** — one pass through the `jals-lint` rule engine, which produces **every**
//!    semantic diagnostic, `cannot-resolve` included. What a project index adds and what a broken
//!    parse withholds are decided in that engine, from the [`Parse`] it is handed; this module does
//!    not edit the configuration it passes down, and names no rule.
//!
//! A finding's secondary unnecessary range (the dead branch of a constant `if`) is flattened into
//! its own hint here, because a protocol carries one range per diagnostic.
//!
//! The result is stably sorted by `(range.start, code)` so hosts and tests see one deterministic
//! order.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_config::DiagnosticSeverity;
use jals_config::lint::Config;
use jals_hir::FileSemantics;
use jals_syntax::Parse;
use jals_syntax::cfg::CfgMap;

/// One diagnostic over one file, in byte coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileDiagnostic {
    /// The byte range the diagnostic covers.
    pub range: Range<usize>,
    /// How to present it.
    pub severity: DiagnosticSeverity,
    /// The producing rule (`wildcard-import`, `type-mismatch`, `cannot-resolve`, …); `None` for a
    /// syntax error, which belongs to the parse and so has no rule to name.
    pub code: Option<&'static str>,
    /// Human-readable message.
    pub message: String,
    /// Whether `range` covers code that is itself unnecessary (an unused local, a dead branch) —
    /// hosts that can render faded code should do so (LSP's `Unnecessary` tag).
    pub unnecessary: bool,
}

pub use api::assemble;

/// Assembles the canonical diagnostics for one file.
mod api {
    use super::{
        CfgMap, Config, DiagnosticSeverity, FileDiagnostic, FileSemantics, Parse, ToOwned, Vec,
    };

    /// Assemble `parse`'s diagnostics under `config` (which already carries the project's
    /// resolved feature set), threading the project `index` into the rule engine.
    ///
    /// `file` is the caller's cached analysis bound to the project index — passed straight
    /// through, so every rule shares one analysis and one type inference instead of the engine
    /// building a second copy of what the caller already keeps. Its analysis must be the analysis
    /// of this `parse` under this `cfg` (see [`jals_lint::LintRequest::file`]); `None` lets the
    /// engine analyse, file-locally.
    ///
    /// `cfg`, when present, is the file's `#[cfg(...)]` evaluation: lint findings inside a
    /// disabled host are suppressed, each disabled range is reported as an `unnecessary` hint
    /// (hosts that render faded code grey it out), and every structural attribute error — the
    /// same set the compile frontend rejects a build with — surfaces as an error diagnostic at
    /// edit time.
    pub async fn assemble(
        parse: &Parse,
        file: Option<&FileSemantics<'_>>,
        config: &Config,
        cfg: Option<&CfgMap>,
    ) -> Vec<FileDiagnostic> {
        // 1. Syntax errors.
        let mut out: Vec<FileDiagnostic> = parse
            .errors()
            .iter()
            .map(|err| FileDiagnostic {
                range: crate::byte_range(err.range()),
                severity: DiagnosticSeverity::Error,
                code: None,
                message: err.message().to_owned(),
                unnecessary: false,
            })
            .collect();

        // 2. Each `cfg`-disabled region as a faded-code hint (the structural attribute errors
        // come out of the lint engine below, under the fixed `cfg` rule).
        if let Some(cfg) = cfg {
            for range in cfg.disabled_ranges() {
                out.push(FileDiagnostic {
                    range: crate::byte_range(range),
                    severity: DiagnosticSeverity::Hint,
                    code: Some("cfg"),
                    message: "disabled by `cfg` under the current feature selection".to_owned(),
                    unnecessary: true,
                });
            }
        }

        // 3. Every semantic diagnostic, from one engine pass. The request carries the parse, so a
        // broken tree is the engine's decision under its own rule table — this module neither edits
        // the configuration nor names a rule, so the two cannot disagree.
        let findings = jals_lint::LintOutput::lint(
            jals_lint::LintRequest {
                cfg,
                file,
                ..jals_lint::LintRequest::new(parse)
            },
            config,
        )
        .await
        .diagnostics;
        for finding in findings {
            out.push(FileDiagnostic {
                range: finding.range,
                severity: finding.severity.into(),
                code: Some(finding.rule),
                message: finding.message,
                unnecessary: finding.unnecessary,
            });
            // A secondary unnecessary range (the dead branch of a constant `if`) becomes its own
            // hint, kept out of the problems list but faded by hosts that support it.
            if let Some((range, message)) = finding.unnecessary_range {
                out.push(FileDiagnostic {
                    range,
                    severity: DiagnosticSeverity::Hint,
                    code: Some(finding.rule),
                    message,
                    unnecessary: true,
                });
            }
        }

        out.sort_by(|a, b| (a.range.start, a.code).cmp(&(b.range.start, b.code)));
        out
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;
    use jals_hir::FileId;
    use jals_hir::ProjectIndex;

    use super::*;

    /// Assemble diagnostics for `text` under the default config, with no project index.
    fn assemble_local(text: &str) -> Vec<FileDiagnostic> {
        block_on_inline(async {
            api::assemble(
                &jals_syntax::Parse::parse(text).await,
                None,
                &Config::default(),
                None,
            )
            .await
        })
    }

    /// Assemble diagnostics for `text` as file 0 of a single-file, stdlib-folded project.
    fn assemble_indexed(text: &str, config: &Config) -> Vec<FileDiagnostic> {
        block_on_inline(async {
            let parse = jals_syntax::Parse::parse(text).await;
            let index = ProjectIndex::builder(&[(FileId(0), parse.syntax())])
                .with_stdlib()
                .build()
                .await;
            let analysis = jals_hir::FileAnalysis::of(&parse.syntax()).await;
            let semantics = analysis.in_project(&index, FileId(0));
            api::assemble(&parse, Some(&semantics), config, None).await
        })
    }

    /// The diagnostics with `code == rule`.
    fn with_code<'a>(diags: &'a [FileDiagnostic], rule: &str) -> Vec<&'a FileDiagnostic> {
        diags
            .iter()
            .filter(|d| d.code == Some(rule) || (rule.is_empty() && d.code.is_none()))
            .collect()
    }

    #[test]
    fn clean_source_has_no_diagnostics() {
        assert!(assemble_local("class A {}\n").is_empty());
    }

    #[test]
    fn syntax_error_becomes_an_uncoded_error() {
        let diags = assemble_local("class A { void m( {}");
        assert!(!diags.is_empty());
        assert_eq!(diags[0].severity, DiagnosticSeverity::Error);
        assert_eq!(diags[0].code, None);
        assert!(!diags[0].message.is_empty());
    }

    #[test]
    fn wildcard_import_becomes_a_lint_warning() {
        let diags = assemble_local("import java.util.*;\nclass C {}\n");
        let wildcard = with_code(&diags, "wildcard-import");
        assert_eq!(wildcard.len(), 1);
        assert_eq!(wildcard[0].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn feature_gated_rule_reads_the_injected_feature_set() {
        // A top-level `main` is a preview feature before Java 25; the caller injects the
        // project's resolved feature set as `config.features`.
        block_on_inline(async {
            let text = "void main() {}\n";
            let mut config = Config::default().with_features(jals_config::FeatureSet::resolve(&[
                jals_config::Feature::Java24,
            ]));
            let parse = jals_syntax::Parse::parse(text).await;
            let diags = api::assemble(&parse, None, &config, None).await;
            let gated = with_code(&diags, "compact-source-file");
            assert_eq!(gated.len(), 1);
            assert_eq!(gated[0].severity, DiagnosticSeverity::Error);

            // A `java25` set (or no features at all) allows the syntax: nothing is reported.
            config.features = jals_config::FeatureSet::resolve(&[jals_config::Feature::Java25]);
            let diags = api::assemble(&parse, None, &config, None).await;
            assert!(with_code(&diags, "compact-source-file").is_empty());
        });
    }

    #[test]
    fn constant_condition_fades_the_dead_branch() {
        let text = "class C { void m() { if (true) { a(); } else { b(); } } }\n";
        let diags = assemble_local(text);
        let constant = with_code(&diags, "constant-condition");
        assert_eq!(
            constant.len(),
            2,
            "warning + dead-branch hint: {constant:?}"
        );

        let warning = constant[0];
        assert_eq!(warning.severity, DiagnosticSeverity::Warning);
        assert!(!warning.unnecessary);
        let cond = text.find("true").unwrap();
        assert_eq!(warning.range, cond..cond + "true".len());

        let hint = constant[1];
        assert_eq!(hint.severity, DiagnosticSeverity::Hint);
        assert!(hint.unnecessary);
        assert_eq!(hint.message, "this code is never executed");
        let dead = text.find("{ b(); }").unwrap();
        assert_eq!(hint.range, dead..dead + "{ b(); }".len());
    }

    #[test]
    fn always_true_without_else_emits_no_hint() {
        let diags = assemble_local("class C { void m() { if (true) { a(); } } }\n");
        let constant = with_code(&diags, "constant-condition");
        assert_eq!(constant.len(), 1, "the warning only: {constant:?}");
        assert_eq!(constant[0].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn unused_local_is_unnecessary_in_place() {
        let diags = assemble_local("class C { void m() { int unused = 1; } }\n");
        let unused = with_code(&diags, "unused-variables");
        assert_eq!(unused.len(), 1, "one flagged warning, no extra diagnostic");
        assert_eq!(unused[0].severity, DiagnosticSeverity::Warning);
        assert!(unused[0].unnecessary);
    }

    #[test]
    fn unresolved_types_flag_only_genuine_unknowns() {
        block_on_inline(async {
            // `Nope` is nameable from nowhere; `String` is java.lang; `Foo` is a same-package
            // project type. Only `Nope` is reported.
            let text = "package a; class Bar { Nope n; String s; Foo f; }";
            let parse = jals_syntax::Parse::parse(text).await;
            let sibling = jals_syntax::Parse::parse("package a; class Foo { }").await;
            let index = ProjectIndex::builder(&[
                (FileId(0), parse.syntax()),
                (FileId(1), sibling.syntax()),
            ])
            .with_stdlib()
            .build()
            .await;
            let analysis = jals_hir::FileAnalysis::of(&parse.syntax()).await;
            let semantics = analysis.in_project(&index, FileId(0));
            let diags = api::assemble(&parse, Some(&semantics), &Config::default(), None).await;
            let unresolved = with_code(&diags, "cannot-resolve");
            assert_eq!(unresolved.len(), 1);
            assert_eq!(unresolved[0].message, "cannot resolve symbol `Nope`");
            assert_eq!(unresolved[0].severity, DiagnosticSeverity::Error);
        });
    }

    #[test]
    fn resolution_passes_are_suppressed_on_parse_errors() {
        // A broken tree yields spurious unknowns and type noise: only the syntax errors (and any
        // purely syntactic lint findings) survive; `cannot-resolve` and `type-mismatch` are
        // silenced everywhere — indexed or not.
        let text = "package a; class Bar { Nope n; int x = \"s\"; ";
        let diags = assemble_indexed(text, &Config::default());
        assert!(diags.iter().any(|d| d.code.is_none()), "syntax errors kept");
        assert!(with_code(&diags, "cannot-resolve").is_empty());
        assert!(with_code(&diags, "type-mismatch").is_empty());
    }

    /// A single-file project with `Base`, `Sub extends Base`, and a `Sub s = new Base();` slot.
    const SUBTYPING_SRC: &str =
        "class Base {} class Sub extends Base {} class C { void m() { Sub s = new Base(); } }";

    #[test]
    fn type_mismatch_runs_through_the_engine_with_the_index() {
        let diags = assemble_indexed(SUBTYPING_SRC, &Config::default());
        let mismatch = with_code(&diags, "type-mismatch");
        assert_eq!(mismatch.len(), 1, "one report, never doubled: {diags:?}");
        assert_eq!(mismatch[0].severity, DiagnosticSeverity::Warning);
        assert!(mismatch[0].message.contains("Base") && mismatch[0].message.contains("Sub"));
    }

    #[test]
    fn type_mismatch_respects_allow_config() {
        let mut config = Config::default();
        config.correctness.type_mismatch.level = jals_config::LintLevel::Allow;
        assert!(with_code(&assemble_indexed(SUBTYPING_SRC, &config), "type-mismatch").is_empty());
    }

    #[test]
    fn type_mismatch_severity_override_escalates() {
        let mut config = Config::default();
        config.correctness.type_mismatch.level = jals_config::LintLevel::Error;
        let diags = assemble_indexed(SUBTYPING_SRC, &config);
        assert_eq!(
            with_code(&diags, "type-mismatch")[0].severity,
            DiagnosticSeverity::Error
        );
    }

    #[test]
    fn unreported_exception_is_index_aware() {
        // New unified spec: the whole rule engine sees the index, so `unreported-exception`
        // (classifying checked exceptions through the stdlib hierarchy) fires here too.
        let text = "class MyEx extends Exception {} class C { void f() { throw new MyEx(); } }";
        let diags = assemble_indexed(text, &Config::default());
        assert!(
            diags
                .iter()
                .any(|d| d.code == Some("unreported-exception") && d.message.contains("MyEx")),
            "expected an unreported-exception finding: {diags:?}"
        );
    }

    #[test]
    fn output_is_sorted_by_start_offset_then_code() {
        let text = "class C { Nope n; void m() { int unused = 1; if (true) { } } }";
        let diags = assemble_indexed(text, &Config::default());
        let keys: Vec<_> = diags.iter().map(|d| (d.range.start, d.code)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "deterministic order: {diags:?}");
        assert!(
            diags.len() >= 3,
            "cannot-resolve + unused-variables + constant-condition: {diags:?}"
        );
    }
}
