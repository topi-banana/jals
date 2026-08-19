#![cfg_attr(not(test), no_std)]
//! A lint checker for JALS/Java source, driven by the `jals-syntax` CST.
//!
//! [`LintOutput::lint`] runs every enabled rule over a [`LintRequest`] and returns a [`LintOutput`]
//! of [`Diagnostic`]s; [`LintOutput::lint_source`] is the file-local shorthand that parses first.
//! It never panics: a source with syntax errors is still linted over the lossless CST.
//!
//! **This crate produces every semantic diagnostic.** An unresolvable type name is the
//! `cannot-resolve` rule here, not a separate pass in a consumer — so every diagnostic a host shows
//! has a rule name, a `jalslint.toml` key, and a configurable severity, and there is one place to
//! look for the analysis behind it.
//!
//! The parser's own errors are **not** in that output. They belong to the parse, so a caller reads
//! them from [`Parse::errors`](jals_syntax::Parse::errors) — the two halves are assembled into one
//! list, in one order, by `jals-editor`'s `FileDiagnostics`, which every host goes through.
//!
//! What a broken parse suppresses is decided **here**, from the [`Parse`] the request carries,
//! rather than by a caller editing the config it passes down — and identically whichever entry point
//! is used, since a policy that varied by entry point would be two rules free to drift.
//!
//! Each rule has a kebab-case name and lives in one `jalslint.toml` **section**, which is the
//! defect class it reports ([`Category`]); its built-in level is the value that section's schema
//! gives its key, and a `jalslint.toml` may set any rule to `allow` / `warn` / `error` and
//! configure whatever options the rule takes. [`RuleInfo::all`] enumerates the whole registry. Rules are
//! read-only and never modify the source.

extern crate alloc;

mod diagnostic;
mod rules;

use alloc::vec::Vec;
use core::cell::OnceCell;

use jals_config::lint::Config;
use jals_config::{Category, LintLevel};
use jals_hir::{FileAnalysis, FileSemantics};
use jals_syntax::cfg::CfgMap;
use jals_syntax::{Parse, SyntaxNode};

use rules::{Checker, FeatureGate, Finding};

pub use diagnostic::{Diagnostic, LintOutput};

/// One rule's identity, as the registry publishes it.
///
/// The linter's own table, readable from outside so a consumer — `jals lint --list`, the ledger
/// test in `jals-lint/tests/registry.rs`, a future config language server — can enumerate the
/// rules instead of restating them. `default_level` is derived by applying the rule's accessor to
/// [`Config::default`], so it is the schema's value and cannot disagree with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleInfo {
    /// The rule's kebab-case name, which is also its key inside `category`'s section.
    pub name: &'static str,
    /// The `jalslint.toml` section the rule is configured under.
    pub category: Category,
    /// The level the rule fires at when nothing configures it.
    pub default_level: LintLevel,
}

impl RuleInfo {
    /// Every rule the linter implements, in the order the sections are declared on the config.
    ///
    /// The registry is the answer to "what does jals lint check?", and it is data rather than
    /// prose, so `jals-lint/README.md`'s table and `jals-lint/MAPPING-rustc-clippy.md`'s ledger
    /// are both checkable against it.
    pub fn all() -> impl Iterator<Item = Self> {
        let defaults = Config::default();
        rules::RULES.iter().map(move |rule| Self {
            name: rule.name,
            category: rule.category,
            default_level: (rule.level)(&defaults),
        })
    }
}

