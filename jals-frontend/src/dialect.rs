//! The jals dialect frontend: desugars jals-specific constructs into plain Java source.
//!
//! Today it desugars grouped imports (`import java.util.{HashMap, ArrayList};`) into one plain
//! import per member, and applies jals attributes (`#[cfg(feature = "x")]`): every attribute's
//! text is stripped from the output (`javac` must never see `#[`), and a host whose `cfg`
//! predicate evaluates false against the resolved build features is blanked wholesale. Each file
//! is parsed once; both features' rewrites merge into one splice pass over the original bytes.
//!
//! The rewrite is a byte splice over the original source: a grouped import's *significant span*
//! (`import` keyword through its `;`) is replaced by an expansion reproducing the exact same
//! number of `\n`, and an attribute strip blanks its span *length-preservingly* (`\n`/`\r` kept,
//! everything else a space). Every other byte is copied verbatim, so a runtime stack trace still
//! names the line the author wrote — the whole point of desugaring in the frontend rather than
//! reformatting through `jals-fmt`.
//!
//! A structural error — a parse error overlapping a construct this pass would rewrite, an unknown
//! attribute, a malformed `cfg` — is never best-effort desugared. The file is emitted verbatim —
//! the output stays one entry per input — together with error diagnostics, which make
//! [`Driver::lower`](crate::driver::Driver::lower) reject the whole lowering and publish nothing.
//! That is a deliberate fail-fast: `javac` never runs, so nothing downstream will report the
//! problem for us and the diagnostics have to carry it themselves.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use jals_storage::ContentDigest;
use jals_syntax::ast::{AstNode, ImportDecl, ImportGroup, SourceFile};
use jals_syntax::{Parse, SyntaxElement, SyntaxKind};

use crate::attr::AttrPlan;
use crate::frontend::{Frontend, FrontendCaps, FrontendFuture};
use crate::ir::{FrontendDiagnostic, FrontendOutput, Ir, Severity};
use crate::level::IrLevel;

/// Which jals dialect desugarings this frontend applies.
///
/// A plain flag set — deliberately *not* `jals-config` types — so `jals-frontend` stays
/// config-free and `no_std`. The caller (which holds the manifest) projects the resolved feature
/// set onto these flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialectFlags {
    /// Desugar grouped imports (`import a.b.{X, Y};`) into one plain import per member.
    pub grouped_imports: bool,
    /// Strip jals attributes (`#[...]`) and apply `#[cfg(...)]` conditional compilation.
    pub attributes: bool,
    /// The resolved build features `#[cfg(feature = "...")]` tests. Populated by the caller only
    /// when `attributes` is on; a name absent here is simply false (Cargo/Rust cfg semantics).
    pub build_features: BTreeSet<String>,
}

impl DialectFlags {
    /// Whether any dialect desugaring is enabled. When `false`, the dialect frontend is
    /// behaviourally identical to [`VanillaFrontend`](crate::VanillaFrontend), so callers can pick
    /// vanilla instead and keep the cache identity stable.
    pub const fn any(&self) -> bool {
        self.grouped_imports || self.attributes
    }
}

/// Lowers jals dialect sources to plain Java sources per [`DialectFlags`].
#[derive(Debug, Clone)]
pub struct DialectFrontend {
    flags: DialectFlags,
}

impl DialectFrontend {
    pub const ID: &'static str = "jals-dialect";

    pub const fn new(flags: DialectFlags) -> Self {
        Self { flags }
    }
}

impl Frontend for DialectFrontend {
    fn caps(&self) -> FrontendCaps {
        FrontendCaps {
            id: Self::ID,
            needs: IrLevel::Bytes,
            extensions: &["java"],
            // Grouped-import expansion is per-file and introduces no new types — one import
            // statement becomes several, but the set of imported names is unchanged. A false
            // `cfg`, however, removes whole declarations (types included) from the output.
            type_stable: !self.flags.attributes,
            version: 1,
        }
    }

    fn config_digest(&self) -> ContentDigest {
        // Fold every flag that affects output, so the driver's cache key changes when the enabled
        // dialect features change (`key.rs` folds this into the lowering/emitted provenance). The
        // build-feature fold is unambiguous: each name is valid UTF-8, so the 0xFF terminator can
        // never occur inside one, and the `BTreeSet` iterates in one canonical order.
        let mut bytes = alloc::vec![
            u8::from(self.flags.grouped_imports),
            u8::from(self.flags.attributes),
        ];
        for feature in &self.flags.build_features {
            bytes.extend_from_slice(feature.as_bytes());
            bytes.push(0xFF);
        }
        ContentDigest::of(&bytes)
    }

