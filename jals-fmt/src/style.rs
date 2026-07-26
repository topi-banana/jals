//! The resolved style — [`Config`] folded once into the form the engine and the visitors read.
//!
//! This is `DESIGN.md` §8's four seams made concrete. A [`Style`] is built once per format and is
//! then immutable, so no visitor re-derives a column count or re-checks a rounding rule:
//!
//! | seam | what moves | read by |
//! |---|---|---|
//! | **S1** engine constants | column limit, block / continuation indent, tab rendering, terminator | [`engine`](crate::engine) |
//! | **S2** emission shape | where a level opens, break fill mode and side, whether a space is emitted | [`visit`](crate::visit) |
//! | **S3** forced breaks and blank lines | brace forcing, one-line collapsing, blank-line counts | [`visit`](crate::visit) |
//! | **S4** pass gating | which L0 / L3 / L4 passes run | [`passes`](crate::passes) |
//!
//! What is **not** here is the resolution algorithm: `compute_breaks` is the same code at every
//! setting. Nothing in this file can make the engine backtrack, search, or read input whitespace.
//!
//! # Rounding
//!
//! Five rules in `Config` are functions of the input's *existing* line breaks. The single engine
//! deliberately does not read those (`DESIGN.md` §17: the one whitespace fact it reads is whether
//! two significant tokens have a blank line between them). [`Style::reify`] rounds each to the
//! nearest canonical value **and reports the rounding as a [`Warning`]** — silently ignoring the
//! setting would leave the user believing it took effect.

use alloc::format;
use alloc::vec::Vec;

use jals_config::fmt::{Comments, Config, IndentStyle, KeepOnOneLine, ParenPositions, Wrapping};

use crate::ir::Indent;
use crate::output::Warning;

/// A [`Config`] resolved for one format run.
#[derive(Debug, Clone)]
pub(crate) struct Style {
    /// The configuration, with the whitespace-dependent rules already rounded. Visitors read
    /// their rules straight off this rather than through 176 mirrored fields.
    pub(crate) cfg: Config,
    /// The line terminator, with `auto` / `native` already resolved against the source.
    pub(crate) newline: &'static str,
    /// Columns in one block indent level.
    pub(crate) indent_cols: usize,
    /// Columns in one continuation indent.
    pub(crate) continuation_cols: usize,
}

impl Style {
    /// Resolve `config` for formatting `src`, collecting a [`Warning`] for every rule rounded to
    /// the single engine's canonical value.
    pub(crate) fn reify(config: &Config, src: &str) -> (Self, Vec<Warning>) {
        let mut cfg = config.clone();
        let mut warnings = Vec::new();

        Self::round_keep_on_one_line(&mut cfg, &mut warnings);
        Self::round_paren_positions(&mut cfg, &mut warnings);
        Self::round_line_break_rules(&mut cfg, &mut warnings);

        let style = Self {
            newline: cfg.layout.newline(src),
            indent_cols: cfg.layout.indent_cols(),
            continuation_cols: cfg.layout.continuation_cols(),
            cfg,
        };
        (style, warnings)
    }

    /// `keep-*-on-one-line = preserve` keeps a body on one line iff the source had it there.
    /// Rounded to `if-single-item`, the closest structural reading of the same intent: rounding
    /// to `never` would instead guarantee a mismatch on every input written that way.
    fn round_keep_on_one_line(cfg: &mut Config, warnings: &mut Vec<Warning>) {
        /// Every `keep-*-on-one-line` field, as accessors so the sweep names each rule once.
        const FIELDS: [(
            &str,
            fn(&mut jals_config::fmt::Braces) -> &mut KeepOnOneLine,
        ); 8] = [
            ("keep-type-body-on-one-line", |b| {
                &mut b.keep_type_body_on_one_line
            }),
            ("keep-method-body-on-one-line", |b| {
                &mut b.keep_method_body_on_one_line
            }),
            ("keep-block-on-one-line", |b| &mut b.keep_block_on_one_line),
            ("keep-lambda-body-on-one-line", |b| {
                &mut b.keep_lambda_body_on_one_line
            }),
            ("keep-switch-body-on-one-line", |b| {
                &mut b.keep_switch_body_on_one_line
            }),
            ("keep-enum-declaration-on-one-line", |b| {
                &mut b.keep_enum_declaration_on_one_line
            }),
            ("keep-record-declaration-on-one-line", |b| {
                &mut b.keep_record_declaration_on_one_line
            }),
            ("keep-annotation-declaration-on-one-line", |b| {
                &mut b.keep_annotation_declaration_on_one_line
            }),
        ];

        for (name, field) in FIELDS {
            let slot = field(&mut cfg.braces);
            if *slot == KeepOnOneLine::Preserve {
                *slot = KeepOnOneLine::IfSingleItem;
                warnings.push(Warning::config(format!(
                    "[braces] {name} = \"preserve\" reads the source's line breaks, which the \
                     single layout engine does not do; using \"if-single-item\" instead",
                )));
            }
        }
    }