/// Everything one lint run reads about the file, apart from the configuration.
///
/// The two optional fields are what a caller *can* supply rather than what it must: a `cfg` map
/// narrows the analysis to the enabled code, and a [`FileSemantics`] both widens it to cross-file
/// facts and saves recomputing the resolution the caller already holds. Construct with
/// [`new`](LintRequest::new) and fill in what applies:
///
/// ```
/// # use jals_lint::LintRequest;
/// # let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse("class C {}"));
/// # let cfg = jals_syntax::cfg::CfgMap::default();
/// let request = LintRequest {
///     cfg: Some(&cfg),
///     ..LintRequest::new(&parse)
/// };
/// # assert!(request.file.is_none());
/// ```
pub struct LintRequest<'a> {
    /// The parsed file. Its [`errors`](Parse::errors) decide what a broken tree suppresses.
    pub parse: &'a Parse,
    /// The file's `#[cfg(...)]` evaluation, from a host with the `attributes` dialect feature on.
    ///
    /// It applies twice: name resolution skips every disabled host, and any finding landing inside a
    /// disabled range is dropped — the analysis-side mirror of the compile frontend blanking it.
    /// The structural attribute errors surface under the fixed `cfg` rule.
    pub cfg: Option<&'a CfgMap>,
    /// The file bound to the project it is linted in: the caller's cached analysis and the project
    /// index in one value.
    ///
    /// One field and not two, because a caller that holds a resolution holds the index it was
    /// resolved alongside — every host reaching this seam builds both together. With `None` the
    /// project-aware rules report nothing and the rest resolve here, file-locally.
    ///
    /// **Its analysis must be the analysis of this `parse` under this `cfg`** —
    /// [`FileAnalysis::of_with_cfg`] for a `Some(cfg)`, [`FileAnalysis::of`] for a `None`. Nothing
    /// checks it, and an analysis computed under a different `cfg` would carry references the map
    /// then hides, so findings about them would be dropped rather than reported.
    pub file: Option<&'a FileSemantics<'a>>,
}

impl<'a> LintRequest<'a> {
    /// A request over `parse` alone: no project, no `cfg` evaluation, no cached analysis.
    pub const fn new(parse: &'a Parse) -> Self {
        Self {
            parse,
            cfg: None,
            file: None,
        }
    }
}

impl LintOutput {
    /// Lint `src` according to `config`.
    ///
    /// The file-local shorthand: it parses `src` and lints it with no project, so reference types
    /// resolve only by spelling and the project-aware rules report nothing. A caller holding a
    /// [`FileSemantics`] (the CLI over a multi-file run, the language server) builds a
    /// [`LintRequest`] and calls [`lint`](LintOutput::lint) instead.
    pub async fn lint_source(src: &str, config: &Config) -> Self {
        Self::lint(
            LintRequest::new(&jals_syntax::Parse::parse(src).await),
            config,
        )
        .await
    }

    /// Lint `request` according to `config`.
    ///
    /// Name resolution is computed at most once and shared across every resolution-based rule (or
    /// taken from [`LintRequest::file`] when the caller has it cached). What a project index
    /// adds, what a `cfg` map removes, and what a broken parse suppresses are all documented on
    /// [`LintRequest`]'s fields.
    pub async fn lint(request: LintRequest<'_>, config: &Config) -> Self {
        Self {
            diagnostics: Self::run_rules(request, config).await,
        }
    }

