//! Sequences of statements / members.
//!
//! Lowers a `{ ... }` body (block, class body, switch body) with one indentation level, plus the
//! item sequences inside it. Brace placement follows [`BraceStyle`] / [`ControlBraceStyle`], and a
//! few options (`empty-item-single-line`, `fn-single-line`, `force-multiline-blocks`) tune the
//! empty / single-statement cases. Blank lines from the source are preserved (clamped by the
//! renderer).

use alloc::vec;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use jals_exec::Yielder;

use crate::config::{BraceStyle, ControlBraceStyle, SwitchCaseBody};
use crate::doc::Doc;
use crate::lower::Ctx;

/// A `case` constant (a `Pattern` / `Expr`, plus an optional `Guard` and `case null, default`'s
/// `default` token) paired with the `,` that follows it (`None` for the last constant).
type CaseChunk = (Vec<SyntaxElement>, Option<SyntaxToken>);

/// A switch label paired with its terminating `:` token.
type SwitchLabelPair = (SyntaxNode, SyntaxToken);

impl Ctx<'_> {
    /// Lower a `{ ... }` node (block, class body, switch body) with one indentation level.
    pub(crate) async fn lower_braced(&self, node: &SyntaxNode) -> Doc {
        let tokens: Vec<SyntaxToken> = node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .collect();
        let lbrace = tokens.iter().find(|t| t.kind() == S::LBRACE);
        let rbrace = tokens.iter().rfind(|t| t.kind() == S::RBRACE);

        // Malformed (a brace is missing from error recovery): never synthesize a brace —
        // fall back to inline emission so the significant-token sequence is preserved.
        let (Some(lbrace), Some(rbrace)) = (lbrace, rbrace) else {
            return self.lower_generic(node).await;
        };

        let (inner, any) = self.lower_items(node).await;
        let open = self.tok(lbrace);
        let has_dangling = self.comments.has_dangling(rbrace);
        let dangling = self.comments.dangling(rbrace);
        let close = Doc::concat(vec![Doc::text("}"), self.comments.trailing_doc(rbrace)]);

        // An empty body. Under `empty_item_single_line` (the default) it collapses to `{}` on the
        // header's line regardless of brace style, so `next-line` never strands a lone `{}`. With
        // the option off, an empty *declaration* body (a type body, or a method / constructor /
        // initializer block) instead expands to a two-line `{ <newline> }`, opening on its own
        // line under `brace-style = next-line`; control-flow / `switch` / lambda / bare blocks are
        // never governed and always keep `{}` (matching rustfmt's item-only scoping).
        if !any && !has_dangling {
            // `force_multiline_blocks` expands every empty block to a two-line `{ <newline> }`,
            // overriding `empty_item_single_line` and extending past it to control-flow / `switch` /
            // lambda / bare blocks (which the latter never governs).
            if !self.cfg.force_multiline_blocks
                && (self.cfg.empty_item_single_line || !Self::governs_empty_single_line(node))
            {
                return Doc::concat(vec![open, close]);
            }
            let lead = if self.opens_on_next_line(node) {
                Doc::hardline()
            } else {
                Doc::nil()
            };
            return Doc::concat(vec![lead, open, Doc::hardline(), close]);
        }

        // `fn-single-line`: a declaration body holding exactly one statement and no comments
        // collapses onto the header's line when it fits `max-width`. The grouped layout renders
        // flat (`header { stmt }`) when it fits and falls back to the standard multi-line body
        // otherwise; the `if_break` lead keeps the brace on its own line in the broken case under
        // `brace-style = next-line` (and is not a forced break, so the flat form stays available).
        if self.cfg.fn_single_line
            && !self.cfg.force_multiline_blocks
            && !has_dangling
            && Self::is_declaration_body(node)
            && self.single_statement_no_comments(node).await
            && !self.header_has_trailing_comment(node).await
        {
            let lead = if self.opens_on_next_line(node) {
                Doc::if_break(Doc::hardline(), Doc::nil())
            } else {
                Doc::nil()
            };
            return Doc::group(Doc::concat(vec![
                lead,
                open,
                Doc::indent(Doc::concat(vec![Doc::line(), inner])),
                Doc::line(),
                close,
            ]));
        }

        // The break after the opening `{`. Normally a plain `hardline`, but with
        // `blank_line_at_block_start` a leading blank line in the source is preserved (clamped by
        // the renderer), so the body's first item keeps its blank line just like the inter-item
        // breaks do.
        let lead_break = match (
            self.cfg.blank_line_at_block_start,
            Self::first_item_token(node),
        ) {
            (true, Some(t)) => self.break_before(&t),
            _ => Doc::hardline(),
        };
        let mut body: Vec<Doc> = vec![lead_break];
        if any {
            body.push(inner);
        }
        if has_dangling {
            if any {
                body.push(Doc::hardline());
            }
            body.push(dangling);
        }

        // Under a `next-line` style a (non-empty) body opens its brace on its own line. The
        // leading break renders at the header's indentation; the separating space the parent
        // emitted before the brace is then trimmed away by the renderer.
        let lead = if self.opens_on_next_line(node) {
            Doc::hardline()
        } else {
            Doc::nil()
        };

        Doc::concat(vec![
            lead,
            open,
            Doc::indent(Doc::concat(body)),
            Doc::hardline(),
            close,
        ])
    }

    /// Whether the opening brace of braced `node` should sit on its own line. Declaration bodies
    /// — every type body (`CLASS_BODY`), a module body (`MODULE_BODY`), and the block of a method,
    /// constructor, or initializer — follow [`BraceStyle`]; control-flow blocks, `switch` blocks,
    /// lambda bodies, and bare statement blocks follow [`ControlBraceStyle`].
    fn opens_on_next_line(&self, node: &SyntaxNode) -> bool {
        match node.kind() {
            S::CLASS_BODY | S::MODULE_BODY => self.cfg.brace_style == BraceStyle::NextLine,
            S::BLOCK if Self::is_declaration_body(node) => {
                self.cfg.brace_style == BraceStyle::NextLine
            }
            S::BLOCK | S::SWITCH_BLOCK => {
                self.cfg.control_brace_style == ControlBraceStyle::NextLine
            }
            _ => false,
        }
    }

    /// Whether `node` (a `BLOCK`) is a declaration body — the block of a method, constructor, or
    /// initializer — as opposed to a control-flow block, lambda body, or bare block.
    fn is_declaration_body(node: &SyntaxNode) -> bool {
        matches!(
            node.parent().map(|p| p.kind()),
            Some(S::METHOD_DECL | S::CONSTRUCTOR_DECL | S::INITIALIZER)
        )
    }

    /// Whether an empty `node` is governed by
    /// [`empty_item_single_line`](crate::config::Config::empty_item_single_line) — a declaration
    /// body: a type body (`CLASS_BODY`), a module body (`MODULE_BODY`), or a method / constructor /
    /// initializer block. Control-flow blocks, `switch` blocks, lambda bodies, and bare blocks are
    /// never governed (they always keep `{}`), matching rustfmt's item-only scoping.
    fn governs_empty_single_line(node: &SyntaxNode) -> bool {
        matches!(node.kind(), S::CLASS_BODY | S::MODULE_BODY)
            || (node.kind() == S::BLOCK && Self::is_declaration_body(node))
    }

    /// Whether `node` (a braced body) holds exactly one statement and carries no comment anywhere
    /// inside the braces — the precondition for
    /// [`fn_single_line`](crate::config::Config::fn_single_line) to collapse it onto one line. A
    /// comment (which must never be dropped or moved off its anchor) or a second statement keeps the
    /// body multi-line.
    async fn single_statement_no_comments(&self, node: &SyntaxNode) -> bool {
        let mut stmts = node
            .children()
            .filter(|c| Self::first_sig_token(c).is_some());
        if stmts.next().is_none() || stmts.next().is_some() {
            return false; // zero or more than one statement
        }
        !self.has_comments_in_subtree(node).await
    }

    /// Whether any significant token of `node`'s parent declaration that precedes the body (`node`)
    /// carries a trailing comment. Such a comment renders as a line suffix that flushes at the
    /// body's first newline; collapsing the body onto one line under
    /// [`fn_single_line`](crate::config::Config::fn_single_line) would relocate it past the closing
    /// brace, re-anchoring it on the next parse and breaking idempotency — so a header trailing
    /// comment keeps the body multi-line. (A comment *inside* the braces is already caught by
    /// [`Ctx::single_statement_no_comments`].)
    async fn header_has_trailing_comment(&self, node: &SyntaxNode) -> bool {
        let Some(parent) = node.parent() else {
            return false;
        };
        let mut yielder = Yielder::new();
        let body_start = node.text_range().start();
        for t in parent
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            yielder.tick().await;
            if !t.kind().is_trivia()
                && t.text_range().end() <= body_start
                && self.comments.has_trailing(&t)
            {
                return true;
            }
        }
        false
    }

    /// Build the inner document for a sequence of item nodes. Returns the content and whether
    /// any item was emitted. Braces are skipped (a brace wrapper adds them); blank lines from
    /// the source are preserved (clamped by the renderer).
    pub(crate) async fn lower_items(&self, node: &SyntaxNode) -> (Doc, bool) {
        let mut parts: Vec<Doc> = Vec::new();
        let mut saw = false;

        for el in node.children_with_tokens() {
            if let Some(child) = el.as_node() {
                // Skip empty nodes (e.g. an empty `MODIFIERS` produced by error recovery):
                // they carry no tokens, and emitting a separator for them would introduce
                // spurious blank lines that grow on re-formatting.
                if Self::first_sig_token(child).is_none() {
                    continue;
                }
                if saw {
                    parts.push(self.item_separator(child));
                }
                parts.push(self.lower(child).await);
                saw = true;
            } else if let Some(t) = el.as_token() {
                let kind = t.kind();
                if kind == S::LBRACE || kind == S::RBRACE || kind.is_trivia() {
                    continue;
                }
                // Stray significant token (e.g. a lone `;`); keep it, space-separated.
                if saw {
                    parts.push(Doc::text(" "));
                }
                parts.push(self.tok(t));
                saw = true;
            }
        }
        (Doc::concat(parts), saw)
    }

    /// The token a leading blank line before the body's first item should anchor on: the first item
    /// node's first significant token (or a leading stray significant token), skipping the opening
    /// brace and trivia. Mirrors how [`Ctx::lower_items`] picks the first item, so the blank-line
    /// run before it is counted exactly as an inter-item break would count it.
    fn first_item_token(node: &SyntaxNode) -> Option<SyntaxToken> {
        for el in node.children_with_tokens() {
            match el {
                SyntaxElement::Node(child) => {
                    if let Some(t) = Self::first_sig_token(&child) {
                        return Some(t);
                    }
                }
                SyntaxElement::Token(t) => {
                    let kind = t.kind();
                    if kind == S::LBRACE || kind == S::RBRACE || kind.is_trivia() {
                        continue;
                    }
                    return Some(t);
                }
            }
        }
        None
    }

    /// The line break before an item node: the source's blank-line run (clamped to
    /// `max_blank_lines` by the renderer) when it had one, else a plain line break.
    pub(crate) fn item_separator(&self, node: &SyntaxNode) -> Doc {
        Self::first_sig_token(node).map_or_else(Doc::hardline, |t| self.break_before(&t))
    }

    /// The line break before a row anchored at significant token `t`: the source's blank-line run
    /// (clamped to `max_blank_lines` by the renderer) when one preceded it, else a plain line break.
    /// The token-anchored core of [`Ctx::item_separator`], shared with the enum-body lowering (which
    /// also anchors on bare `;` tokens that have no containing item node).
    pub(crate) fn break_before(&self, t: &SyntaxToken) -> Doc {
        let blanks = if self.comments.has_leading(t) {
            self.comments.blank_lines_before_first(t)
        } else {
            Self::blank_lines_before(t)
        };
        if blanks > 0 {
            Doc::blank_line(blanks)
        } else {
            Doc::hardline()
        }
    }

    /// The number of blank lines preceding `tok` in the source (0 when it is on the next line,
    /// or on the same line as the previous token). A run of `n` consecutive newlines is `n - 1`
    /// blank lines. A comment between stops the run, so a lone comment line is not a blank line.
    pub(crate) fn blank_lines_before(tok: &SyntaxToken) -> usize {
        let mut newlines = 0usize;
        let mut cur = tok.prev_token();
        while let Some(t) = cur {
            match t.kind() {
                S::NEWLINE => newlines += 1,
                S::WHITESPACE => {}
                _ => break,
            }
            cur = t.prev_token();
        }
        newlines.saturating_sub(1)
    }

    /// Lower a *legacy* (colon-form) `switch` group — `(SwitchLabel ':')+ Stmt*` — per
    /// [`SwitchCaseBody`]. The arrow form is a `SWITCH_RULE` node and never reaches here.
    ///
    /// `SameLine` keeps the generic inline layout wholesale (`case X: stmt; stmt;`). `Always` (and a
    /// `SingleLine` group that is not eligible to stay inline) puts each label on its own line and
    /// breaks every body statement onto its own line, indented one level — google-java-format's
    /// layout. `SingleLine` keeps a lone label with a single, comment-free statement inline. A
    /// malformed group (error recovery — a label without a colon, a stray significant token) falls
    /// back to the inline path, so every significant token is still emitted exactly once.
    pub(crate) async fn lower_switch_group(&self, node: &SyntaxNode) -> Doc {
        if self.cfg.switch_case_body == SwitchCaseBody::SameLine {
            return self.lower_generic(node).await;
        }
        let Some((labels, stmts)) = Self::split_switch_group(node) else {
            return self.lower_generic(node).await;
        };
        // `single-line`: a single label with a single statement and no comments stays on the colon
        // line. A comment forces the broken, indented form so it renders correctly and idempotently.
        if self.cfg.switch_case_body == SwitchCaseBody::SingleLine
            && labels.len() == 1
            && stmts.len() == 1
            && !self.has_comments_in_subtree(node).await
        {
            return self.lower_generic(node).await;
        }

        let mut parts: Vec<Doc> = Vec::new();
        for (i, (label, colon)) in labels.iter().enumerate() {
            // Each fall-through label takes its own line; `item_separator` preserves a leading
            // comment / blank line on the label, whose tokens are emitted by `lower_elements`.
            if i > 0 {
                parts.push(self.item_separator(label));
            }
            let els = [
                SyntaxElement::Node(label.clone()),
                SyntaxElement::Token(colon.clone()),
            ];
            parts.push(self.lower_elements(els.into_iter(), false).await);
        }
        // The body statements break onto their own lines, one indent level deeper than the labels;
        // the first statement's `item_separator` is the break from the last label's colon line.
        let mut body: Vec<Doc> = Vec::new();
        for stmt in &stmts {
            body.push(self.item_separator(stmt));
            body.push(self.lower(stmt).await);
        }
        if !body.is_empty() {
            parts.push(Doc::indent(Doc::concat(body)));
        }
        Doc::concat(parts)
    }

    /// Lower an *arrow-form* `switch` rule — `SwitchLabel '->' (Block | ThrowStmt | Expr ';')`.
    ///
    /// When a comment forces a break right after `->` — a trailing comment on `->`, or a leading
    /// comment on the body's first token — the `->` and the body hang at one *continuation* indent
    /// past the label (the continuation-indent counterpart to the legacy colon group's
    /// block-indented body, [`Ctx::lower_switch_group`]). Gating on a forced break keeps every
    /// comment-free body byte-for-byte unchanged (a long body still wraps on the arrow line, a
    /// `{ … }` block still aligns its `}` with the label); and because the wrap only fires when the
    /// body is already on its own line, shifting the *whole* body to the continuation level is
    /// correct even for an anonymous-class / lambda body (its `}` stays aligned with the hung body).
    /// A `{ … }` body is excluded (blocks never take a continuation indent), and a malformed rule
    /// with no `->` falls back to the inline path, so every significant token is still emitted
    /// exactly once.
    pub(crate) async fn lower_switch_rule(&self, node: &SyntaxNode) -> Doc {
        // Children (grammar): `SwitchLabel '->' (Block | ThrowStmt | Expr ';')`. The label is the
        // only significant content before the `->`, so the rule splits cleanly into the label and a
        // tail.
        let label = node.children().find(|n| n.kind() == S::SWITCH_LABEL);
        let arrow = node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|t| t.kind() == S::ARROW);
        let (Some(label), Some(arrow)) = (label, arrow) else {
            return self.lower_generic(node).await; // malformed: missing the label or the `->`
        };

        // A `{ … }` block body keeps the generic layout — its `{` rides on the arrow line and it
        // aligns its own `}` with the label, so it is never hung at a continuation indent.
        let body = node.children().find(|n| n.kind() != S::SWITCH_LABEL);
        if body.as_ref().map(SyntaxNode::kind) == Some(S::BLOCK) {
            return self.lower_generic(node).await;
        }

        // The body lands on its own line only when a comment forces a break right after `->` — a
        // trailing comment on `->`, or a leading comment on the body's first significant token.
        let forces_break = self.comments.has_trailing(&arrow)
            || body
                .as_ref()
                .and_then(Self::first_sig_token)
                .is_some_and(|t| self.comments.has_leading(&t));

        // A wrappable multi-constant label with a non-block body breaks the body onto its own
        // continuation line when the arm overflows, so the label group is measured against
        // `case … ->` alone rather than the whole arm. This matches google-java-format, which wraps
        // the labels only when the labels themselves overflow and otherwise breaks the body — e.g.
        // I880's `case 0, 1, …, 9 -> "…"`, where the labels fit but the long body does not. Without
        // this the label group's `fits` look-ahead would run into the body and wrap the (short)
        // labels instead.
        if !forces_break && self.case_label_wraps(&label) {
            let arrow_sep = self
                .sep(Self::last_sig_token(&label).as_ref(), &arrow)
                .await;
            let after_arrow = node
                .children_with_tokens()
                .skip_while(|e| e.as_token().map(SyntaxToken::kind) != Some(S::ARROW))
                .skip(1);
            return Doc::group(Doc::concat(vec![
                self.lower(&label).await,
                arrow_sep,
                self.tok(&arrow),
                Doc::continuation_indent(Doc::concat(vec![
                    Doc::line(),
                    self.lower_elements(after_arrow, false).await,
                ])),
            ]));
        }
        if !forces_break {
            return self.lower_generic(node).await;
        }

        // The label stays at the rule's level; the `->` and the body hang at one continuation
        // indent, so the comment-forced body line sits one level past the label. The `->` is inside
        // the wrap so the break its trailing comment carries — and any leading-comment break on the
        // body — is requested at `base + continuation`, governing where the body's first line lands.
        let arrow_sep = self
            .sep(Self::last_sig_token(&label).as_ref(), &arrow)
            .await;
        let tail = node
            .children_with_tokens()
            .skip_while(|e| e.as_token().map(SyntaxToken::kind) != Some(S::ARROW));
        Doc::concat(vec![
            self.lower(&label).await,
            arrow_sep,
            Doc::continuation_indent(self.lower_elements(tail, false).await),
        ])
    }

    /// Lower a `switch` `case` label, wrapping its comma-separated constant list across lines when
    /// `wrap-case-labels` is on and the arm overflows `max-width` — google-java-format's layout
    /// (e.g. `ExpressionSwitch`'s `breakLongCaseArgs`). The `case` keyword stays at the label's
    /// indent, the first constant rides on the `case` line, and each subsequent constant hangs at
    /// one continuation indent, the comma kept attached to its constant (the break falls *after* the
    /// comma). The trailing `->` / `:` and the body are emitted by the caller, so the group is
    /// measured together with what follows on the same line (`fits` looks past the group, stopping
    /// at the body's first break) — a short list, a single constant, and a bare `default` all stay
    /// on one line.
    ///
    /// With the option off, or for any label that is not a multi-constant `case`, this is
    /// byte-for-byte [`Ctx::lower_generic`]; a malformed label (no `case`, an empty constant) also
    /// falls back, so every significant token is still emitted exactly once.
    pub(crate) async fn lower_switch_label(&self, node: &SyntaxNode) -> Doc {
        if !self.cfg.wrap_case_labels {
            return self.lower_generic(node).await;
        }
        // Only a multi-constant `case` list can wrap; skip the split (and its allocations) for the
        // common single-constant label and a bare `default`, neither of which has a top-level comma.
        if !node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .any(|t| t.kind() == S::COMMA)
        {
            return self.lower_generic(node).await;
        }
        let Some((case_kw, chunks)) = Self::split_case_label(node) else {
            return self.lower_generic(node).await; // a bare `default`, or malformed
        };
        // A lone constant with a trailing comma (`case A,` — error recovery) yields a single chunk.
        if chunks.len() < 2 {
            return self.lower_generic(node).await;
        }
        let Some(first) = chunks
            .first()
            .and_then(|(els, _)| Self::first_sig_token_of_elements(els))
        else {
            return self.lower_generic(node).await;
        };

        // Each constant carries its trailing comma, and consecutive constants are joined by `line()`
        // (a space when the list fits, a break when it wraps). The whole run hangs at one
        // continuation indent; the first constant stays on the `case` line (no leading break), the
        // rest break below.
        let mut rows: Vec<Doc> = Vec::with_capacity(chunks.len());
        for (els, comma) in &chunks {
            let mut row = vec![self.lower_elements(els.iter().cloned(), false).await];
            if let Some(c) = comma {
                // No space before a comma; its leading/trailing comments ride along via `tok`.
                row.push(self.tok(c));
            }
            rows.push(Doc::concat(row));
        }
        Doc::group(Doc::concat(vec![
            self.tok(&case_kw),
            Doc::continuation_indent(Doc::concat(vec![
                self.sep(Some(&case_kw), &first).await,
                Doc::join(&Doc::line(), rows),
            ])),
        ]))
    }

    /// Whether [`Ctx::lower_switch_label`] will actually wrap `label` — `wrap-case-labels` is on and
    /// the label is a multi-constant `case` list (two or more comma-separated constants). The arrow
    /// rule uses this to decide whether to break a non-block body, so the wrapping label group is
    /// measured against `case … ->` alone. The cheap comma pre-check skips the split for the common
    /// single-constant label and a bare `default`.
    fn case_label_wraps(&self, label: &SyntaxNode) -> bool {
        self.cfg.wrap_case_labels
            && label
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .any(|t| t.kind() == S::COMMA)
            && Self::split_case_label(label).is_some_and(|(_, chunks)| chunks.len() >= 2)
    }

    /// Split a `SWITCH_LABEL` into its `case` keyword and the comma-separated constant chunks.
    /// Returns `None` for a bare `default` (no `case` keyword) or a malformed label (an empty /
    /// leading comma), so the caller falls back to inline emission and preserves every token.
    /// Trivia are dropped here (comments are re-attached to their token by `lower_elements` / `tok`).
    fn split_case_label(node: &SyntaxNode) -> Option<(SyntaxToken, Vec<CaseChunk>)> {
        let mut case_kw: Option<SyntaxToken> = None;
        let mut chunks: Vec<CaseChunk> = Vec::new();
        let mut current: Vec<SyntaxElement> = Vec::new();
        for el in node.children_with_tokens() {
            match &el {
                SyntaxElement::Token(t) if t.kind().is_trivia() => {}
                SyntaxElement::Token(t)
                    if t.kind() == S::CASE_KW
                        && case_kw.is_none()
                        && current.is_empty()
                        && chunks.is_empty() =>
                {
                    case_kw = Some(t.clone());
                }
                SyntaxElement::Token(t) if t.kind() == S::COMMA => {
                    if current.is_empty() {
                        return None; // a leading / doubled comma (error recovery)
                    }
                    chunks.push((core::mem::take(&mut current), Some(t.clone())));
                }
                _ => current.push(el),
            }
        }
        if !current.is_empty() {
            chunks.push((current, None));
        }
        Some((case_kw?, chunks))
    }

    /// The first significant token among a run of CST elements, descending into nodes.
    fn first_sig_token_of_elements(els: &[SyntaxElement]) -> Option<SyntaxToken> {
        els.iter().find_map(|e| match e {
            SyntaxElement::Node(n) => Self::first_sig_token(n),
            SyntaxElement::Token(t) if !t.kind().is_trivia() => Some(t.clone()),
            SyntaxElement::Token(_) => None,
        })
    }

    /// Split a `SWITCH_GROUP` into its `(label, ':')` pairs and its body statements. Returns `None`
    /// for a malformed group (a label without a following colon, a statement before a colon, a stray
    /// colon / significant token, or no labels at all) so the caller can fall back to inline
    /// emission and preserve every token. Empty (token-less) statement nodes from error recovery are
    /// skipped.
    fn split_switch_group(node: &SyntaxNode) -> Option<(Vec<SwitchLabelPair>, Vec<SyntaxNode>)> {
        let mut labels: Vec<SwitchLabelPair> = Vec::new();
        let mut stmts: Vec<SyntaxNode> = Vec::new();
        let mut pending: Option<SyntaxNode> = None;
        for el in node.children_with_tokens() {
            if let Some(n) = el.as_node() {
                if n.kind() == S::SWITCH_LABEL {
                    // A label must precede every statement and follow a colon-terminated label.
                    if pending.is_some() || !stmts.is_empty() {
                        return None;
                    }
                    pending = Some(n.clone());
                } else if Self::first_sig_token(n).is_some() {
                    if pending.is_some() {
                        return None; // a statement before its label's colon
                    }
                    stmts.push(n.clone());
                }
            } else if let Some(t) = el.as_token() {
                let kind = t.kind();
                if kind == S::COLON {
                    labels.push((pending.take()?, t.clone()));
                } else if !kind.is_trivia() {
                    return None; // a stray significant token
                }
            }
        }
        if pending.is_some() || labels.is_empty() {
            return None;
        }
        Some((labels, stmts))
    }

    /// Whether any significant token in `node`'s subtree carries a comment. Used to keep a body that
    /// holds a comment (which must never be dropped or moved off its anchor) out of a one-line
    /// layout.
    async fn has_comments_in_subtree(&self, node: &SyntaxNode) -> bool {
        let mut yielder = Yielder::new();
        for t in node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            yielder.tick().await;
            if !t.kind().is_trivia() && self.comments.has_comments(&t) {
                return true;
            }
        }
        false
    }
}