    /// `paren-* = preserve` keeps the delimiters wherever the source put them. Rounded to
    /// `common-lines`, which is what every canonical formatter does.
    fn round_paren_positions(cfg: &mut Config, warnings: &mut Vec<Warning>) {
        /// Every `paren-*` field.
        const FIELDS: [(&str, fn(&mut Wrapping) -> &mut ParenPositions); 6] = [
            ("paren-method-declaration", |w| {
                &mut w.paren_method_declaration
            }),
            ("paren-method-invocation", |w| {
                &mut w.paren_method_invocation
            }),
            ("paren-control", |w| &mut w.paren_control),
            ("paren-annotation", |w| &mut w.paren_annotation),
            ("paren-lambda", |w| &mut w.paren_lambda),
            ("paren-record", |w| &mut w.paren_record),
        ];

        for (name, field) in FIELDS {
            let slot = field(&mut cfg.wrapping);
            if *slot == ParenPositions::Preserve {
                *slot = ParenPositions::CommonLines;
                warnings.push(Warning::config(format!(
                    "[wrapping] {name} = \"preserve\" reads the source's line breaks, which the \
                     single layout engine does not do; using \"common-lines\" instead",
                )));
            }
        }
    }

    /// The three rules that ask the engine to keep, rather than recompute, the input's line
    /// breaks. All three round to "always recompute".
    fn round_line_break_rules(cfg: &mut Config, warnings: &mut Vec<Warning>) {
        if !cfg.wrapping.join_wrapped_lines {
            cfg.wrapping.join_wrapped_lines = true;
            warnings.push(Warning::config(
                "[wrapping] join-wrapped-lines = false keeps the source's line breaks, which the \
                 single layout engine does not do; lines are always rejoined"
                    .into(),
            ));
        }
        // `wrap-long-lines = true` asks for a break where the syntax offers none — IntelliJ will
        // split mid-expression to respect the margin. A `Doc` engine can only take breaks that
        // were emitted, so the request cannot be honored; the engine already wraps at every break
        // point it has, which is what the default (`false`) describes. Only the opt-in value is
        // worth reporting.
        if cfg.wrapping.wrap_long_lines {
            cfg.wrapping.wrap_long_lines = false;
            warnings.push(Warning::config(
                "[wrapping] wrap-long-lines = true asks for a break where the syntax offers none; \
                 the single layout engine only takes breaks a rule emitted, so a line with no \
                 break point stays long"
                    .into(),
            ));
        }
        if cfg.comments.preserve_line_breaks {
            cfg.comments.preserve_line_breaks = false;
            warnings.push(Warning::config(
                "[comments] preserve-line-breaks = true keeps the source's line breaks inside \
                 comment prose, which the single layout engine does not do; prose is always \
                 refilled"
                    .into(),
            ));
        }
    }

    /// The column limit.
    pub(crate) const fn max_width(&self) -> usize {
        self.cfg.layout.max_width
    }

    /// One block indent level, as an [`Indent`].
    pub(crate) fn indent(&self) -> Indent {
        Indent::columns(i32::try_from(self.indent_cols).unwrap_or(i32::MAX))
    }

    /// One continuation indent, as an [`Indent`].
    pub(crate) fn continuation(&self) -> Indent {
        Indent::columns(i32::try_from(self.continuation_cols).unwrap_or(i32::MAX))
    }

    /// Render `columns` of indentation into `out`, honoring `[layout] indent-style`.
    ///
    /// Indentation is always *measured* in columns; only the characters differ. `tab` and
    /// `mixed` both emit tabs up to the last whole tab stop and spaces for the remainder, which
    /// for the usual `indent-width == tab-width` is exactly one tab per level.
    pub(crate) fn write_indent(&self, columns: usize, out: &mut alloc::string::String) {
        match self.cfg.layout.indent_style {
            IndentStyle::Space => {
                for _ in 0..columns {
                    out.push(' ');
                }
            }
            IndentStyle::Tab | IndentStyle::Mixed => {
                let tab = self.cfg.layout.tab_width.max(1);
                for _ in 0..(columns / tab) {
                    out.push('\t');
                }
                for _ in 0..(columns % tab) {
                    out.push(' ');
                }
            }
        }
    }

    /// The comment reflow settings.
    pub(crate) const fn comments(&self) -> &Comments {
        &self.cfg.comments
    }

    /// The width comment prose is reflowed to, measured from column zero.
    ///
    /// `count-width-from-start` measures Eclipse's `comment.line_length` from the comment's own
    /// starting column instead, so the budget shifts with the indent.
    pub(crate) const fn comment_width(&self, start_column: usize) -> usize {
        if self.cfg.comments.count_width_from_start {
            start_column + self.cfg.comments.width
        } else {
            self.cfg.comments.width
        }
    }
}