    /// The rule engine: every enabled rule over `request`, sorted by start offset.
    ///
    /// The file's analysis is shared across every rule that reads one and computed lazily, so a
    /// configuration that enables only syntactic rules never pays for it, and one that enables
    /// several analyses just once — unless the caller cached one already, which wins outright. The
    /// project binding is threaded only into [`Checker::Semantic`] rules, which is also what keeps
    /// a resolution-only rule from forcing the file's type inference.
    ///
    /// A broken parse is answered here rather than by a caller, in two steps: the project binding
    /// is withheld, which silences every [`Checker::Semantic`] rule (each reports nothing without
    /// one), and a rule marked [`needs_clean_parse`](rules::RuleMeta::needs_clean_parse) does not
    /// run at all, which covers the one rule that still reports file-locally. The caller's
    /// *analysis* is kept either way: it is the analysis of this tree, broken or not.
    ///
    /// The `cfg` map applies twice: resolution skips disabled hosts (so resolution-based rules
    /// never see a disabled definition), and a post-pass drops any finding whose range lands
    /// inside a disabled host (covering the syntactic rules, which walk the raw CST). Note the
    /// `attribute` gate rule and a non-empty `cfg` never coexist: a host produces a non-empty map
    /// only when the `attributes` dialect feature is on, and exactly then `config.features`
    /// permits the feature and the gate is silent.
    async fn run_rules(request: LintRequest<'_>, config: &Config) -> Vec<Diagnostic> {
        let LintRequest { parse, cfg, file } = request;
        let root = &parse.syntax();
        let clean = parse.errors().is_empty();
        // A half-parsed tree's types are recovery artefacts, so nothing may be concluded across the
        // project from it — but the caller's *analysis* is still the analysis of this tree, so the
        // resolution-based rules keep sharing it rather than resolving a second copy.
        let supplied = file.map(FileSemantics::analysis);
        let project = if clean { file } else { None };
        let analysis = OnceCell::new();
        let mut diagnostics = Vec::new();
        for rule in rules::RULES {
            let severity = (rule.level)(config);
            if severity == LintLevel::Allow || (rule.needs_clean_parse && !clean) {
                continue;
            }
            let findings = match rule.check {
                Checker::Syntactic(check) => check(root, config).await,
                Checker::Analyzed(check) => {
                    check(
                        Self::analysis_once(&analysis, supplied, root, cfg).await,
                        config,
                    )
                    .await
                }
                Checker::Semantic(check) => {
                    check(
                        Self::analysis_once(&analysis, supplied, root, cfg).await,
                        project,
                        config,
                    )
                    .await
                }
                // Run a feature-gated rule's detector only when the project's set does not permit
                // its guarded feature (`FeatureSet::permits` owns the empty-set exemption),
                // stamping the shared gate message on each node the detector located.
                Checker::Gated {
                    feature,
                    subject,
                    find,
                } => {
                    if config.features.permits(feature) {
                        Vec::new()
                    } else {
                        let message = FeatureGate::preview_message(feature, subject);
                        find(root)
                            .iter()
                            .map(|node| Finding::at_node(node, message.clone()))
                            .collect()
                    }
                }
            };
            for finding in findings {
                diagnostics.push(Diagnostic::new(rule, severity, finding));
            }
        }
        // Nothing inside a `cfg`-disabled host is reported: the code will not be compiled, so
        // findings there are noise. One geometric pass covers every rule at once.
        if let Some(cfg) = cfg {
            diagnostics.retain(|d| !cfg.is_disabled_span(d.range.start, d.range.end));
            // The structural attribute errors — the same set the compile frontend rejects a
            // build with — surface here under the fixed `cfg` rule, so every consumer (the CLI,
            // the editor) reports them at analysis time. Deliberately outside the per-rule
            // severity gate: a build-blocking error is not a configurable lint.
            for error in cfg.errors() {
                diagnostics.push(Diagnostic {
                    rule: "cfg",
                    severity: LintLevel::Error,
                    message: error.kind.message(),
                    range: usize::from(error.range.start())..usize::from(error.range.end()),
                    unnecessary: false,
                    unnecessary_range: None,
                });
            }
        }
        diagnostics.sort_by_key(|d| d.range.start);
        diagnostics
    }

