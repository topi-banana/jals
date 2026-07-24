//! jals attribute stripping and `cfg` conditional compilation.
//!
//! The evaluation itself — host classification, predicate evaluation, structural errors — lives
//! in [`jals_syntax::cfg::CfgMap`], shared with the analysis side (HIR, lint, the editor).
//! [`AttrPlan::compute`] translates that map into this frontend's rewrite plan: blank the
//! attribute text of every enabled host (`javac` must never see `#[`), blank the whole span of a
//! `cfg`-disabled host, and render each structural error with the 1-based line it sits on.
//! Blanking is length-preserving, so every other byte offset in the file — and every line
//! number — stays exactly where the author put it.
//!
//! Structural errors are collected as messages; the caller emits them as error diagnostics and
//! publishes nothing. Content *inside* a disabled host is neither validated nor evaluated (Rust
//! parity).

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use jals_syntax::cfg::CfgMap;
use jals_syntax::{Parse, SyntaxKind, SyntaxNode};

/// A byte span to blank in place (length-preserving).
pub(crate) struct Blank {
    pub start: usize,
    pub end: usize,
    /// Write a `;` as the first blanked byte: a stripped *statement* must stay a statement so a
    /// sole control-structure body (`if (c) #[cfg(x)] f();`) remains valid Java.
    pub semicolon: bool,
}

/// What the attribute pass contributes to the rewrite of one file.
#[derive(Default)]
pub(crate) struct AttrPlan {
    /// Spans to blank in place, mutually disjoint.
    pub blanks: Vec<Blank>,
    /// Host spans removed by a false `cfg`. Rewrites planned by other passes (a grouped-import
    /// expansion) inside one of these ranges must be dropped.
    pub disabled: Vec<(usize, usize)>,
    /// Structural errors; any entry makes the file fail instead of rewriting.
    pub errors: Vec<String>,
}

impl AttrPlan {
    /// Whether `span` lies inside a disabled range.
    pub(crate) fn disables(&self, span: (usize, usize)) -> bool {
        self.disabled
            .iter()
            .any(|&(start, end)| start <= span.0 && span.1 <= end)
    }

    /// The 1-based line `offset` falls on. `offset` comes from a token range, so it is always a
    /// char boundary; `get` rather than an index keeps a defect from becoming a panic in the
    /// compile path.
    pub(crate) fn line_of(text: &str, offset: usize) -> usize {
        text.get(..offset)
            .map_or(1, |prefix| 1 + prefix.matches('\n').count())
    }

    /// Compute the attribute rewrite plan for one parsed file. `text` is the parsed source, used
    /// to name the offending line in error messages — the lowering they belong to is rejected, so
    /// no compiler downstream will restate them and each must locate its construct on its own.
    pub(crate) fn compute(parse: &Parse, text: &str, features: &BTreeSet<String>) -> Self {
        let cfg = CfgMap::compute(parse, features);
        let mut out = Self::default();
        for span in cfg.attr_spans() {
            out.blanks.push(Blank {
                start: span.start().into(),
                end: span.end().into(),
                semicolon: false,
            });
        }
        for host in cfg.disabled_hosts() {
            let (start, end) = (
                usize::from(host.range.start()),
                usize::from(host.range.end()),
            );
            out.blanks.push(Blank {
                start,
                end,
                semicolon: Self::needs_semicolon(&host.host),
            });
            out.disabled.push((start, end));
        }
        for error in cfg.errors() {
            let line = Self::line_of(text, error.range.start().into());
            out.errors.push(error.kind.render_with_line(line));
        }
        out
    }

    /// Whether stripping this host must leave a `;` behind: as the sole body of a control
    /// structure (or a labeled statement's target), removing the statement entirely would leave
    /// the structure without a body.
    fn needs_semicolon(host: &SyntaxNode) -> bool {
        use SyntaxKind as S;
        matches!(
            host.parent().map(|p| p.kind()),
            Some(
                S::IF_STMT
                    | S::WHILE_STMT
                    | S::DO_WHILE_STMT
                    | S::FOR_STMT
                    | S::FOR_EACH_STMT
                    | S::LABELED_STMT
            )
        )
    }
}
