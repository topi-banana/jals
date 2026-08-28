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

use jals_syntax::cfg::{CfgMap, TestHost};
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
    /// The `#[test]` methods this lowering *keeps*, for the harness generator to call. Empty
    /// unless the lowering is for a test run — an ordinary build removes them instead.
    pub tests: Vec<TestHost>,
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
    ///
    /// `tests` says whether this lowering is for a test run. When it is not, every `#[test]`
    /// method is blanked exactly as a `cfg`-disabled host is — that is what keeps a test out of
    /// the classes an ordinary `jals build` produces, and it is the same rule Rust applies to
    /// `#[cfg(test)]`.
    pub(crate) fn compute(
        parse: &Parse,
        text: &str,
        features: &BTreeSet<String>,
        tests: bool,
    ) -> Self {
        let cfg = CfgMap::compute(parse, features);
        let mut out = Self::default();
        if tests {
            out.tests = cfg.tests().to_vec();
        } else {
            // Removed *before* the attribute pass below, so `disables` already answers for them:
            // `blanks` are mutually disjoint, and a dropped method's own span already covers the
            // attributes written on it.
            for test in cfg.tests() {
                // A method declaration is never the sole body of a control structure, so removing
                // one leaves nothing that needs a `;` to stand in for it.
                out.remove_host(test.range.start().into(), test.range.end().into(), false);
            }
        }
        for span in cfg.attr_spans() {
            let (start, end) = (usize::from(span.start()), usize::from(span.end()));
            if !out.disables((start, end)) {
                out.blanks.push(Blank {
                    start,
                    end,
                    semicolon: false,
                });
            }
        }
        for host in cfg.disabled_hosts() {
            out.remove_host(
                host.range.start().into(),
                host.range.end().into(),
                Self::needs_semicolon(&host.host),
            );
        }
        for error in cfg.errors() {
            let line = Self::line_of(text, error.range.start().into());
            out.errors.push(error.kind.render_with_line(line));
        }
        out
    }

    /// Blank a whole host away and record it as removed, so a rewrite another pass planned inside
    /// it is dropped.
    fn remove_host(&mut self, start: usize, end: usize, semicolon: bool) {
        self.blanks.push(Blank {
            start,
            end,
            semicolon,
        });
        self.disabled.push((start, end));
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
