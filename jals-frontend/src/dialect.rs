//! The jals dialect frontend: desugars jals-specific constructs into plain Java source.
//!
//! Today it desugars grouped imports (`import java.util.{HashMap, ArrayList};`) into one plain
//! import per member. The rewrite is a byte splice over the original source: only each grouped
//! import's *significant span* (`import` keyword through its `;`) is replaced, and the replacement
//! reproduces the exact same number of `\n` as the span it replaces. Every other byte is copied
//! verbatim, so a runtime stack trace still names the line the author wrote — the whole point of
//! desugaring in the frontend rather than reformatting through `jals-fmt`.
//!
//! A parse error overlapping a grouped import is never desugared: plain imports synthesized from a
//! broken group would be a guess at what the author meant. The file is emitted verbatim — the
//! output stays one entry per input — together with an error diagnostic, which makes
//! [`Driver::lower`](crate::driver::Driver::lower) reject the whole lowering and publish nothing.
//! That is a deliberate fail-fast: `javac` never runs, so nothing downstream will report the syntax
//! error for us and the diagnostic has to carry the offending line itself.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use jals_storage::ContentDigest;
use jals_syntax::ast::{AstNode, ImportDecl, ImportGroup, SourceFile};
use jals_syntax::{Parse, SyntaxElement, SyntaxKind};

use crate::frontend::{Frontend, FrontendCaps, FrontendFuture};
use crate::ir::{FrontendDiagnostic, FrontendOutput, Ir, Severity};
use crate::level::IrLevel;

/// Which jals dialect desugarings this frontend applies.
///
/// A plain flag set — deliberately *not* `jals-config` types — so `jals-frontend` stays
/// config-free and `no_std`. The caller (which holds the manifest) projects the resolved feature
/// set onto these flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DialectFlags {
    /// Desugar grouped imports (`import a.b.{X, Y};`) into one plain import per member.
    pub grouped_imports: bool,
}

impl DialectFlags {
    /// Whether any dialect desugaring is enabled. When `false`, the dialect frontend is
    /// behaviourally identical to [`VanillaFrontend`](crate::VanillaFrontend), so callers can pick
    /// vanilla instead and keep the cache identity stable.
    pub const fn any(self) -> bool {
        self.grouped_imports
    }
}

