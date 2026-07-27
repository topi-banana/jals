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
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::ir::Indent;
use crate::visit::Ctx;

impl Ctx<'_> {
    /// A paren-delimited list.
    pub(super) async fn visit_delimited(&mut self, node: &SyntaxNode) {
        // A format call's policy is decided by the values it interpolates, not by the template:
        // the template is long by nature and would send every value onto a line of its own.
        let format = self.is_format_call(node);
        let policy = if format {
            let rest = node.children().skip(1);
            if self.style.cfg.wrapping.call_arguments == WrapPolicy::IfLong
                && !self.fills_items(rest)
            {
                WrapPolicy::IfLongPerItem
            } else {
                self.style.cfg.wrapping.call_arguments
            }
        } else {
            self.list_policy(node)
        };
        let parens = self.paren_positions(node);
        self.emit_delimited(node, policy, parens, S::LPAREN, S::RPAREN, format)
            .await;
    }

    /// Which `[wrapping]` rule governs this list.
    ///
    /// An argument list gets one refinement: `[wrapping] fill-item-width` turns a fill into a
    /// one-per-line list once any argument is wide enough that packing would read as one
    /// expression when it is several. The refinement only ever *tightens* `if-long`; the other
    /// three policies say what they mean and are left alone.
    #[allow(
        clippy::match_same_arms,
        reason = "a named arm documents that the kind was considered, even when it falls back"
    )]
    fn list_policy(&self, node: &SyntaxNode) -> WrapPolicy {
        let wrapping = &self.style.cfg.wrapping;
        let policy = match node.kind() {
            S::ARG_LIST => wrapping.call_arguments,
            S::PARAM_LIST | S::LAMBDA_PARAMS => wrapping.method_parameters,
            S::RECORD_HEADER => wrapping.record_components,
            S::ANNOTATION_ARG_LIST | S::ATTR_ARG_LIST => wrapping.annotation_arguments,
            S::RESOURCE_LIST => self.resource_policy(),
            S::RECORD_PATTERN => wrapping.deconstruction_list,
            // A list the grammar can produce but no rule names: an argument list is the closest
            // shape, and its policy is the one a reader would expect to govern it.
            _ => wrapping.call_arguments,
        };
        if policy == WrapPolicy::IfLong
            && matches!(node.kind(), S::ARG_LIST | S::ANNOTATION_ARG_LIST)
            && !self.fills(node)
        {
            return WrapPolicy::IfLongPerItem;
        }
        policy
    }

    /// Whether every item is short enough for the list to keep filling — see
    /// `[wrapping] fill-item-width`.
    fn fills(&self, node: &SyntaxNode) -> bool {
        self.fills_items(node.children())
    }

    /// The same test over an arbitrary run of items.
    fn fills_items(&self, items: impl Iterator<Item = SyntaxNode>) -> bool {
        let limit = self.style.cfg.wrapping.fill_item_width;
        if limit == 0 {
            return true;
        }
        items
            .into_iter()
            .all(|arg| Self::source_width(&arg) < limit)
    }

    /// Whether this list is a call whose first argument is a format string — see
    /// `[wrapping] format-string-arguments`.
    fn is_format_call(&self, node: &SyntaxNode) -> bool {
        self.style.cfg.wrapping.format_string_arguments
            && node.kind() == S::ARG_LIST
            && node.children().count() >= 2
            && node
                .children()
                .next()
                .is_some_and(|first| Self::is_format_string(&first))
    }

    /// Whether an expression is a string literal — or a concatenation of them — carrying a format
    /// specifier.
    ///
    /// `isStringConcat`: the whole argument has to be literal text, because an interpolation the
    /// formatter cannot see the shape of is not a template.
    fn is_format_string(node: &SyntaxNode) -> bool {
        let mut placeholder = false;
        for element in node.descendants_with_tokens() {
            match element {
                SyntaxElement::Node(inner) => {
                    if !matches!(inner.kind(), S::LITERAL | S::BINARY_EXPR) {
                        return false;
                    }
                }
                SyntaxElement::Token(tok) if tok.kind().is_trivia() => {}
                SyntaxElement::Token(tok) => match tok.kind() {
                    S::STRING_LITERAL | S::TEXT_BLOCK => {
                        placeholder |= Self::has_placeholder(tok.text());
                    }
                    S::PLUS => {}
                    _ => return false,
                },
            }
        }
        placeholder
    }

    /// Whether a literal's text holds a `%` or a `{0}`-style placeholder.
    fn has_placeholder(text: &str) -> bool {
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                return true;
            }
            if ch == '{'
                && chars.peek().is_some_and(char::is_ascii_digit)
                && chars.clone().nth(1) == Some('}')
            {
                return true;
            }
        }
        false
    }

    /// Which `[wrapping] paren-*` rule governs this list's delimiters.
    #[allow(
        clippy::match_same_arms,
        reason = "a named arm documents that the kind was considered, even when it falls back"
    )]
    fn paren_positions(&self, node: &SyntaxNode) -> ParenPositions {
        let wrapping = &self.style.cfg.wrapping;
        match node.kind() {
            S::ARG_LIST => wrapping.paren_method_invocation,
            S::PARAM_LIST => wrapping.paren_method_declaration,
            S::LAMBDA_PARAMS => wrapping.paren_lambda,
            S::RECORD_HEADER => wrapping.paren_record,
            S::ANNOTATION_ARG_LIST | S::ATTR_ARG_LIST => wrapping.paren_annotation,
            S::RESOURCE_LIST => wrapping.paren_control,
            // Same fallback as `list_policy`, for the same reason.
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
        format: bool,
    ) {
        let children = Self::children(node);
        let items: Vec<&SyntaxElement> = children
            .iter()
            .filter(|child| {
                !matches!(
                    child.as_token().map(SyntaxToken::kind),
                    Some(kind) if kind == open || kind == close
                )
            })
            .collect();
        let empty = items
            .iter()
            .all(|child| child.as_token().is_some_and(|tok| tok.kind() == S::COMMA));

        // The construct that owns a list may decide where it indents from: a method chain, because
        // the answer depends on whether the dot before the name broke (`addArguments`' `plusIndent`
        // parameter), and a declaration header, because the header level already took the step.
        let continuation = self
            .list_indent
            .take()
            .unwrap_or_else(|| self.style.continuation());
        // A tag lets the closing delimiter follow the opening one's decision, which is what
        // `separate-lines-if-wrapped` means.
        let tag = self.ops.new_tag();

        let mut opened = false;
        // A format call's values get a level of their own after the template, so they pack
        // together instead of inheriting the break the template forced.
        let mut values = false;
        let mut seen = 0usize;
        for child in &children {
            let kind = child.as_token().map(SyntaxToken::kind);
            if kind == Some(open) {
                self.visit_element(child).await;
                self.open(continuation.clone());
                opened = true;
                if !empty {
                    self.delimiter_break(parens, Some(tag));
                    // The items get a level of their own, inside the one the delimiters own.
                    // Breaking after the `(` and packing the items onto the continuation line are
                    // then two decisions rather than one, which is what lets
                    // `f(\n    a, b, c)` exist at all — google-java-format's `builder.open(ZERO)`
                    // around `argList` / `visitFormals`.
                    self.open_flat(Indent::ZERO);
                }
                continue;
            }
            if kind == Some(close) {
                if opened {
                    if !empty {
                        if values {
                            self.close();
                            values = false;
                        }
                        self.close();
                        self.closing_break(parens, tag);
                    }
                    // The closing delimiter is *inside* the level, so the level's width is the
                    // whole list including its `)`. Closing first would measure one column short
                    // and let a list that ends exactly at the limit stay flat.
                    self.visit_element(child).await;
                    self.close_indent(&continuation);
                    opened = false;
                    continue;
                }
                self.visit_element(child).await;
                continue;
            }
            if self.style.cfg.wrapping.before_comma {
                if child.as_token().is_some_and(|tok| tok.kind() == S::COMMA) {
                    let flat = Self::flat_space(self.style.cfg.spacing.before_comma);
                    self.list_break_flat(policy, flat, Indent::ZERO);
                } else if Self::follows_comma(&children, child) {
                    self.space_if(self.style.cfg.spacing.after_comma);
                }
            } else if Self::follows_comma(&children, child) {
                let flat = Self::flat_space(self.style.cfg.spacing.after_comma);
                if format && seen == 1 && !values {
                    // `isFormatMethod`: the break after the template is all-or-nothing, and the
                    // values that follow it fill among themselves.
                    self.ops
                        .brk(crate::ir::FillMode::Unified, flat, Indent::ZERO, None);
                    self.space_already_emitted();
                    self.open_flat(Indent::ZERO);
                    values = true;
                } else {
                    self.list_break_flat(policy, flat, Indent::ZERO);
                }
            }
            if child.as_node().is_some() {
                seen += 1;
            }
            self.visit_element(child).await;
        }
        if opened {
            if !empty {
                if values {
                    self.close();
                }
                self.close();
            }
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
                children[at - 1].as_token().map(SyntaxToken::kind),
                Some(S::COMMA)
            )
    }

    /// The break just inside an opening delimiter.
    fn delimiter_break(&mut self, parens: ParenPositions, tag: Option<crate::ir::BreakTag>) {
        match parens {
            // google-java-format's shape: the first item shares the opener's line. The
            // "if wrapped" variant opens the same way and differs only at the *closing*
            // delimiter, which reads this break's tag. `preserve` is rounded here by
            // `Style::reify`.
            ParenPositions::CommonLines
            | ParenPositions::Preserve
            | ParenPositions::SeparateLinesIfWrapped => {
                self.ops
                    .brk(crate::ir::FillMode::Unified, "", Indent::ZERO, tag);
                self.space_already_emitted();
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
                self.space_already_emitted();
            }
            ParenPositions::SeparateLinesIfNotEmpty | ParenPositions::SeparateLines => {
                self.forced_break(dedent);
            }
        }
    }

    /// The level a variable's array initializer opens at.
    ///
    /// A declaration opens a continuation level for its initializer, but a block-shaped one does
    /// not use it: `int[] xs = {` stays on the declaration's line, so its contents belong one
    /// block indent from *that* line and its `}` at the line's own indent. Cancelling the
    /// continuation here is google-java-format's `minusFour`.
    fn initializer_base(&self, node: &SyntaxNode) -> Indent {
        let is_initializer = node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                S::FIELD_DECL | S::LOCAL_VAR_DECL | S::RESOURCE
            )
        });
        if is_initializer {
            Indent::columns(-i32::try_from(self.style.continuation_cols).unwrap_or(0))
        } else {
            Indent::ZERO
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
        // Same refinement an argument list gets: one element too wide to pack sends the whole
        // initializer one element per line (`hasOnlyShortItems` in `visitArrayInitializer`).
        let policy = self.style.cfg.wrapping.array_initializer;
        let policy = if policy == WrapPolicy::IfLong && !self.fills(node) {
            WrapPolicy::IfLongPerItem
        } else {
            policy
        };
        let indent = self.style.indent();
        let children = Self::children(node);
        let empty = !children.iter().any(|child| child.as_node().is_some());
        // A trailing comma is the author saying "one item per row" — google-java-format keeps
        // such an initializer vertical whether or not it would fit. The comma is a significant
        // token that survives either way; only the layout responds to it.
        let trailing_comma = Self::has_trailing_comma(&children);
        let last_element = children.iter().rposition(|child| child.as_node().is_some());
        let base = self.initializer_base(node);

        self.open_flat(base.clone());
        let mut opened = false;
        for (nth, child) in children.iter().enumerate() {
            match child.as_token().map(SyntaxToken::kind) {
                Some(S::LBRACE) => {
                    self.brace_before(self.style.cfg.braces.array_initializer);
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
                        let flat = Self::flat_space(self.style.cfg.spacing.after_comma);
                        self.list_break_flat(policy, flat, Indent::ZERO);
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
    /// vertical, negotiable otherwise. Its flat form carries `within-array-initializer-braces`.
    fn edge_break(&mut self, vertical: bool) {
        if vertical {
            self.forced_break(Indent::ZERO);
            return;
        }
        let flat = Self::flat_space(self.style.cfg.spacing.within_array_initializer_braces);
        self.ops
            .brk(crate::ir::FillMode::Unified, flat, Indent::ZERO, None);
        self.space_already_emitted();
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
        let mut elements = node.children().peekable();
        let Some(first) = elements.peek() else {
            return false;
        };
        let Some(start) = Self::source_column(first) else {
            return false;
        };
        let mut rows: Vec<usize> = Vec::new();
        for element in node.children() {
            let leaves = Self::leaf_count(&element);
            let column = Self::source_column(&element);
            if rows.is_empty() {
                rows.push(leaves);
                continue;
            }
            match column {
                // A row continues while its elements sit *past* the column the rows begin at.
                Some(column) if column > start => {
                    if let Some(last) = rows.last_mut() {
                        *last += leaves;
                    }
                }
                Some(column) if column == start => rows.push(leaves),
                // An element to the left of the first one is not a grid at all.
                _ => return false,
            }
        }
        let Some((last, full)) = rows.split_last() else {
            return false;
        };
        // Two rows are already a table; only the *last* one may be short.
        !full.is_empty()
            && full[0] > 1
            && full.iter().all(|row| *row == full[0])
            && *last <= full[0]
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

    /// The source column an element starts at, in display columns from the line's start.
    ///
    /// A table's rows all start at the *same* column. That is the test google-java-format's
    /// `argumentsAreTabular` applies (`actualColumn`), and it is what tells a grid apart from a
    /// right-aligned list: the `{"a", x}, {"bb", y}` of a real table line up, while
    /// `      "a", x,` / `    "bbb", y,` does not — and google-java-format lays the second one out
    /// one element per line.
    fn source_column(node: &SyntaxNode) -> Option<usize> {
        let first = Ctx::first_token(node)?;
        let mut column = 0usize;
        let mut cursor = first.prev_token();
        while let Some(previous) = cursor {
            if previous.kind() == S::NEWLINE {
                return Some(column);
            }
            column += crate::ir::Width::utf16(previous.text());
            cursor = previous.prev_token();
        }
        Some(column)
    }

    /// Emit a grid-shaped initializer, keeping the source's row breaks.
    async fn emit_tabular_array(&mut self, node: &SyntaxNode) {
        let indent = self.style.indent();
        let base = self.initializer_base(node);
        self.open_flat(base.clone());
        let mut opened = false;
        for child in Self::children(node) {
            match child.as_token().map(SyntaxToken::kind) {
                Some(S::LBRACE) => {
                    self.brace_before(self.style.cfg.braces.array_initializer);
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
}