    fn run<'a>(&'a self, ir: Ir<'a>) -> FrontendFuture<'a> {
        Box::pin(async move {
            let mut files = Vec::with_capacity(ir.files().len());
            let mut diagnostics = Vec::new();
            for file in ir.files() {
                let verbatim = || (file.path.clone(), file.bytes.to_vec());
                if !self.flags.any() {
                    files.push(verbatim());
                    continue;
                }
                let Ok(text) = core::str::from_utf8(&file.bytes) else {
                    // Java sources are UTF-8; anything else we cannot parse, so leave it alone.
                    diagnostics.push(FrontendDiagnostic {
                        severity: Severity::Warning,
                        file: Some(file.path.clone()),
                        message: "source is not valid UTF-8; jals dialect constructs not desugared"
                            .to_owned(),
                    });
                    files.push(verbatim());
                    continue;
                };
                match Self::desugar_file(text, &self.flags).await {
                    Desugared::Unchanged => files.push(verbatim()),
                    Desugared::Rewritten(rewritten) => {
                        files.push((file.path.clone(), rewritten.into_bytes()));
                    }
                    Desugared::Failed(errors) => {
                        // Emit verbatim so `javac` would report any real syntax error too; the
                        // error diagnostics make the driver reject the lowering regardless.
                        diagnostics.extend(errors.into_iter().map(|message| FrontendDiagnostic {
                            severity: Severity::Error,
                            file: Some(file.path.clone()),
                            message,
                        }));
                        files.push(verbatim());
                    }
                }
            }
            Ok(FrontendOutput {
                files,
                diagnostics,
                // Line numbers are preserved by same-line emission, so an explicit origin map
                // would be redundant. (Column offsets shift, but no consumer reads origins yet.)
                origins: Vec::new(),
            })
        })
    }

    fn describe(&self, ir: &Ir<'_>) -> String {
        format!(
            "desugar jals dialect in {} source file(s)",
            ir.files().len()
        )
    }
}

/// The outcome of desugaring one file.
enum Desugared {
    /// No dialect construct present: emit the input unchanged.
    Unchanged,
    /// Rewritten source bytes.
    Rewritten(String),
    /// Structural errors: caller emits verbatim and diagnoses each message.
    Failed(Vec<String>),
}

/// One planned byte-range rewrite.
enum Splice {
    /// Blank the span length-preservingly (an attribute strip / a `cfg`-disabled host).
    Blank { semicolon: bool },
    /// Replace the span with the text (a grouped-import expansion; same `\n` count).
    Replace(String),
}

impl DialectFrontend {
    /// Desugar one file: parse once, plan both features' rewrites, splice them in one pass.
    /// Any structural error fails the whole file instead of best-effort rewriting.
    async fn desugar_file(text: &str, flags: &DialectFlags) -> Desugared {
        let parse = Parse::parse(text).await;
        let Some(source_file) = SourceFile::cast(parse.syntax()) else {
            return Desugared::Unchanged;
        };
        let attr_plan = if flags.attributes {
            AttrPlan::compute(&parse, text, &flags.build_features)
        } else {
            AttrPlan::default()
        };
        let mut errors = attr_plan.errors.clone();
        // (start, end, rewrite) for each planned splice; construction keeps them disjoint (an
        // import's significant span starts at its `import` keyword, *after* any attribute, and
        // splices inside a `cfg`-disabled host are dropped).
        let mut splices: Vec<(usize, usize, Splice)> = Vec::new();
        if flags.grouped_imports {
            for import in source_file.imports() {
                let Some(group) = import.group() else {
                    continue;
                };
                // A malformed group is never desugared: plain imports synthesized from a broken
                // group would be a guess at what the author meant. The message names the group's
                // line and the parser's own detail, because the rejected lowering means `javac`
                // never runs and nothing downstream restates the problem.
                let malformed = |line: usize, detail: &str| {
                    format!("grouped import on line {line} is malformed ({detail}); not desugared")
                };
                let Some((start, end)) = Self::import_span(&import) else {
                    // No `;` at all, so there is no span to replace.
                    let line = Self::keyword_start(&import)
                        .map_or(1, |offset| AttrPlan::line_of(text, offset));
                    errors.push(malformed(line, "missing `;`"));
                    continue;
                };
                // A `cfg`-disabled import is blanked wholesale; its group must not expand.
                if attr_plan.disables((start, end)) {
                    continue;
                }
                // Only a parse error touching *this* group blocks desugaring — errors elsewhere
                // in the file (e.g. a method body) do not. Half-open overlap on `[start, end)`.
                if let Some(error) = parse.errors().iter().find(|error| {
                    usize::from(error.range().start()) < end
                        && start < usize::from(error.range().end())
                }) {
                    errors.push(malformed(AttrPlan::line_of(text, start), error.message()));
                    continue;
                }
                splices.push((
                    start,
                    end,
                    Splice::Replace(Self::expand_group(&import, &group, &text[start..end])),
                ));
            }
        }
        if !errors.is_empty() {
            return Desugared::Failed(errors);
        }
        for blank in attr_plan.blanks {
            splices.push((
                blank.start,
                blank.end,
                Splice::Blank {
                    semicolon: blank.semicolon,
                },
            ));
        }
        if splices.is_empty() {
            return Desugared::Unchanged;
        }
        splices.sort_by_key(|(start, _, _)| *start);
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0usize;
        for (start, end, splice) in splices {
            debug_assert!(cursor <= start, "splices must be disjoint");
            out.push_str(&text[cursor..start]);
            match splice {
                Splice::Replace(replacement) => out.push_str(&replacement),
                Splice::Blank { semicolon } => {
                    Self::blank_onto(&mut out, &text[start..end], semicolon);
                }
            }
            cursor = end;
        }
        out.push_str(&text[cursor..]);
        Desugared::Rewritten(out)
    }

