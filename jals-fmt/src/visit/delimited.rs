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
    ///
    /// An argument list gets one refinement google-java-format applies and no vendor setting
    /// expresses: a list of **simple** arguments (names, literals, `X.class`) packs by width,
    /// but as soon as one argument is itself a call, a lambda, or an initializer, the list goes
    /// one argument per line. Packing complex arguments produces lines that read as one
    /// expression when they are several, which is the readability problem the rule exists for.
    /// The refinement only ever *tightens* `if-long`; the other three policies say what they mean
    /// and are left alone.
    fn list_policy(&self, node: &SyntaxNode) -> WrapPolicy {
        let wrapping = &self.style.cfg.wrapping;
        let policy = match node.kind() {
            S::ARG_LIST => wrapping.call_arguments,
            S::PARAM_LIST | S::LAMBDA_PARAMS => wrapping.method_parameters,
            S::RECORD_HEADER => wrapping.record_components,
            S::ANNOTATION_ARG_LIST | S::ATTR_ARG_LIST => wrapping.annotation_arguments,
            S::RESOURCE_LIST => self.resource_policy(),
            S::RECORD_PATTERN => wrapping.deconstruction_list,
            _ => wrapping.call_arguments,
        };
        if policy == WrapPolicy::IfLong
            && matches!(node.kind(), S::ARG_LIST | S::ANNOTATION_ARG_LIST)
            && !node.children().all(|arg| Self::is_simple_argument(&arg))
        {
            return WrapPolicy::IfLongPerItem;
        }
        policy
    }

    /// Whether an argument is simple enough to share a line with its neighbours.
    fn is_simple_argument(node: &SyntaxNode) -> bool {
        match node.kind() {
            S::NAME_REF | S::LITERAL | S::CLASS_LITERAL | S::TYPE => true,
            S::FIELD_ACCESS | S::PAREN_EXPR | S::UNARY_EXPR | S::POSTFIX_EXPR => node
                .children()
                .all(|child| Self::is_simple_argument(&child)),
            _ => false,
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
    /// An array initializer `{ a, b, c }`.
    ///
    /// Unlike a paren-delimited list, an initializer is **block-shaped**: the `{` ends its line,
    /// the elements sit at one *block* indent, and the `}` returns to the line that opened it.
    /// That is what google-java-format does and why an initializer is the one list whose closing
    /// delimiter always dangles.
    pub(super) async fn visit_array_init(&mut self, node: &SyntaxNode) {
        if self.style.cfg.wrapping.tabular_array_initializers && Self::is_tabular(node) {
            self.emit_tabular_array(node).await;
            return;
        }
        let policy = self.style.cfg.wrapping.array_initializer;
        let indent = self.style.indent();
        let children = Self::children(node);
        let empty = !children.iter().any(|child| child.as_node().is_some());
        // A trailing comma is the author saying "one item per row" — google-java-format keeps
        // such an initializer vertical whether or not it would fit. The comma is a significant
        // token that survives either way; only the layout responds to it.
        let trailing_comma = Self::has_trailing_comma(&children);
        let last_element = children.iter().rposition(|child| child.as_node().is_some());

        self.open_flat(Indent::ZERO);
        let mut opened = false;
        for (nth, child) in children.iter().enumerate() {
            match child.as_token().map(|tok| tok.kind()) {
                Some(S::LBRACE) => {
                    self.visit_element(child).await;
                    if !empty {
                        self.open(indent.clone());
                        opened = true;
                        self.edge_break(trailing_comma);
                    }
                    continue;
                }
                Some(S::RBRACE) => {
                    if opened {
                        self.close_indent(&indent);
                        opened = false;
                        self.edge_break(trailing_comma);
                    }
                    self.visit_element(child).await;
                    continue;
                }
                Some(S::COMMA) => {
                    self.visit_element(child).await;
                    // The trailing comma separates nothing, so the closing edge's break is the
                    // only one it needs; emitting one here too would leave `1, 2, }`.
                    if last_element.is_some_and(|last| nth < last) {
                        self.list_break(policy, Indent::ZERO);
                    }
                    continue;
                }
                _ => {}
            }
            self.visit_element(child).await;
        }
        if opened {
            self.close_indent(&indent);
        }
        self.close();
    }

    /// The break just inside an initializer's braces: forced when the initializer is pinned
    /// vertical, negotiable otherwise.
    fn edge_break(&mut self, vertical: bool) {
        if vertical {
            self.forced_break(Indent::ZERO);
        } else {
            self.break_tight(Indent::ZERO);
        }
    }

    /// Whether the last significant token before the closing brace is a comma.
    fn has_trailing_comma(children: &[SyntaxElement]) -> bool {
        let Some(last_element) = children.iter().rposition(|child| child.as_node().is_some())
        else {
            return false;
        };
        children[last_element + 1..]
            .iter()
            .any(|child| child.as_token().is_some_and(|tok| tok.kind() == S::COMMA))
    }

    /// Whether an initializer is written as a **grid**: several source rows, each holding the
    /// same number of elements, and more than one element per row.
    ///
    /// The row-length test is what separates a table from a plain one-per-line list. An
    /// initializer written one element per row carries no column structure, so
    /// google-java-format refills it by width; a `3 × 3` block does, and refilling it would
    /// destroy information the source encodes and the width cannot recover. A short final row is
    /// tolerated — a table's last line is rarely full.
    fn is_tabular(node: &SyntaxNode) -> bool {
        let mut rows: Vec<usize> = Vec::new();
        for element in node.children() {
            let leaves = Self::leaf_count(&element);
            if rows.is_empty() || Self::starts_a_row(&element) {
                rows.push(leaves);
            } else if let Some(last) = rows.last_mut() {
                *last += leaves;
            }
        }
        let Some((last, full)) = rows.split_last() else {
            return false;
        };
        full.len() > 1 && full[0] > 1 && full.iter().all(|row| *row == full[0]) && *last <= full[0]
    }

    /// How many *leaf* values an element contributes to its row.
    ///
    /// A row of nested initializers is a row of their contents: `{{"a","b","c"}, …}` written one
    /// nested initializer per line is a three-column table, while `{{"a"}, {"b"}}` written the
    /// same way is a one-column list and gets refilled.
    fn leaf_count(node: &SyntaxNode) -> usize {
        if node.kind() != S::ARRAY_INIT {
            return 1;
        }
        node.children()
            .map(|child| Self::leaf_count(&child))
            .sum::<usize>()
            .max(1)
    }

    /// Whether `node` is the first element on its source line.
    fn starts_a_row(node: &SyntaxNode) -> bool {
        let Some(first) = Ctx::first_token(node) else {
            return false;
        };
        let mut cursor = first.prev_token();
        while let Some(previous) = cursor {
            match previous.kind() {
                S::NEWLINE => return true,
                kind if kind.is_trivia() => {}
                _ => return false,
            }
            cursor = previous.prev_token();
        }
        false
    }

    /// Emit a grid-shaped initializer, keeping the source's row breaks.
    async fn emit_tabular_array(&mut self, node: &SyntaxNode) {
        let indent = self.style.indent();
        self.open_flat(Indent::ZERO);
        let mut opened = false;
        for child in Self::children(node) {
            match child.as_token().map(|tok| tok.kind()) {
                Some(S::LBRACE) => {
                    self.visit_element(&child).await;
                    self.open(indent.clone());
                    opened = true;
                    self.forced_break(Indent::ZERO);
                    continue;
                }
                Some(S::RBRACE) => {
                    if opened {
                        self.close_indent(&indent);
                        opened = false;
                    }
                    self.forced_break(Indent::ZERO);
                    self.visit_element(&child).await;
                    continue;
                }
                _ => {}
            }
            if let Some(element) = child.as_node()
                && Self::starts_a_row(element)
                && !self.ops.last_is_break()
            {
                self.forced_break(Indent::ZERO);
            }
            self.visit_element(&child).await;
        }
        if opened {
            self.close_indent(&indent);
        }
        self.close();
    }

    /// Note that whitespace has already been emitted, so the next token owes no space.
    fn mark_spaced(&mut self) {
        self.space_already_emitted();
    }
}
