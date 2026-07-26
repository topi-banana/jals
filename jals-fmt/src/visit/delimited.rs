//! Delimited lists: argument lists, parameter lists, record headers, resources, array
//! initializers.
//!
//! Every one of them is `open`, an item run separated by commas, `close`. Handling them in one
//! place is what makes `[wrapping]`'s per-construct policies and `[wrapping] paren-*` behave
//! consistently: a rule that changes how an argument list wraps changes a record header the same
//! way, because there is only one implementation to change.
//!
//! # `paren-*`
//!
//! [`ParenPositions`] decides whether the delimiters share a line with the items. The two
//! "if wrapped" variants are correlated decisions — the closing delimiter goes to its own line
//! exactly when the opening one did — which is what [`BreakTag`](crate::ir::BreakTag) and
//! [`Indent::If`] exist for, rather than something the visitor has to guess.

use alloc::vec::Vec;

use jals_config::fmt::{ParenPositions, WrapPolicy};
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode};

use crate::ir::Indent;
use crate::visit::Ctx;

impl Ctx<'_> {
    /// A paren-delimited list.
    pub(super) async fn visit_delimited(&mut self, node: &SyntaxNode) {
        let policy = self.list_policy(node);
        let parens = self.paren_positions(node);
        self.emit_delimited(node, policy, parens, S::LPAREN, S::RPAREN)
            .await;
    }

    /// Which `[wrapping]` rule governs this list.
    fn list_policy(&self, node: &SyntaxNode) -> WrapPolicy {
        let wrapping = &self.style.cfg.wrapping;
        match node.kind() {
            S::ARG_LIST => wrapping.call_arguments,
            S::PARAM_LIST | S::LAMBDA_PARAMS => wrapping.method_parameters,
            S::RECORD_HEADER => wrapping.record_components,
            S::ANNOTATION_ARG_LIST | S::ATTR_ARG_LIST => wrapping.annotation_arguments,
            S::RESOURCE_LIST => self.resource_policy(),
            S::RECORD_PATTERN => wrapping.deconstruction_list,
            _ => wrapping.call_arguments,
        }
    }

    /// Which `[wrapping] paren-*` rule governs this list's delimiters.
    fn paren_positions(&self, node: &SyntaxNode) -> ParenPositions {
        let wrapping = &self.style.cfg.wrapping;
        match node.kind() {
            S::ARG_LIST => wrapping.paren_method_invocation,
            S::PARAM_LIST => wrapping.paren_method_declaration,
            S::LAMBDA_PARAMS => wrapping.paren_lambda,
            S::RECORD_HEADER => wrapping.paren_record,
            S::ANNOTATION_ARG_LIST | S::ATTR_ARG_LIST => wrapping.paren_annotation,
            S::RESOURCE_LIST => wrapping.paren_control,
            _ => wrapping.paren_method_invocation,
        }
    }

    /// The shared delimited-list emitter.
    async fn emit_delimited(
        &mut self,
        node: &SyntaxNode,
        policy: WrapPolicy,
        parens: ParenPositions,
        open: S,
        close: S,
    ) {
        let children = Self::children(node);
        let items: Vec<&SyntaxElement> = children
            .iter()
            .filter(|child| {
                !matches!(
                    child.as_token().map(|tok| tok.kind()),
                    Some(kind) if kind == open || kind == close
                )
            })
            .collect();
        let empty = items
            .iter()
            .all(|child| child.as_token().is_some_and(|tok| tok.kind() == S::COMMA));

        let continuation = self.style.continuation();
        // A tag lets the closing delimiter follow the opening one's decision, which is what
        // `separate-lines-if-wrapped` means.
        let tag = self.ops.new_tag();

        let mut opened = false;
        for child in &children {
            let kind = child.as_token().map(|tok| tok.kind());
            if kind == Some(open) {
                self.visit_element(child).await;
                self.open(continuation.clone());
                opened = true;
                if !empty {
                    self.delimiter_break(parens, Some(tag));
                }
                continue;
            }
            if kind == Some(close) {
                if opened {
                    if !empty {
                        self.closing_break(parens, tag);
                    }
                    self.close_indent(&continuation);
                    opened = false;
                }
                self.visit_element(child).await;
                continue;
            }
            let after_comma = Self::follows_comma(&children, child);
            if after_comma {
                self.list_break(policy, Indent::ZERO);
            }
            self.visit_element(child).await;
        }
        if opened {
            self.close_indent(&continuation);
        }
    }

    /// Whether `child` is preceded by a comma among `children`.
    fn follows_comma(children: &[SyntaxElement], child: &SyntaxElement) -> bool {
        let Some(at) = children.iter().position(|other| other == child) else {
            return false;
        };
        at > 0
            && matches!(
                children[at - 1].as_token().map(|tok| tok.kind()),
                Some(S::COMMA)
            )
    }

    /// The break just inside an opening delimiter.
    fn delimiter_break(&mut self, parens: ParenPositions, tag: Option<crate::ir::BreakTag>) {
        match parens {
            // google-java-format's shape: the first item shares the opener's line.
            // `preserve` is rounded here by `Style::reify`.
            ParenPositions::CommonLines | ParenPositions::Preserve => {
                self.ops
                    .brk(crate::ir::FillMode::Unified, "", Indent::ZERO, tag);
                self.mark_spaced();
            }
            ParenPositions::SeparateLinesIfWrapped => {
                self.ops
                    .brk(crate::ir::FillMode::Unified, "", Indent::ZERO, tag);
                self.mark_spaced();
            }
            ParenPositions::SeparateLinesIfNotEmpty | ParenPositions::SeparateLines => {
                self.forced_break(Indent::ZERO);
            }
        }
    }

    /// The break just before a closing delimiter.
    ///
    /// Under `common-lines` there is none — the `)` hugs the last item, which is what
    /// google-java-format always does. The `separate-lines*` variants dedent it back to the line
    /// that opened the list.
    fn closing_break(&mut self, parens: ParenPositions, tag: crate::ir::BreakTag) {
        let dedent = Indent::columns(-i32::try_from(self.style.continuation_cols).unwrap_or(0));
        match parens {
            ParenPositions::CommonLines | ParenPositions::Preserve => {}
            ParenPositions::SeparateLinesIfWrapped => {
                // Only when the opening delimiter broke: an `Indent::If` on the same tag.
                self.ops.brk(
                    crate::ir::FillMode::Unified,
                    "",
                    Indent::when_broken(tag, dedent, Indent::ZERO),
                    None,
                );
                self.mark_spaced();
            }
            ParenPositions::SeparateLinesIfNotEmpty | ParenPositions::SeparateLines => {
                self.forced_break(dedent);
            }
        }
    }

    /// An array initializer `{ a, b, c }`.
    ///
    /// `tabular-array-initializers` keeps a grid-shaped initializer's source rows instead of
    /// reflowing it by width — google-java-format's behavior, and the one place a *layout*
    /// decision legitimately consults the source, because the grid is information the width
    /// alone cannot recover.
    pub(super) async fn visit_array_init(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.array_initializer;
        if self.style.cfg.wrapping.tabular_array_initializers && Self::is_tabular(node) {
            self.emit_tabular_array(node).await;
            return;
        }
        self.emit_delimited(
            node,
            policy,
            ParenPositions::CommonLines,
            S::LBRACE,
            S::RBRACE,
        )
        .await;
    }

    /// Whether an initializer is written as a grid: more than one line, and every line holding
    /// the same number of elements.
    fn is_tabular(node: &SyntaxNode) -> bool {
        let mut rows: Vec<usize> = Vec::new();
        let mut current = 0usize;
        let mut saw_break = false;
        for child in node.children_with_tokens() {
            match child {
                SyntaxElement::Token(tok) if tok.kind() == S::NEWLINE => {
                    if current > 0 {
                        rows.push(current);
                        current = 0;
                    }
                    saw_break = true;
                }
                SyntaxElement::Node(_) => current += 1,
                SyntaxElement::Token(_) => {}
            }
        }
        if current > 0 {
            rows.push(current);
        }
        saw_break && rows.len() > 1 && rows.windows(2).all(|pair| pair[0] == pair[1])
    }

    /// Emit a grid-shaped initializer, keeping the source's row breaks.
    async fn emit_tabular_array(&mut self, node: &SyntaxNode) {
        let indent = self.style.continuation();
        let mut opened = false;
        let mut pending_row_break = false;
        for child in node.children_with_tokens() {
            match &child {
                SyntaxElement::Token(tok) if tok.kind() == S::NEWLINE => {
                    if opened {
                        pending_row_break = true;
                    }
                    continue;
                }
                SyntaxElement::Token(tok) if tok.kind().is_trivia() => continue,
                SyntaxElement::Token(tok) if tok.kind() == S::LBRACE => {
                    self.token(tok);
                    self.open(indent.clone());
                    opened = true;
                    pending_row_break = true;
                    continue;
                }
                SyntaxElement::Token(tok) if tok.kind() == S::RBRACE => {
                    if opened {
                        self.close_indent(&indent);
                        opened = false;
                    }
                    self.forced_break(Indent::columns(0));
                    self.token(tok);
                    continue;
                }
                _ => {}
            }
            if pending_row_break {
                pending_row_break = false;
                self.forced_break(Indent::ZERO);
            }
            self.visit_element(&child).await;
        }
        if opened {
            self.close_indent(&indent);
        }
    }

    /// Note that whitespace has already been emitted, so the next token owes no space.
    fn mark_spaced(&mut self) {
        self.space_already_emitted();
    }
}
