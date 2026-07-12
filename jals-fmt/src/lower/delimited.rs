//! Delimited lists (params, args, array initializers).
//!
//! A comma-separated, delimited list wraps all-or-nothing: items are separated by a soft line
//! that is a space when flat and a break when wrapped. Argument lists and array initializers also
//! break against their own width budgets (`fn-call-width` / `array-width`); a parameter list
//! follows `fn-params-layout`. With `overflow-delimited-expr` a call / annotation argument list
//! whose final item is a delimited expression instead hangs that item past the call line. The
//! trailing comma is preserved by default; for an array initializer it follows `trailing-comma`.
//! With `closing-paren = hug` a wrapped paren-delimited list (call / annotation args, params,
//! record header) keeps its closing `)` on the last item's line instead of dedenting it.

use alloc::vec;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::{ClosingParen, FnParamsLayout, TrailingComma};
use crate::doc::Doc;
use crate::lower::Ctx;

impl Ctx<'_> {
    /// The node forming the entire final item of a paren-delimited list — `(…, <node>)` with the
    /// node directly between the last comma (or the open paren) and the close paren — plus that
    /// close paren. `None` for any other shape: a trailing comma, stray or missing tokens, or an
    /// empty recovery node; the caller then keeps the all-or-nothing layout.
    fn sole_last_item(list: &SyntaxNode) -> Option<(SyntaxNode, SyntaxToken)> {
        let sig: Vec<SyntaxElement> = list
            .children_with_tokens()
            .filter(|el| match el {
                SyntaxElement::Node(n) => Self::first_sig_token(n).is_some(),
                SyntaxElement::Token(t) => !t.kind().is_trivia(),
            })
            .collect();
        let [.., prev, cand, close] = sig.as_slice() else {
            return None;
        };
        let close = close.as_token().filter(|t| t.kind() == S::RPAREN)?.clone();
        let node = cand.as_node()?.clone();
        match prev.as_token().map(SyntaxToken::kind)? {
            S::COMMA | S::LPAREN => Some((node, close)),
            _ => None,
        }
    }

    /// Whether `node` is a delimited expression that `overflow-delimited-expr` may hang: a
    /// block-bodied lambda, an anonymous-class / array-creating `new`, a bare array initializer,
    /// or — in an annotation argument list — an `ANNOTATION_PAIR` whose value is one of those.
    /// Expression-bodied lambdas are excluded so they keep the all-or-nothing layout exactly.
    fn is_overflowable_expr(node: &SyntaxNode, in_annotation: bool) -> bool {
        match node.kind() {
            S::LAMBDA_EXPR => node.children().any(|c| c.kind() == S::BLOCK),
            S::ARRAY_INIT => true,
            S::NEW_EXPR => node
                .children()
                .any(|c| matches!(c.kind(), S::CLASS_BODY | S::ARRAY_INIT)),
            S::ANNOTATION_PAIR if in_annotation => node
                .children()
                .last()
                .is_some_and(|v| Self::is_overflowable_expr(&v, false)),
            _ => false,
        }
    }

    /// Whether `overflow-delimited-expr` applies to this list: the option is on, the list is a
    /// call / annotation argument list, its final item is exactly one overflowable node, and
    /// neither that node's first token nor the close paren carries leading comments — those need
    /// their own line above their anchor, which the vertical layout provides.
    fn overflows_last_item(&self, node: &SyntaxNode) -> bool {
        if !self.cfg.overflow_delimited_expr
            || !matches!(node.kind(), S::ARG_LIST | S::ANNOTATION_ARG_LIST)
        {
            return false;
        }
        let Some((cand, close)) = Self::sole_last_item(node) else {
            return false;
        };
        Self::is_overflowable_expr(&cand, node.kind() == S::ANNOTATION_ARG_LIST)
            && Self::first_sig_token(&cand).is_some_and(|t| !self.comments.has_leading(&t))
            && !self.comments.has_leading(&close)
    }

    /// Whether the array element `node` begins a new *source row* — its leading trivia (every token
    /// before its first significant one) contains a `NEWLINE`. A comment in that leading run is not
    /// a newline, so it does not start a row here; [`Ctx::tabular_rows`] disqualifies any commented
    /// initializer separately.
    fn starts_new_row(node: &SyntaxNode) -> bool {
        for t in node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            if !t.kind().is_trivia() {
                return false;
            }
            if t.kind() == S::NEWLINE {
                return true;
            }
        }
        false
    }

    /// Whether the subtree under `node` contains any comment token. A tabular layout preserves
    /// inter-element whitespace only; an interior comment would need its own anchoring, so a
    /// commented initializer is never treated as tabular and falls back to width-based wrapping.
    fn has_interior_comment(node: &SyntaxNode) -> bool {
        node.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .any(|t| crate::comments::CommentMap::is_comment(t.kind()))
    }

    /// The source-row partition of an array initializer laid out as a *table*, or `None` when it is
    /// not tabular. Tabular requires: no interior comments; at least two source rows; every row but
    /// the last holding the same element count `N` (`N >= 1`); and the last holding `1..=N`.
    /// `element_count` is the number of comma-separated items the caller built — the partition is
    /// rejected unless it accounts for exactly that many, so a malformed list (stray tokens, a
    /// count mismatch) safely falls back to width-based wrapping.
    fn tabular_rows(node: &SyntaxNode, element_count: usize) -> Option<Vec<usize>> {
        if Self::has_interior_comment(node) {
            return None;
        }
        let mut rows: Vec<usize> = Vec::new();
        let mut cur = 0usize;
        for child in node
            .children()
            .filter(|c| Self::first_sig_token(c).is_some())
        {
            if cur > 0 && Self::starts_new_row(&child) {
                rows.push(cur);
                cur = 0;
            }
            cur += 1;
        }
        if cur > 0 {
            rows.push(cur);
        }
        if rows.len() < 2 || rows.iter().sum::<usize>() != element_count {
            return None;
        }
        // Every pushed row has `cur > 0`, so `n >= 1`; a table needs every body row to equal `n`
        // and the final row to hold `1..=n`.
        let n = rows[0];
        let (body, last) = rows.split_at(rows.len() - 1);
        if body.iter().any(|&c| c != n) || last[0] > n {
            return None;
        }
        Some(rows)
    }

    /// Lay out a tabular array initializer: each source row on its own line, elements within a row
    /// separated by a single space, rows separated by a forced break. `items` are the already-built
    /// per-element docs (each including its trailing comma); `row_sizes` partitions them into rows
    /// and is guaranteed by [`Ctx::tabular_rows`] to sum to `items.len()`.
    ///
    /// Unlike the width-wrapped array path (which uses `continuation_indent`), the rows are laid
    /// out with one block [`Doc::indent`] level: a preserved table reads as a block of rows, and
    /// google-java-format indents tabular contents by the block indent, not the wider continuation
    /// indent.
    fn tabular_doc(open: Doc, close: Doc, items: Vec<Doc>, row_sizes: &[usize]) -> Doc {
        let mut it = items.into_iter();
        let row_docs: Vec<Doc> = row_sizes
            .iter()
            .map(|&n| Doc::join(&Doc::text(" "), it.by_ref().take(n).collect()))
            .collect();
        Doc::concat(vec![
            open,
            Doc::indent(Doc::concat(vec![
                Doc::hardline(),
                Doc::join(&Doc::hardline(), row_docs),
            ])),
            Doc::hardline(),
            close,
        ])
    }

    /// Whether `kind` is a *paren-delimited* list whose closing `)` is governed by `closing-paren`:
    /// a call / annotation argument list, a parameter list, or a record header. The brace-delimited
    /// array initializer (`ARRAY_INIT`) is excluded — its `}` always stays on its own line.
    const fn is_paren_delimited(kind: S) -> bool {
        matches!(
            kind,
            S::ARG_LIST | S::PARAM_LIST | S::ANNOTATION_ARG_LIST | S::RECORD_HEADER
        )
    }

    /// The separator emitted just before a wrapped list's closing token. Hugging the last item
    /// emits nothing — the close token cuddles it; otherwise a soft line, so the close token
    /// dedents onto its own line when the list breaks and collapses to nothing when it stays flat.
    fn close_sep(hug: bool) -> Doc {
        if hug { Doc::nil() } else { Doc::softline() }
    }

    /// Lower a comma-separated, delimited list that wraps all-or-nothing. Items are separated by a
    /// soft line that becomes a space when flat and a break when wrapped. An argument list
    /// (`ARG_LIST`) additionally breaks when its flat width exceeds `fn-call-width`, and an array
    /// initializer (`ARRAY_INIT`) when it exceeds `array-width`.
    ///
    /// With `overflow-delimited-expr` enabled, a call or annotation argument list whose final
    /// item is a delimited expression (see [`Ctx::is_overflowable_expr`]) instead hangs that item:
    /// laid out flat, the earlier items stay on the line and only the item's body breaks
    /// (`f(a, () -> {` … `});`); laid out broken, the result is identical to the all-or-nothing
    /// layout.
    ///
    /// Inter-item commas are emitted verbatim. The final item's trailing comma is preserved by
    /// default; for an array initializer it instead follows the `trailing-comma` policy (see
    /// [`crate::rules::trailing_comma::TrailingCommaRule::doc`]) — the only Java list where adding
    /// or dropping it is legal.
    pub(crate) fn lower_delimited(&self, node: &SyntaxNode) -> Doc {
        // Never synthesize a delimiter that the source lacks (error recovery): start empty
        // and fill from the real tokens so the significant-token sequence is preserved.
        let mut open_doc = Doc::nil();
        let mut close_doc = Doc::nil();
        // Whether a closing delimiter token was actually present. A list left open by error
        // recovery has none; synthesizing a trailing comma there is unsafe (see `policy` below).
        let mut has_close = false;
        // Each row is one item's content plus the comma token that follows it (if any). The comma
        // of the final row is the list's (optional) trailing comma.
        let mut rows: Vec<(Doc, Option<SyntaxToken>)> = Vec::new();
        let mut current: Vec<Doc> = Vec::new();
        let mut cur_prev: Option<SyntaxToken> = None;

        for el in node.children_with_tokens() {
            if let Some(child) = el.as_node() {
                if let Some(first) = Self::first_sig_token(child) {
                    current.push(self.sep(cur_prev.as_ref(), &first));
                }
                current.push(self.lower(child));
                cur_prev = Self::last_sig_token(child);
            } else if let Some(t) = el.as_token() {
                let kind = t.kind();
                match kind {
                    S::LPAREN | S::LBRACE | S::LBRACK => open_doc = self.tok(t),
                    S::RPAREN | S::RBRACE | S::RBRACK => {
                        close_doc = self.tok(t);
                        has_close = true;
                    }
                    S::COMMA => {
                        // The comma ends the current item; keep it so the trailing one can follow
                        // the `trailing-comma` policy while inter-item commas stay verbatim.
                        rows.push((Doc::concat(core::mem::take(&mut current)), Some(t.clone())));
                        cur_prev = None;
                    }
                    _ if kind.is_trivia() => {}
                    _ => {
                        current.push(self.sep(cur_prev.as_ref(), t));
                        current.push(self.tok(t));
                        cur_prev = Some(t.clone());
                    }
                }
            }
        }
        if !current.is_empty() {
            rows.push((Doc::concat(core::mem::take(&mut current)), None));
        }

        if rows.is_empty() {
            return Doc::concat(vec![open_doc, close_doc]);
        }

        // Only an array initializer honors `trailing-comma`; every other delimited list preserves
        // the source exactly (a trailing comma elsewhere is invalid Java, reachable only via error
        // recovery, so dropping/adding one is never appropriate). An initializer left unclosed by
        // error recovery (no `}`) is also preserved: with no closing brace a synthesized trailing
        // comma is not actually trailing — on a re-parse it reads as an item separator and pulls
        // the following token into the list, which would break idempotency.
        let policy = if node.kind() == S::ARRAY_INIT && has_close {
            self.cfg.trailing_comma
        } else {
            TrailingComma::Preserve
        };

        let last = rows.len() - 1;
        let mut items: Vec<Doc> = rows
            .into_iter()
            .enumerate()
            .map(|(i, (content, comma))| {
                let comma_doc = if i == last {
                    crate::rules::trailing_comma::TrailingCommaRule::doc(
                        policy,
                        comma.as_ref(),
                        self,
                    )
                } else {
                    // An inter-item comma is required; emit it verbatim. A missing one (malformed
                    // input) is never synthesized.
                    comma.map_or_else(Doc::nil, |t| self.tok(&t))
                };
                Doc::concat(vec![content, comma_doc])
            })
            .collect();

        // `tabular-array-initializers`: a table-shaped array initializer keeps its source row
        // breaks instead of reflowing by width. Layout-only — `items` already carries every token
        // verbatim.
        if node.kind() == S::ARRAY_INIT
            && self.cfg.tabular_array_initializers
            && let Some(row_sizes) = Self::tabular_rows(node, items.len())
        {
            return Self::tabular_doc(open_doc, close_doc, items, &row_sizes);
        }

        // `closing-paren = hug` keeps the closing `)` on the last item's line (no break before it)
        // for a paren-delimited list; the default dedents it onto its own line. The brace-delimited
        // array initializer is never hugged.
        let hug_close =
            self.cfg.closing_paren == ClosingParen::Hug && Self::is_paren_delimited(node.kind());

        // `overflow-delimited-expr`: hang the final delimited item past the call line. Laid out
        // flat, the earlier items stay on the line and only the item's body breaks; laid out
        // broken, the result is identical to the all-or-nothing layout below, so this structure
        // strictly generalizes it.
        if self.overflows_last_item(node)
            && let Some(last_item) = items.pop()
        {
            let mut head_inner = vec![Doc::softline()];
            if !items.is_empty() {
                head_inner.push(Doc::join(&Doc::line(), items));
                head_inner.push(Doc::line());
            }
            let head = Doc::concat(vec![
                open_doc,
                Doc::continuation_indent(Doc::concat(head_inner)),
            ]);
            // Hug drops the break before `)` so it cuddles the overflowed item's close (`});`); a
            // flat layout already hugs (the softline is flat), so this only changes the broken case.
            let tail = Doc::concat(vec![Self::close_sep(hug_close), close_doc]);
            let budget = (node.kind() == S::ARG_LIST).then_some(self.cfg.fn_call_width);
            return Doc::group_overflow(head, last_item, tail, budget);
        }

        // A `Compressed` parameter list packs as many parameters per line as fit (a `Fill`);
        // every other list joins its items with a plain break, wrapping all-or-nothing.
        let compressed_params =
            node.kind() == S::PARAM_LIST && self.cfg.fn_params_layout == FnParamsLayout::Compressed;
        let inner = if compressed_params {
            Doc::fill(items)
        } else {
            Doc::join(&Doc::line(), items)
        };
        let doc = Doc::concat(vec![
            open_doc,
            Doc::continuation_indent(Doc::concat(vec![Doc::softline(), inner])),
            Self::close_sep(hug_close),
            close_doc,
        ]);
        // A call's argument list (`ARG_LIST`) honors `fn-call-width` and an array initializer
        // (`ARRAY_INIT`) honors `array-width`. A parameter list (`PARAM_LIST`) follows
        // `fn-params-layout`: `Vertical` forces one parameter per line, while `Tall` / `Compressed`
        // break against `max-width` like every other list. The rest only break against `max-width`.
        match node.kind() {
            S::ARG_LIST => Doc::group_within(doc, self.cfg.fn_call_width),
            S::ARRAY_INIT => Doc::group_within(doc, self.cfg.array_width),
            S::PARAM_LIST if self.cfg.fn_params_layout == FnParamsLayout::Vertical => {
                Doc::group_always_break(doc)
            }
            _ => Doc::group(doc),
        }
    }
}