/// Lowers jals dialect sources to plain Java sources per [`DialectFlags`].
#[derive(Debug, Clone, Copy)]
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
            // statement becomes several, but the set of imported names is unchanged.
            type_stable: true,
            version: 1,
        }
    }

    fn config_digest(&self) -> ContentDigest {
        // Fold every flag that affects output, so the driver's cache key changes when the enabled
        // dialect features change (`key.rs` folds this into the lowering/emitted provenance).
        ContentDigest::of(&[u8::from(self.flags.grouped_imports)])
    }

    fn run<'a>(&'a self, ir: Ir<'a>) -> FrontendFuture<'a> {
        Box::pin(async move {
            let mut files = Vec::with_capacity(ir.files().len());
            let mut diagnostics = Vec::new();
            for file in ir.files() {
                let verbatim = || (file.path.clone(), file.bytes.to_vec());
                if !self.flags.grouped_imports {
                    files.push(verbatim());
                    continue;
                }
                let Ok(text) = core::str::from_utf8(&file.bytes) else {
                    // Java sources are UTF-8; anything else we cannot parse, so leave it alone.
                    diagnostics.push(FrontendDiagnostic {
                        severity: Severity::Warning,
                        file: Some(file.path.clone()),
                        message: "source is not valid UTF-8; grouped imports not desugared"
                            .to_owned(),
                    });
                    files.push(verbatim());
                    continue;
                };
                match Self::desugar_grouped_imports(text).await {
                    Desugared::Unchanged => files.push(verbatim()),
                    Desugared::Rewritten(rewritten) => {
                        files.push((file.path.clone(), rewritten.into_bytes()));
                    }
                    Desugared::Malformed { line, detail } => {
                        // Never synthesize plain imports from a broken group. The file is emitted
                        // verbatim so the output keeps one entry per input, but the error makes the
                        // driver reject the whole lowering — `javac` never runs, so this message is
                        // the only report the user gets and has to locate the group itself.
                        diagnostics.push(FrontendDiagnostic {
                            severity: Severity::Error,
                            file: Some(file.path.clone()),
                            message: format!(
                                "grouped import on line {line} is malformed ({detail}); \
                                 not desugared"
                            ),
                        });
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
    /// No grouped imports: emit the input unchanged.
    Unchanged,
    /// Rewritten source bytes (grouped imports expanded).
    Rewritten(String),
    /// A parse error overlaps a grouped import: caller emits verbatim and diagnoses.
    ///
    /// Carries what the diagnostic needs to stand on its own, because the lowering it belongs to
    /// is rejected and no compiler downstream will restate it: the 1-based `line` of the offending
    /// group's `import` keyword, and `detail` — the parser's own message, or the missing `;`.
    Malformed { line: usize, detail: String },
}

impl DialectFrontend {
    async fn desugar_grouped_imports(text: &str) -> Desugared {
        let parse = Parse::parse(text).await;
        let Some(source_file) = SourceFile::cast(parse.syntax()) else {
            return Desugared::Unchanged;
        };
        // (span_start, span_end, replacement) for each grouped import.
        let mut splices: Vec<(usize, usize, String)> = Vec::new();
        for import in source_file.imports() {
            let Some(group) = import.group() else {
                continue;
            };
            let Some((start, end)) = Self::significant_span(&import) else {
                // No `;` at all, so there is no span to replace.
                return Desugared::Malformed {
                    line: Self::keyword_start(&import)
                        .map_or(1, |offset| Self::line_of(text, offset)),
                    detail: "missing `;`".to_owned(),
                };
            };
            // Only a parse error touching *this* group blocks desugaring — errors elsewhere in the
            // file (e.g. a method body) do not. Half-open overlap on `[start, end)`.
            if let Some(error) = parse.errors().iter().find(|error| {
                usize::from(error.range().start()) < end && start < usize::from(error.range().end())
            }) {
                return Desugared::Malformed {
                    line: Self::line_of(text, start),
                    detail: error.message().to_owned(),
                };
            }
            splices.push((
                start,
                end,
                Self::expand_group(&import, &group, &text[start..end]),
            ));
        }
        if splices.is_empty() {
            return Desugared::Unchanged;
        }
        splices.sort_by_key(|(start, _, _)| *start);
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0usize;
        for (start, end, replacement) in splices {
            out.push_str(&text[cursor..start]);
            out.push_str(&replacement);
            cursor = end;
        }
        out.push_str(&text[cursor..]);
        Desugared::Rewritten(out)
    }

    /// The 1-based line `offset` falls on. `offset` comes from a token range, so it is always a
    /// char boundary; `get` rather than an index keeps a defect from becoming a panic in the
    /// compile path.
    fn line_of(text: &str, offset: usize) -> usize {
        text.get(..offset)
            .map_or(1, |prefix| 1 + prefix.matches('\n').count())
    }

    /// The offset of the declaration's `import` keyword — the anchor a diagnostic about it points
    /// at, and the start of [`significant_span`](Self::significant_span). Present whenever the
    /// parser built the node at all, since it opens an `IMPORT_DECL` by consuming that keyword.
    fn keyword_start(import: &ImportDecl) -> Option<usize> {
        import
            .syntax()
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|token| token.kind() == SyntaxKind::IMPORT_KW)
            .map(|token| usize::from(token.text_range().start()))
    }

    /// The significant span of an import declaration: from the `import` keyword's start to the `;`'s
    /// end. Excludes the leading trivia rowan attaches inside the node (the previous line's
    /// newline), which is exactly what keeps line numbers stable.
    fn significant_span(import: &ImportDecl) -> Option<(usize, usize)> {
        let start = Self::keyword_start(import)?;
        let end = import
            .syntax()
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| token.kind() == SyntaxKind::SEMICOLON)
            .map(|token| usize::from(token.text_range().end()))
            .last()?;
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
