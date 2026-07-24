//! The canonical, protocol-neutral diagnostics assembly for one file.
//!
//! Every editor host used to sequence its own passes (syntax errors, lint rules, cross-file
//! resolution) with subtly different ordering and suppression. The one policy lives here now;
//! hosts only map each [`FileDiagnostic`] to their protocol's shape (LSP `Diagnostic`, Monaco
//! marker).
//!
//! The policy, in order:
//! 1. **Syntax errors** — always reported, as [`DiagnosticSeverity::Error`] with no code.
//! 2. **Lint rules** — one pass through the `jals-lint` rule engine. On a clean parse the
//!    project index (when any) is threaded in, so the index-aware rules (`type-mismatch`,
//!    `unreported-exception`) check cross-file facts under their configured severities. On a
//!    broken parse the engine runs file-locally with `type-mismatch` forced off — a broken tree
//!    yields spurious type noise, never worth reporting.
//! 3. **Unresolved types** — `cannot resolve symbol` errors from the project index, only on a
//!    clean parse.
//!
//! The result is stably sorted by `(range.start, code)` so hosts and tests see one deterministic
//! order.

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ops::Range;

use jals_config::lint::Config;
use jals_hir::{FileId, ProjectIndex, Resolved};
use jals_syntax::Parse;
use jals_syntax::cfg::CfgMap;

/// How a host should present a [`FileDiagnostic`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    /// A definite problem (syntax error, unresolvable symbol, error-severity rule).
    Error,
    /// A warn-severity lint finding.
    Warning,
    /// Supplementary information kept out of the problems list — today only the faded
    /// dead-branch range of a constant condition.
    Hint,
}

impl DiagnosticSeverity {
    /// The presentation severity for a lint finding's configured severity. `Allow` rules are
    /// skipped inside the engine and never reach here; map them alongside `Warn` defensively.
    const fn of_lint(severity: jals_config::Severity) -> Self {
        match severity {
            jals_config::Severity::Error => Self::Error,
            jals_config::Severity::Warn | jals_config::Severity::Allow => Self::Warning,
        }
    }
}

/// One diagnostic over one file, in byte coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileDiagnostic {
    /// The byte range the diagnostic covers.
    pub range: Range<usize>,
    /// How to present it.
    pub severity: DiagnosticSeverity,
    /// The producing rule (`wildcard-import`, `type-mismatch`, …) or pass (`cannot-resolve`);
    /// `None` for a syntax error.
    pub code: Option<&'static str>,
    /// Human-readable message.
    pub message: String,
    /// Whether `range` covers code that is itself unnecessary (an unused local, a dead branch) —
    /// hosts that can render faded code should do so (LSP's `Unnecessary` tag).
    pub unnecessary: bool,
}

/// Assembles the canonical diagnostics for one file.
pub struct FileDiagnostics;