    /// The shared file analysis, computed at most once per lint. The async-once shape (compute,
    /// then publish) is single-threaded, so at worst a re-entrant caller would compute twice —
    /// benign, since the analysis is pure.
    ///
    /// `supplied` is the caller's own cached analysis, which wins outright: the editor keeps one
    /// per open file, and resolving again here would do identical work a second time on every
    /// keystroke. It is the caller's obligation that it matches `root` and `cfg`
    /// ([`LintRequest::file`]); the cell is only the fallback for a caller that has none.
    async fn analysis_once<'c>(
        cell: &'c OnceCell<FileAnalysis>,
        supplied: Option<&'c FileAnalysis>,
        root: &SyntaxNode,
        cfg: Option<&CfgMap>,
    ) -> &'c FileAnalysis {
        if let Some(analysis) = supplied {
            return analysis;
        }
        if cell.get().is_none() {
            let computed = match cfg {
                Some(cfg) => FileAnalysis::of_with_cfg(root, cfg).await,
                None => FileAnalysis::of(root).await,
            };
            let _ = cell.set(computed);
        }
        cell.get().expect("the cell was just filled")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jals_exec::block_on_inline;

    #[test]
    fn rule_findings_are_reported() {
        // `import java.util.*;` is well-formed but trips the `wildcard-import` rule.
        let out = block_on_inline(LintOutput::lint_source(
            "import java.util.*;\nclass C {}\n",
            &Config::default(),
        ));
        assert!(
            out.diagnostics.iter().any(|d| d.rule == "wildcard-import"),
            "expected a wildcard-import finding: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn a_broken_parse_yields_rule_findings_only() {
        // Syntax errors belong to the parse. Linting a broken tree is still best-effort over the
        // lossless CST, and what comes back is rules only — the caller reads `Parse::errors`.
        let src = "import java.util.*;\nclass C { void m( {}\n";
        let parse = block_on_inline(jals_syntax::Parse::parse(src));
        assert!(!parse.errors().is_empty(), "the fixture must not parse");
        let out = block_on_inline(LintOutput::lint(
            LintRequest::new(&parse),
            &Config::default(),
        ));
        assert!(out.diagnostics.iter().any(|d| d.rule == "wildcard-import"));
        assert!(out.diagnostics.iter().all(|d| d.rule != "syntax-error"));
    }

    #[test]
    fn a_broken_parse_withholds_the_inference_rules() {
        // The suppression lives in the engine, not in a caller editing the config it passes down —
        // and it applies to `lint_source` too, which reaches the same driver.
        let src = "class C { int x = \"s\"; void m( {}\n";
        let parse = block_on_inline(jals_syntax::Parse::parse(src));
        assert!(!parse.errors().is_empty(), "the fixture must not parse");
        let index = block_on_inline(
            jals_hir::ProjectIndex::builder(&[(jals_hir::FileId(0), parse.syntax())]).build(),
        );
        let analysis = block_on_inline(jals_hir::FileAnalysis::of(&parse.syntax()));
        let semantics = analysis.in_project(&index, jals_hir::FileId(0));
        let out = block_on_inline(LintOutput::lint(
            LintRequest {
                file: Some(&semantics),
                ..LintRequest::new(&parse)
            },
            &Config::default(),
        ));
        assert!(
            out.diagnostics.iter().all(|d| d.rule != "type-mismatch"),
            "type inference over a recovered tree is noise: {:?}",
            out.diagnostics
        );
        // And the same fixture through the file-local entry point, which cannot be handed an index
        // at all: one policy, whichever way the engine is reached.
        assert!(
            block_on_inline(LintOutput::lint_source(src, &Config::default()))
                .diagnostics
                .iter()
                .all(|d| d.rule != "type-mismatch")
        );
    }

    #[test]
    fn a_broken_parse_keeps_the_resolution_rules() {
        // The other half of `needs_clean_parse`, and the half that is easy to get wrong: the
        // criterion is *findings derived from type inference*, not *reads `Resolved`*.
        // `unused-variables` and `constant-condition` both read it, and both keep reporting on a
        // recovered tree — a binding nothing refers to and a literal condition are not artefacts
        // of recovery.
        let src = "class C { void m() { int unused = 1; if (true) { a(); } } void n( {}\n";
        let parse = block_on_inline(jals_syntax::Parse::parse(src));
        assert!(!parse.errors().is_empty(), "the fixture must not parse");
        let out = block_on_inline(LintOutput::lint(
            LintRequest::new(&parse),
            &Config::default(),
        ));
        assert!(
            out.diagnostics.iter().any(|d| d.rule == "unused-variables"),
            "a binding nothing refers to survives recovery: {:?}",
            out.diagnostics
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.rule == "constant-condition"),
            "a literal condition survives recovery: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn a_broken_parse_keeps_the_callers_analysis() {
        // The one thing collapsing `index` + `resolved` into a single field could have lost: on a
        // broken parse the *project* is withheld, but the caller's analysis is still the analysis of
        // this tree and must go on being shared rather than resolved a second time.
        //
        // Observed the same way `a_supplied_resolution_is_the_one_the_rules_read` does — by handing
        // over an analysis of a *different* tree, where the binding is used. `unused-variables`
        // reads only the analysis, so if the request's is still consumed the rule falls silent; if
        // the engine resolved the broken tree itself, it would fire.
        let broken = block_on_inline(jals_syntax::Parse::parse(
            "class C { void m() { int a = 1; } void n( {}\n",
        ));
        assert!(!broken.errors().is_empty(), "the fixture must not parse");
        let used = block_on_inline(jals_syntax::Parse::parse(
            "class C { int m() { int a = 1; return a; } }",
        ));
        let used_analysis = block_on_inline(jals_hir::FileAnalysis::of(&used.syntax()));
        let used_index = block_on_inline(
            jals_hir::ProjectIndex::builder(&[(jals_hir::FileId(0), used.syntax())]).build(),
        );
        let used_semantics = used_analysis.in_project(&used_index, jals_hir::FileId(0));
        let cfg = Config::default();

        assert!(
            block_on_inline(LintOutput::lint(LintRequest::new(&broken), &cfg))
                .diagnostics
                .iter()
                .any(|d| d.rule == "unused-variables"),
            "the fixture must be unused when the engine analyses it itself"
        );
        let out = block_on_inline(LintOutput::lint(
            LintRequest {
                file: Some(&used_semantics),
                ..LintRequest::new(&broken)
            },
            &cfg,
        ));
        assert!(
            out.diagnostics.iter().all(|d| d.rule != "unused-variables"),
            "a broken parse withholds the project, not the caller's analysis: {:?}",
            out.diagnostics
        );
        // And the project really was withheld: the project-aware rules stay silent.
        assert!(
            out.diagnostics
                .iter()
                .all(|d| d.rule != "cannot-resolve" && d.rule != "unreported-exception"),
            "a broken tree's types are recovery artefacts: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn a_bare_request_matches_lint_source() {
        // A request with nothing filled in is exactly `lint_source` — the delegation must not drift.
        let src = "import java.util.*;\nclass C { int x = 1.0; }\n";
        let cfg = Config::default();
        let parse = block_on_inline(jals_syntax::Parse::parse(src));
        let bare = block_on_inline(LintOutput::lint(LintRequest::new(&parse), &cfg));
        let file_local = block_on_inline(LintOutput::lint_source(src, &cfg));
        assert_eq!(bare.diagnostics, file_local.diagnostics);
    }

    #[test]
    fn a_supplied_resolution_matches_computing_one() {
        // The caller's cached resolution is shared with every rule rather than recomputed; passing
        // it must not change a single finding.
        let src = "class C { void m() { int unused = 1; if (true) { a(); } else { b(); } } }";
        let cfg = Config::default();
        let parse = block_on_inline(jals_syntax::Parse::parse(src));
        let analysis = block_on_inline(jals_hir::FileAnalysis::of(&parse.syntax()));
        let index = block_on_inline(
            jals_hir::ProjectIndex::builder(&[(jals_hir::FileId(0), parse.syntax())]).build(),
        );
        let semantics = analysis.in_project(&index, jals_hir::FileId(0));
        let supplied = block_on_inline(LintOutput::lint(
            LintRequest {
                file: Some(&semantics),
                ..LintRequest::new(&parse)
            },
            &cfg,
        ));
        let computed = block_on_inline(LintOutput::lint(LintRequest::new(&parse), &cfg));
        assert_eq!(supplied.diagnostics, computed.diagnostics);
        assert!(!supplied.diagnostics.is_empty(), "the fixture must lint");
    }

    #[test]
    fn a_supplied_resolution_is_the_one_the_rules_read() {
        // Equivalence alone would also hold if the field were ignored, so this pins that it is
        // actually consumed: `unused-variables` reads only `Resolved` (never the tree), so
        // handing it a resolution where the binding *is* used silences it over a tree where it is
        // not.
        //
        // Deliberately breaking the documented "same parse" precondition is the whole point — it is
        // the only way to tell a shared resolution from a recomputed one from the outside.
        let unused = block_on_inline(jals_syntax::Parse::parse(
            "class C { void m() { int a = 1; } }",
        ));
        let used = block_on_inline(jals_syntax::Parse::parse(
            "class C { int m() { int a = 1; return a; } }",
        ));
        // The analysis of the *other* tree, bound to an index over it.
        let used_analysis = block_on_inline(jals_hir::FileAnalysis::of(&used.syntax()));
        let used_index = block_on_inline(
            jals_hir::ProjectIndex::builder(&[(jals_hir::FileId(0), used.syntax())]).build(),
        );
        let used_semantics = used_analysis.in_project(&used_index, jals_hir::FileId(0));
        let cfg = Config::default();

        assert!(
            block_on_inline(LintOutput::lint(LintRequest::new(&unused), &cfg))
                .diagnostics
                .iter()
                .any(|d| d.rule == "unused-variables"),
            "the fixture must be unused when the engine resolves it itself"
        );
        assert!(
            block_on_inline(LintOutput::lint(
                LintRequest {
                    file: Some(&used_semantics),
                    ..LintRequest::new(&unused)
                },
                &cfg,
            ))
            .diagnostics
            .iter()
            .all(|d| d.rule != "unused-variables"),
            "the supplied resolution must be the one the rules read"
        );
    }

    #[test]
    fn an_index_catches_project_subtyping() {
        // `Base` is not assignable to `Sub`. Reference subtyping resolves only against a project
        // index, so the file-local `lint_source` cannot see this, but a request carrying one can.
        // A field initializer keeps `unused-variables` out of the way, isolating `type-mismatch`.
        let src = "class Base {} class Sub extends Base {} class C { Sub f = new Base(); }";
        let cfg = Config::default();
        let parse = block_on_inline(jals_syntax::Parse::parse(src));

        // File-local: the subtyping mismatch is invisible.
        assert!(
            block_on_inline(LintOutput::lint_source(src, &cfg))
                .diagnostics
                .iter()
                .all(|d| d.rule != "type-mismatch")
        );

        // Index-aware: it is flagged.
        let index = block_on_inline(
            jals_hir::ProjectIndex::builder(&[(jals_hir::FileId(0), parse.syntax())]).build(),
        );
        let analysis = block_on_inline(jals_hir::FileAnalysis::of(&parse.syntax()));
        let semantics = analysis.in_project(&index, jals_hir::FileId(0));
        let out = block_on_inline(LintOutput::lint(
            LintRequest {
                file: Some(&semantics),
                ..LintRequest::new(&parse)
            },
            &cfg,
        ));
        assert!(
            out.diagnostics.iter().any(|d| d.rule == "type-mismatch"
                && d.message.contains("Base")
                && d.message.contains("Sub")),
            "expected an index-aware type-mismatch: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn constant_condition_carries_the_dead_branch_range() {
        let src = "class C { void m() { if (true) { a(); } else { b(); } } }";
        let out = block_on_inline(LintOutput::lint_source(src, &Config::default()));
        let constant = out
            .diagnostics
            .iter()
            .find(|d| d.rule == "constant-condition")
            .expect("a constant-condition diagnostic");
        let else_start = src.find("{ b(); }").unwrap();
        assert_eq!(
            constant.unnecessary_range,
            Some((
                else_start..else_start + "{ b(); }".len(),
                "this code is never executed".to_owned()
            ))
        );
        // Every other rule leaves the secondary range empty.
        let out = block_on_inline(LintOutput::lint_source(
            "import java.util.*;\nclass C {}\n",
            &Config::default(),
        ));
        let wildcard = out
            .diagnostics
            .iter()
            .find(|d| d.rule == "wildcard-import")
            .expect("a wildcard-import diagnostic");
        assert_eq!(wildcard.unnecessary_range, None);
    }

    #[test]
    fn unreported_exception_needs_the_index() {
        // A checked exception thrown but not declared. Classifying it as checked and finding it
        // undeclared needs the project index (with stdlib), so `lint_source` cannot see it.
        let src = "class MyEx extends Exception {} class C { void f() { throw new MyEx(); } }";
        let cfg = Config::default();
        let parse = block_on_inline(jals_syntax::Parse::parse(src));

        // File-local: nothing to report without the hierarchy.
        assert!(
            block_on_inline(LintOutput::lint_source(src, &cfg))
                .diagnostics
                .iter()
                .all(|d| d.rule != "unreported-exception")
        );

        // Index-aware (with stdlib): the undeclared checked exception is flagged.
        let index = block_on_inline(
            jals_hir::ProjectIndex::builder(&[(jals_hir::FileId(0), parse.syntax())])
                .with_stdlib()
                .build(),
        );
        let analysis = block_on_inline(jals_hir::FileAnalysis::of(&parse.syntax()));
        let semantics = analysis.in_project(&index, jals_hir::FileId(0));
        let out = block_on_inline(LintOutput::lint(
            LintRequest {
                file: Some(&semantics),
                ..LintRequest::new(&parse)
            },
            &cfg,
        ));
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.rule == "unreported-exception" && d.message.contains("MyEx")),
            "expected an unreported-exception finding: {:?}",
            out.diagnostics
        );
    }

    /// Lint `src` as file 0 of a single-file project index, which is what `cannot-resolve` needs.
    fn indexed(src: &str, config: &Config) -> Vec<Diagnostic> {
        block_on_inline(async {
            let parse = jals_syntax::Parse::parse(src).await;
            let index = jals_hir::ProjectIndex::builder(&[(jals_hir::FileId(0), parse.syntax())])
                .build()
                .await;
            let analysis = jals_hir::FileAnalysis::of(&parse.syntax()).await;
            let semantics = analysis.in_project(&index, jals_hir::FileId(0));
            LintOutput::lint(
                LintRequest {
                    file: Some(&semantics),
                    ..LintRequest::new(&parse)
                },
                config,
            )
            .await
            .diagnostics
        })
    }

    #[test]
    fn cannot_resolve_needs_the_index() {
        // `Nope` is nameable from nowhere; `String` is `java.lang` (external, never reported);
        // `Helper` resolves file-locally. Only `Nope` is a finding, and only with an index.
        let src = "package a; class Bar { Nope n; String s; Helper h; } class Helper {}";
        let cfg = Config::default();

        // File-local: whether a name is nameable from nowhere is a project-wide question.
        assert!(
            block_on_inline(LintOutput::lint_source(src, &cfg))
                .diagnostics
                .iter()
                .all(|d| d.rule != "cannot-resolve")
        );

        let found: Vec<_> = indexed(src, &cfg)
            .into_iter()
            .filter(|d| d.rule == "cannot-resolve")
            .collect();
        assert_eq!(found.len(), 1, "only `Nope`: {found:?}");
        assert_eq!(found[0].message, "cannot resolve symbol `Nope`");
        // An unresolvable name is not a style question, so it is an error by default — the one rule
        // here that is.
        assert_eq!(found[0].severity, LintLevel::Error);
    }

    #[test]
    fn cannot_resolve_is_configurable() {
        // The point of moving it into the rule engine: it has a `jalslint.toml` key like any other
        // rule, so a project that indexes only part of its sources can turn it off.
        let src = "package a; class Bar { Nope n; }";
        let mut config = Config::default();
        config.correctness.cannot_resolve.level = LintLevel::Allow;
        assert!(
            indexed(src, &config)
                .iter()
                .all(|d| d.rule != "cannot-resolve"),
            "`allow` must suppress it"
        );

        let mut config = Config::default();
        config.correctness.cannot_resolve.level = LintLevel::Warn;
        let found: Vec<_> = indexed(src, &config)
            .into_iter()
            .filter(|d| d.rule == "cannot-resolve")
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].severity, LintLevel::Warn);
    }

    #[test]
    fn cannot_resolve_is_withheld_from_a_broken_parse() {
        // The driver withholds the index from a broken tree, which silences every index-aware rule
        // without naming one — the property `Checker::Indexed` rules must keep.
        let src = "package a; class Bar { Nope n; void m( {}\n";
        let diagnostics = indexed(src, &Config::default());
        assert!(
            diagnostics.iter().all(|d| d.rule != "cannot-resolve"),
            "{diagnostics:?}"
        );
    }
}