    /// Blank `span` onto `out`, length-preservingly: `\n` and `\r` are kept (line numbers *and*
    /// the byte length are invariant, so other splice offsets never shift), every other byte
    /// becomes a space (a multi-byte char becomes a run of spaces — same byte count, still valid
    /// UTF-8). With `semicolon`, the first byte becomes `;` instead — a span always starts at a
    /// significant ASCII token, never at a newline.
    fn blank_onto(out: &mut String, span: &str, semicolon: bool) {
        let mut bytes = span.bytes();
        if semicolon && bytes.next().is_some() {
            out.push(';');
        }
        for byte in bytes {
            out.push(match byte {
                b'\n' => '\n',
                b'\r' => '\r',
                _ => ' ',
            });
        }
    }

    /// The offset of the declaration's `import` keyword — the anchor a diagnostic about it points
    /// at, and the start of [`import_span`](Self::import_span). Present whenever the parser built
    /// the declaration's keyword at all.
    fn keyword_start(import: &ImportDecl) -> Option<usize> {
        import
            .syntax()
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|token| token.kind() == SyntaxKind::IMPORT_KW)
            .map(|token| usize::from(token.text_range().start()))
    }

    /// The significant span of an import declaration: from the `import` keyword's start to the
    /// `;`'s end. Excludes both the leading trivia rowan attaches inside the node (the previous
    /// line's newline) and any leading jals attributes (blanked separately), which is exactly
    /// what keeps line numbers stable and the splices disjoint.
    fn import_span(import: &ImportDecl) -> Option<(usize, usize)> {
        let start = Self::keyword_start(import)?;
        let end = import
            .syntax()
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| token.kind() == SyntaxKind::SEMICOLON)
            .last()
            .map(|token| usize::from(token.text_range().end()))?;
        Some((start, end))
    }

    /// Expand one grouped import into `import`-per-member text with the *same* newline count as
    /// `span_text` (the significant span it replaces).
    ///
    /// Placement of the emitted imports is free: every byte after the span is copied verbatim, so a
    /// following line's number depends only on the *total* newline count in the replacement, not on
    /// where those newlines sit. So the statements are joined with single spaces and the original
    /// newline count is appended at the end. An empty group (`import a.{};`) joins to `""` and
    /// collapses to just those newlines.
    fn expand_group(import: &ImportDecl, group: &ImportGroup, span_text: &str) -> String {
        let newlines = span_text.matches('\n').count();
        let Some(prefix) = import.name().map(|name| name.text()) else {
            // A group with no prefix name is malformed and would be caught by the error check; keep
            // the original bytes as a defensive fallback.
            return span_text.to_owned();
        };
        let static_kw = if import.is_static() { "static " } else { "" };
        let statements: Vec<String> = group
            .members()
            .map(|member| format!("import {static_kw}{prefix}.{};", member.text()))
            .collect();
        let mut out = statements.join(" ");
        for _ in 0..newlines {
            out.push('\n');
        }
        out
    }
}