impl FileDiagnostics {
    /// Assemble `parse`'s diagnostics under `config` (which already carries the project's
    /// resolved feature set), threading the project `index` into the index-aware passes.
    ///
    /// `resolved` is the file's local name resolution when the caller has it cached; `None`
    /// resolves on demand (only needed for the unresolved-types pass).
    ///
    /// `cfg`, when present, is the file's `#[cfg(...)]` evaluation: lint findings inside a
    /// disabled host are suppressed, each disabled range is reported as an `unnecessary` hint
    /// (hosts that render faded code grey it out), and every structural attribute error — the
    /// same set the compile frontend rejects a build with — surfaces as an error diagnostic at
    /// edit time.
    pub async fn assemble(
        parse: &Parse,
        resolved: Option<&Resolved>,
        index: Option<(&ProjectIndex, FileId)>,
        config: &Config,
        cfg: Option<&CfgMap>,
    ) -> Vec<FileDiagnostic> {
        let root = parse.syntax();

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
        let clean_parse = out.is_empty();

        // 1b. Each `cfg`-disabled region as a faded-code hint (the structural attribute errors
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

        // 2. Lint rules — one engine pass. A broken tree runs file-locally with `type-mismatch`
        // forced off; a clean tree threads the index in, so the index-aware rules check
        // cross-file facts under the user's configured severities. The `cfg` map rides along on
        // both paths, suppressing findings inside disabled hosts.
        let findings = if clean_parse {
            jals_lint::LintOutput::lint_parse_with_index(parse, config, index, cfg)
                .await
                .diagnostics
        } else {
            let mut quiet = config.clone();
            quiet.rules.insert(
                jals_lint::TYPE_MISMATCH_RULE.to_owned(),
                jals_config::Severity::Allow,
            );
            jals_lint::LintOutput::lint_parse_with_index(parse, &quiet, None, cfg)
                .await
                .diagnostics
        };
        for finding in findings {
            out.push(FileDiagnostic {
                range: finding.range,
                severity: DiagnosticSeverity::of_lint(finding.severity),
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

        // 3. Unresolved type names, from the project index. Suppressed on a broken tree, whose
        // spurious unknowns would only echo the syntax errors already reported. Reuses the
        // caller's cached resolution, resolving on demand otherwise.
        if clean_parse && let Some((index, file)) = index {
            if let Some(resolved) = resolved {
                Self::push_unresolved(&mut out, &root, index, file, resolved).await;
            } else {
                // Match the caller-cached shape: with a `cfg` map, resolution skips disabled
                // hosts, so no `cannot-resolve` is reported for a name only disabled code uses.
                let resolved = match cfg {
                    Some(cfg) => Resolved::resolve_node_with_cfg(&root, cfg).await,
                    None => Resolved::resolve_node(&root).await,
                };
                Self::push_unresolved(&mut out, &root, index, file, &resolved).await;
            }
        }

        out.sort_by(|a, b| (a.range.start, a.code).cmp(&(b.range.start, b.code)));
        out
    }

    /// Append a `cannot resolve symbol` error for each of `file`'s type-name references that
    /// resolve to nothing — neither file-locally nor anywhere in the project index.
    async fn push_unresolved(
        out: &mut Vec<FileDiagnostic>,
        root: &jals_syntax::SyntaxNode,
        index: &ProjectIndex,
        file: FileId,
        resolved: &Resolved,
    ) {
        let text = root.text();
        for range in index.unresolved_types(file, resolved).await {
            let name = text.slice(Self::text_range(&range)).to_string();
            out.push(FileDiagnostic {
                range,
                severity: DiagnosticSeverity::Error,
                code: Some("cannot-resolve"),
                message: alloc::format!("cannot resolve symbol `{name}`"),
                unnecessary: false,
            });
        }
    }

    /// A byte range as a `text_size::TextRange` (for slicing the tree's text).
    fn text_range(range: &Range<usize>) -> text_size::TextRange {
        text_size::TextRange::new(
            crate::sat_text_size(range.start),
            crate::sat_text_size(range.end),
        )
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
            FileDiagnostics::assemble(
                &jals_syntax::Parse::parse(text).await,
                None,
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
            FileDiagnostics::assemble(&parse, None, Some((&index, FileId(0))), config, None).await
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
            let mut config = Config {
                features: jals_config::FeatureSet::resolve(&[jals_config::Feature::Java24]),
                ..Default::default()
            };
            let parse = jals_syntax::Parse::parse(text).await;
            let diags = FileDiagnostics::assemble(&parse, None, None, &config, None).await;
            let gated = with_code(&diags, "compact-source-file");
            assert_eq!(gated.len(), 1);
            assert_eq!(gated[0].severity, DiagnosticSeverity::Error);

            // A `java25` set (or no features at all) allows the syntax: nothing is reported.
            config.features = jals_config::FeatureSet::resolve(&[jals_config::Feature::Java25]);
            let diags = FileDiagnostics::assemble(&parse, None, None, &config, None).await;
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
        let unused = with_code(&diags, "unused-local");
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
            let diags = FileDiagnostics::assemble(
                &parse,
                None,
                Some((&index, FileId(0))),
                &Config::default(),
                None,
            )
            .await;
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
        config
            .rules
            .insert("type-mismatch".to_owned(), jals_config::Severity::Allow);
        assert!(with_code(&assemble_indexed(SUBTYPING_SRC, &config), "type-mismatch").is_empty());
    }

    #[test]
    fn type_mismatch_severity_override_escalates() {
        let mut config = Config::default();
        config
            .rules
            .insert("type-mismatch".to_owned(), jals_config::Severity::Error);
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
            "cannot-resolve + unused + constant-condition: {diags:?}"
        );
    }
}
