//! Statements: blocks, control flow, `switch`, and the simple one-liners.
//!
//! # Brace forcing
//!
//! `[braces] force-*` is the one rule in the crate that *adds* significant tokens, and
//! `if-multiline` is the one rule whose condition reads the engine's own output. The engine is a
//! single greedy pass with no return edge, so that condition cannot be evaluated where it is
//! asked; [`Ctx::forces_braces`] answers it from the **source's** line span instead — decided once,
//! before the document exists, and never revisited. Idempotency then holds because the second run
//! sees a body that already has braces and the rule no longer applies (`DESIGN.md` §8.1, §17).

use alloc::vec::Vec;

use jals_config::fmt::{ForceBraces, KeepOnOneLine, WrapPolicy};
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::ir::Indent;
use crate::visit::Ctx;

impl Ctx<'_> {
    /// A `{ … }` block.
    pub(super) async fn visit_block(&mut self, node: &SyntaxNode) {
        let statements: Vec<SyntaxNode> = node.children().collect();
        let keep = self.keep_for_block(node);
        let lbrace = Self::token_of(node, S::LBRACE);
        let rbrace = Self::token_of(node, S::RBRACE);

        let dangling = rbrace
            .as_ref()
            .is_some_and(|brace| self.comments.has_dangling(brace));
        if statements.is_empty() && !dangling {
            if let Some(brace) = &lbrace {
                self.token(brace);
            }
            if keep == KeepOnOneLine::Never {
                self.forced_break(Indent::ZERO);
            }
            if let Some(brace) = &rbrace {
                self.token(brace);
            }
            return;
        }

        let blank = self.style.cfg.blank_lines;
        let collapsible = Self::block_collapses(statements.len(), dangling, keep);
        let indent = self.style.indent();
        let is_method_body = matches!(
            node.parent().map(|parent| parent.kind()),
            Some(S::METHOD_DECL | S::CONSTRUCTOR_DECL)
        );

        // Both braces and the statements share one level — see `emit_member_body`.
        self.open_flat(Indent::ZERO);
        if let Some(brace) = &lbrace {
            self.token(brace);
        }
        self.open(indent.clone());
        for (nth, statement) in statements.iter().enumerate() {
            let enforced = if nth == 0 {
                if is_method_body {
                    blank.before_method_body
                } else {
                    blank.at_block_start
                }
            } else {
                0
            };
            let source = self.blank_lines_before(statement).min(blank.max_in_code);
            let element = SyntaxElement::Node(statement.clone());
            if let Some(at) = self.disabled_region_of(&element) {
                if self.take_disabled_region(at) {
                    self.block_break(collapsible, enforced.max(source));
                    self.emit_disabled(at);
                }
                continue;
            }
            self.block_break(collapsible, enforced.max(source));
            self.visit(statement).await;
        }
        // A comment written just before the closing brace documents the block, so it keeps the
        // statements' indent (see [`Ctx::hoist_comments_before`]).
        let dangling_before_brace = rbrace
            .as_ref()
            .is_some_and(|brace| self.hoist_comments_before(brace));
        self.close_indent(&indent);

        if let Some(brace) = &rbrace {
            // A block another clause follows may keep a blank line above its `}`:
            // `visitTry` passes `AllowTrailingBlankLine.valueOf(trailingClauses)`, and the same
            // holds for a `then` branch an `else` follows. The last clause of the chain does not.
            let cap = if Self::followed_by_clause(node) {
                blank.max_before_closing_brace.max(1)
            } else {
                blank.max_before_closing_brace
            };
            let trailing = if dangling_before_brace {
                0
            } else {
                blank
                    .at_block_end
                    .max(Self::source_blank_lines_of(brace).min(cap))
            };
            self.block_break(collapsible, trailing);
            self.token(brace);
        }
        self.close();
    }

    /// Whether another clause of the same statement follows this block.
    ///
    /// A `try` with a `catch` after it, a `catch` with another `catch` or a `finally`, a `then`
    /// branch with an `else`. The last clause of a chain has nothing after it and closes tight.
    fn followed_by_clause(block: &SyntaxNode) -> bool {
        let Some(parent) = block.parent() else {
            return false;
        };
        let after = |from: &SyntaxNode| {
            let start = from.text_range().end();
            from.parent().is_some_and(|owner| {
                owner
                    .children_with_tokens()
                    .filter(|sibling| sibling.text_range().start() >= start)
                    .any(|sibling| match sibling {
                        SyntaxElement::Node(node) => {
                            matches!(node.kind(), S::CATCH_CLAUSE | S::FINALLY_CLAUSE)
                        }
                        SyntaxElement::Token(tok) => tok.kind() == S::ELSE_KW,
                    })
            })
        };
        match parent.kind() {
            S::TRY_STMT | S::IF_STMT => after(block),
            // A `catch`'s own block asks about the clause, not about itself.
            S::CATCH_CLAUSE => after(&parent),
            _ => false,
        }
    }

    /// Blank lines the source had before a token, ignoring comments.
    fn source_blank_lines_of(tok: &jals_syntax::SyntaxToken) -> usize {
        let mut newlines = 0usize;
        let mut cursor = tok.prev_token();
        while let Some(previous) = cursor {
            match previous.kind() {
                S::NEWLINE => newlines += 1,
                S::WHITESPACE => {}
                _ => break,
            }
            cursor = previous.prev_token();
        }
        newlines.saturating_sub(1)
    }

    /// Whether a block of `items` statements may share a line.
    const fn block_collapses(items: usize, dangling: bool, keep: KeepOnOneLine) -> bool {
        if dangling {
            return false;
        }
        match keep {
            KeepOnOneLine::Never => false,
            KeepOnOneLine::IfEmpty => items == 0,
            KeepOnOneLine::IfSingleItem | KeepOnOneLine::Preserve => items <= 1,
            KeepOnOneLine::Always => true,
        }
    }

    /// A break between two statements: negotiable when the block may collapse, forced otherwise.
    fn block_break(&mut self, collapsible: bool, blanks: usize) {
        if collapsible && blanks == 0 {
            self.break_op(Indent::ZERO);
        } else {
            self.blank_lines(blanks, Indent::ZERO);
        }
    }

    /// A local variable declaration.
    ///
    /// google-java-format never splits one declaration into several; the declarators wrap as a
    /// list and the initializer breaks after `=`.
    pub(super) async fn visit_local_var(&mut self, node: &SyntaxNode) {
        // The modifiers go outside the continuation level, for the reason `visit_field` gives.
        if let Some(modifiers) = Self::child_of(node, S::MODIFIERS) {
            self.visit(&modifiers).await;
        }
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        self.emit_declarators(node).await;
        self.close_indent(&continuation);
    }

    /// `expr;`, `return expr;`, `throw expr;`, `yield expr;`.
    ///
    /// No level of its own: the statement is already one split of the enclosing block's level, so
    /// the engine measures it as a unit either way — and a level here would add a *second*
    /// continuation indent on top of the one the expression inside opens, pushing every wrapped
    /// argument list eight columns instead of four.
    pub(super) async fn visit_simple_stmt(&mut self, node: &SyntaxNode) {
        self.visit_children(node).await;
    }

    /// `if (cond) then [else …]`.
    pub(super) async fn visit_if(&mut self, node: &SyntaxNode) {
        let children = Self::children(node);
        let force = self.style.cfg.braces.force_if;
        let mut seen_condition = false;

        for (nth, child) in children.iter().enumerate() {
            let kind = child.as_token().map(SyntaxToken::kind);
            if kind == Some(S::RPAREN) {
                self.visit_element(child).await;
                seen_condition = true;
                continue;
            }
            if kind == Some(S::ELSE_KW) {
                self.continuation_keyword(
                    self.style.cfg.braces.else_on_new_line,
                    children.get(nth.wrapping_sub(1)),
                );
                self.visit_element(child).await;
                continue;
            }
            if let Some(branch) = child.as_node()
                && (seen_condition || Self::is_after_else(&children, nth))
                && Self::is_statement(branch.kind())
            {
                self.emit_branch(branch, force, Self::is_after_else(&children, nth))
                    .await;
                continue;
            }
            self.visit_element(child).await;
        }
    }

    /// Whether the child at `nth` follows an `else` keyword.
    fn is_after_else(children: &[SyntaxElement], nth: usize) -> bool {
        nth > 0
            && children[..nth]
                .iter()
                .rev()
                .find_map(|child| child.as_token().map(SyntaxToken::kind))
                == Some(S::ELSE_KW)
    }

    /// Whether a node kind is a statement that can be a control-flow body.
    const fn is_statement(kind: S) -> bool {
        matches!(
            kind,
            S::BLOCK
                | S::EXPR_STMT
                | S::RETURN_STMT
                | S::THROW_STMT
                | S::YIELD_STMT
                | S::IF_STMT
                | S::WHILE_STMT
                | S::DO_WHILE_STMT
                | S::FOR_STMT
                | S::FOR_EACH_STMT
                | S::TRY_STMT
                | S::SWITCH_STMT
                | S::SYNCHRONIZED_STMT
                | S::LOCAL_VAR_DECL
                | S::BREAK_STMT
                | S::CONTINUE_STMT
                | S::ASSERT_STMT
                | S::LABELED_STMT
                | S::EMPTY_STMT
        )
    }

    /// Emit a control-flow body, adding braces when `[braces] force-*` asks for them.
    ///
    /// `compact-else-if` is what keeps `else if` on one line instead of nesting the inner `if`
    /// a level deeper, so an `else`-owned `IF_STMT` is emitted inline rather than as a body.
    async fn emit_branch(&mut self, branch: &SyntaxNode, force: ForceBraces, after_else: bool) {
        if after_else && branch.kind() == S::IF_STMT && self.style.cfg.braces.compact_else_if {
            self.space();
            self.visit(branch).await;
            return;
        }
        if branch.kind() == S::BLOCK {
            self.brace_before(self.style.cfg.braces.block);
            self.visit(branch).await;
            return;
        }
        if Self::forces_braces(branch, force) {
            self.brace_before(self.style.cfg.braces.block);
            self.space_if(self.style.cfg.spacing.before_left_brace);
            self.synthetic("{");
            let indent = self.style.indent();
            self.open(indent.clone());
            self.forced_break(Indent::ZERO);
            self.visit(branch).await;
            self.close_indent(&indent);
            self.forced_break(Indent::ZERO);
            self.synthetic("}");
            self.braced_branch = true;
            return;
        }
        self.emit_braceless_body(branch).await;
    }

    /// A braceless body: on the header's line when `keep-control-statement-on-one-line` allows
    /// it, otherwise indented on its own line.
    async fn emit_braceless_body(&mut self, branch: &SyntaxNode) {
        let indent = self.style.indent();
        if self.style.cfg.braces.keep_control_statement_on_one_line {
            self.open(indent.clone());
            self.break_op(Indent::ZERO);
            self.visit(branch).await;
            self.close_indent(&indent);
            return;
        }
        self.open(indent.clone());
        self.forced_break(Indent::ZERO);
        self.visit(branch).await;
        self.close_indent(&indent);
    }

    /// Whether `[braces] force-*` inserts braces around this body.
    ///
    /// `if-multiline` asks whether the statement spans more than one line, which the engine has
    /// not decided yet; the **source's** span answers it instead. Deciding it before the document
    /// exists is what keeps the engine a single forward pass.
    fn forces_braces(branch: &SyntaxNode, force: ForceBraces) -> bool {
        match force {
            ForceBraces::Never => false,
            ForceBraces::Always => true,
            // Only the body's **interior** line span counts — the source text strictly between
            // its first and last significant token. The span of the whole *statement* would flip
            // on the second run, because the first run already moved a braceless body onto its
            // own line; the interior span does not, so `fmt ∘ fmt = fmt` holds.
            ForceBraces::IfMultiline => Self::spans_lines(branch),
        }
    }

    /// Whether a node's own tokens are spread over more than one source line.
    fn spans_lines(node: &SyntaxNode) -> bool {
        let mut tokens = node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| !tok.kind().is_trivia());
        let Some(first) = tokens.next() else {
            return false;
        };
        let mut cursor = first;
        for next in tokens {
            let mut between = cursor.next_token();
            while let Some(tok) = between {
                if tok == next {
                    break;
                }
                if tok.kind() == S::NEWLINE {
                    return true;
                }
                between = tok.next_token();
            }
            cursor = next;
        }
        false
    }

    /// Emit the separation before a continuation keyword (`else`, `catch`, `finally`, `while`).
    fn continuation_keyword(&mut self, on_new_line: bool, previous: Option<&SyntaxElement>) {
        // A branch this run put braces around ends in `}` just like a source block does.
        let after_brace = core::mem::take(&mut self.braced_branch)
            || previous
                .and_then(SyntaxElement::as_node)
                .is_some_and(|node| node.kind() == S::BLOCK)
            || previous
                .and_then(SyntaxElement::as_token)
                .is_some_and(|tok| tok.kind() == S::RBRACE);
        if on_new_line || !after_brace {
            self.forced_break(Indent::ZERO);
        }
    }

    /// `while (cond) body`.
    pub(super) async fn visit_while(&mut self, node: &SyntaxNode) {
        self.visit_loop(node, self.style.cfg.braces.force_while)
            .await;
    }

    /// `for (init; cond; update) body`.
    pub(super) async fn visit_for(&mut self, node: &SyntaxNode) {
        let force = self.style.cfg.braces.force_for;
        let policy = self.style.cfg.wrapping.for_statement;
        let continuation = self.style.continuation();
        let children = Self::children(node);
        let mut in_header = false;

        for child in &children {
            let kind = child.as_token().map(SyntaxToken::kind);
            match kind {
                Some(S::LPAREN) => {
                    self.visit_element(child).await;
                    self.open(continuation.clone());
                    in_header = true;
                    continue;
                }
                Some(S::SEMICOLON) if in_header => {
                    self.visit_element(child).await;
                    let flat = Self::flat_space(self.style.cfg.spacing.after_semicolon);
                    self.list_break_flat(policy, flat, Indent::ZERO);
                    continue;
                }
                Some(S::RPAREN) if in_header => {
                    self.close_indent(&continuation);
                    in_header = false;
                    self.visit_element(child).await;
                    continue;
                }
                _ => {}
            }
            if let Some(body) = child.as_node()
                && !in_header
                && Self::is_statement(body.kind())
            {
                self.emit_branch(body, force, false).await;
                continue;
            }
            self.visit_element(child).await;
        }
        if in_header {
            self.close_indent(&continuation);
        }
    }

    /// `for (T x : xs) body`. The `:` is an assignment-shaped separator, so the break falls after
    /// it — the same rule `=` follows.
    pub(super) async fn visit_for_each(&mut self, node: &SyntaxNode) {
        // `visitEnhancedForLoop` declares the variable with `:` as its `=`, so the sequence gets
        // a level of its own and moves down whole rather than breaking inside itself.
        let force = self.style.cfg.braces.force_for;
        let continuation = self.style.continuation();
        let mut past_header = false;
        let mut opened = false;
        for child in Self::children(node) {
            if child.as_token().is_some_and(|tok| tok.kind() == S::COLON) {
                self.visit_element(&child).await;
                self.open(continuation.clone());
                opened = true;
                let flat = Self::flat_space(self.style.cfg.spacing.after_foreach_colon);
                self.list_break_flat(self.style.cfg.wrapping.for_statement, flat, Indent::ZERO);
                continue;
            }
            if child.as_token().is_some_and(|tok| tok.kind() == S::RPAREN) {
                if opened {
                    self.close_indent(&continuation);
                    opened = false;
                }
                self.visit_element(&child).await;
                past_header = true;
                continue;
            }
            if let Some(body) = child.as_node()
                && past_header
                && Self::is_statement(body.kind())
            {
                self.emit_branch(body, force, false).await;
                continue;
            }
            self.visit_element(&child).await;
        }
        if opened {
            self.close_indent(&continuation);
        }
    }

    /// The shared `header (…) body` shape of `while` and `for`-each.
    async fn visit_loop(&mut self, node: &SyntaxNode, force: ForceBraces) {
        let mut past_header = false;
        for child in Self::children(node) {
            if child.as_token().is_some_and(|tok| tok.kind() == S::RPAREN) {
                self.visit_element(&child).await;
                past_header = true;
                continue;
            }
            if let Some(body) = child.as_node()
                && past_header
                && Self::is_statement(body.kind())
            {
                self.emit_branch(body, force, false).await;
                continue;
            }
            self.visit_element(&child).await;
        }
    }

    /// `do body while (cond);`.
    pub(super) async fn visit_do_while(&mut self, node: &SyntaxNode) {
        let force = self.style.cfg.braces.force_do_while;
        let children = Self::children(node);
        for (nth, child) in children.iter().enumerate() {
            if child
                .as_token()
                .is_some_and(|tok| tok.kind() == S::WHILE_KW)
            {
                self.continuation_keyword(
                    self.style.cfg.braces.while_on_new_line,
                    children.get(nth.wrapping_sub(1)),
                );
                self.visit_element(child).await;
                continue;
            }
            if let Some(body) = child.as_node()
                && nth == 1
                && Self::is_statement(body.kind())
            {
                self.emit_branch(body, force, false).await;
                continue;
            }
            self.visit_element(child).await;
        }
    }

    /// A `try`-with-resources list. Its separator is `;`, so the shared comma-list emitter does
    /// not see it; the wrap policy and the paren positions are otherwise the same decision.
    pub(super) async fn visit_resource_list(&mut self, node: &SyntaxNode) {
        let policy = self.resource_policy();
        let children = Self::children(node);
        let last = children.iter().rposition(|child| child.as_node().is_some());
        // One resource carries no continuation indent of the list's own: whatever it wraps to is
        // its own business. Several do, so the second and later ones line up under the first.
        let several = children
            .iter()
            .filter(|child| child.as_node().is_some())
            .count()
            > 1;
        let indent = if several {
            self.style.continuation()
        } else {
            Indent::ZERO
        };
        let mut opened = false;
        for (nth, child) in children.iter().enumerate() {
            match child.as_token().map(SyntaxToken::kind) {
                Some(S::LPAREN) => {
                    self.visit_element(child).await;
                    self.open(indent.clone());
                    opened = true;
                    // No break after the `(`: a resource list is a statement header, so its first
                    // resource stays on the `try` line however it wraps, exactly as `if (…)`
                    // keeps its condition there. google-java-format's `visitTry` opens its level
                    // straight after the token.
                    continue;
                }
                Some(S::RPAREN) => {
                    if opened {
                        self.close_indent(&indent);
                        opened = false;
                    }
                    self.visit_element(child).await;
                    continue;
                }
                Some(S::SEMICOLON) => {
                    self.visit_element(child).await;
                    if last.is_some_and(|last| nth < last) {
                        let flat = Self::flat_space(self.style.cfg.spacing.after_semicolon);
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
    }

    /// `try (resources) { … } catch … finally …`.
    pub(super) async fn visit_try(&mut self, node: &SyntaxNode) {
        for child in Self::children(node) {
            if let Some(block) = child.as_node()
                && block.kind() == S::BLOCK
            {
                self.brace_before(self.style.cfg.braces.block);
            }
            self.visit_element(&child).await;
        }
    }

    /// A `catch` clause, preceded by its own line when `catch-on-new-line` asks.
    pub(super) async fn visit_catch(&mut self, node: &SyntaxNode) {
        self.clause_keyword(self.style.cfg.braces.catch_on_new_line);
        let policy = self.style.cfg.wrapping.multi_catch_types;
        let continuation = self.style.continuation();
        let flat = Self::flat_space(self.style.cfg.spacing.around_type_bounds);
        // The header is a level of its own. Sharing one with the body would put the body's forced
        // breaks in the same split as the union's, and a split that can never fit takes every
        // break before it — so `catch (A | B e)` broke at the `|` however short it was.
        self.open_flat(Indent::ZERO);
        let mut header_open = true;
        for child in Self::children(node) {
            if let Some(block) = child.as_node()
                && block.kind() == S::BLOCK
            {
                if header_open {
                    self.close();
                    header_open = false;
                }
                self.brace_before(self.style.cfg.braces.block);
            }
            // `catch (A | B | C e)` — the union is a list like any other, with `|` in the
            // separator's place.
            if child.as_token().is_some_and(|tok| tok.kind() == S::PIPE) {
                self.list_break_flat(policy, flat, continuation.clone());
                self.visit_element(&child).await;
                continue;
            }
            self.visit_element(&child).await;
        }
        if header_open {
            self.close();
        }
    }

    /// A `finally` clause.
    pub(super) async fn visit_finally(&mut self, node: &SyntaxNode) {
        self.clause_keyword(self.style.cfg.braces.finally_on_new_line);
        for child in Self::children(node) {
            if let Some(block) = child.as_node()
                && block.kind() == S::BLOCK
            {
                self.brace_before(self.style.cfg.braces.block);
            }
            self.visit_element(&child).await;
        }
    }

    /// The separation before a `catch` / `finally` that follows a closing brace.
    fn clause_keyword(&mut self, on_new_line: bool) {
        if on_new_line {
            self.forced_break(Indent::ZERO);
        } else {
            self.space();
        }
    }

    /// `synchronized (lock) { … }`.
    pub(super) async fn visit_synchronized(&mut self, node: &SyntaxNode) {
        for child in Self::children(node) {
            if let Some(block) = child.as_node()
                && block.kind() == S::BLOCK
            {
                self.brace_before(self.style.cfg.braces.block);
            }
            self.visit_element(&child).await;
        }
    }

    /// A `switch` statement or expression.
    pub(super) async fn visit_switch(&mut self, node: &SyntaxNode) {
        for child in Self::children(node) {
            if let Some(block) = child.as_node()
                && block.kind() == S::SWITCH_BLOCK
            {
                self.brace_before(self.style.cfg.braces.switch);
            }
            self.visit_element(&child).await;
        }
    }

    /// A `switch` body: one rule or group per line, indented per `indent-switch-labels`.
    pub(super) async fn visit_switch_block(&mut self, node: &SyntaxNode) {
        let arms: Vec<SyntaxNode> = node.children().collect();
        let keep = self.style.cfg.braces.keep_switch_body_on_one_line;
        let lbrace = Self::token_of(node, S::LBRACE);
        let rbrace = Self::token_of(node, S::RBRACE);

        let dangling = rbrace
            .as_ref()
            .is_some_and(|brace| self.comments.has_dangling(brace));
        if arms.is_empty() && !dangling {
            if let Some(brace) = &lbrace {
                self.token(brace);
            }
            if keep == KeepOnOneLine::Never {
                self.forced_break(Indent::ZERO);
            }
            if let Some(brace) = &rbrace {
                self.token(brace);
            }
            return;
        }

        let indent = if self.style.cfg.layout.indent_switch_labels {
            self.style.indent()
        } else {
            Indent::ZERO
        };
        let between = self.style.cfg.blank_lines.between_switch_groups;
        self.open_flat(Indent::ZERO);
        if let Some(brace) = &lbrace {
            self.token(brace);
        }
        self.open(indent.clone());
        for (nth, arm) in arms.iter().enumerate() {
            let enforced = if nth == 0 { 0 } else { between };
            let source = self
                .blank_lines_before(arm)
                .min(self.style.cfg.blank_lines.max_in_code);
            self.blank_lines(enforced.max(source), Indent::ZERO);
            self.visit(arm).await;
        }
        self.close_indent(&indent);
        if let Some(brace) = &rbrace {
            self.forced_break(Indent::ZERO);
            self.token(brace);
        }
        self.close();
    }

    /// An arrow-form rule: `case L -> body`.
    pub(super) async fn visit_switch_rule(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.switch_expression;
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        for child in Self::children(node) {
            if child.as_token().is_some_and(|tok| tok.kind() == S::ARROW) {
                self.visit_element(&child).await;
                self.list_break(policy, Indent::ZERO);
                continue;
            }
            if let Some(block) = child.as_node()
                && block.kind() == S::BLOCK
            {
                self.close_indent(&continuation);
                self.brace_before(self.style.cfg.braces.block);
                self.visit_element(&child).await;
                return;
            }
            self.visit_element(&child).await;
        }
        self.close_indent(&continuation);
    }

    /// A colon-form group: one or more labels, then its statements indented one level.
    ///
    /// google-java-format never synthesizes a fall-through comment, so neither does this.
    pub(super) async fn visit_switch_group(&mut self, node: &SyntaxNode) {
        let indent = if self.style.cfg.layout.indent_switch_case_body {
            self.style.indent()
        } else {
            Indent::ZERO
        };
        let mut body_open = false;
        let mut first = true;
        for child in Self::children(node) {
            // The `:` belongs to the group, not to the label, so it has to be emitted with the
            // label it terminates rather than treated as the first body statement.
            if child.as_token().is_some_and(|tok| tok.kind() == S::COLON) {
                self.visit_element(&child).await;
                continue;
            }
            if child
                .as_node()
                .is_some_and(|child| child.kind() == S::SWITCH_LABEL)
            {
                if body_open {
                    self.close_indent(&indent);
                    body_open = false;
                }
                // The enclosing switch block already separated this group from the previous one.
                if !first {
                    self.forced_break(Indent::ZERO);
                }
                first = false;
                self.visit_element(&child).await;
                continue;
            }
            if !body_open {
                self.open(indent.clone());
                body_open = true;
            }
            first = false;
            // A group's statements keep the blank lines between them, like any other run of
            // statements: `visitStatements` asks for `BlankLineWanted.PRESERVE`.
            let blanks = child.as_node().map_or(0, |statement| {
                self.blank_lines_before(statement)
                    .min(self.style.cfg.blank_lines.max_in_code)
            });
            self.blank_lines(blanks, Indent::ZERO);
            self.visit_element(&child).await;
        }
        if body_open {
            self.close_indent(&indent);
        }
    }

    /// A `case` / `default` label, whose constant list wraps under `case-labels`.
    pub(super) async fn visit_switch_label(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.case_labels;
        // The label list groups without indenting — `visitCase`'s `builder.open(ZERO)`. The arm
        // itself already opened the continuation the labels break into, and adding another here
        // would indent a deconstruction pattern's components twice over.
        self.open_flat(Indent::ZERO);
        self.emit_comma_list(node, policy, Indent::ZERO).await;
        self.close();
    }

    /// A `when` guard: `case Foo f when f.isBar() ->`.
    pub(super) async fn visit_guard(&mut self, node: &SyntaxNode) {
        // `visitCase` separates the guard from the pattern with a fill break, so it moves onto
        // its own line only when the label would not fit otherwise.
        self.ops
            .brk(crate::ir::FillMode::Independent, " ", Indent::ZERO, None);
        self.space_already_emitted();
        self.visit_children(node).await;
    }

    /// `label: stmt`, indented by `[layout] label-indent`.
    pub(super) async fn visit_labeled(&mut self, node: &SyntaxNode) {
        let label_indent = self.style.cfg.layout.label_indent;
        let indent = Indent::columns(i32::try_from(label_indent).unwrap_or(0));
        let policy = self.style.cfg.wrapping.labeled_statement;
        self.open_flat(indent);
        // The label and its `:` are the level; `labeled-statement` decides whether what they
        // introduce starts a line of its own.
        for child in Self::children(node) {
            let colon = child.as_token().is_some_and(|tok| tok.kind() == S::COLON);
            self.visit_element(&child).await;
            if colon {
                let flat = Self::flat_space(self.style.cfg.spacing.after_label_colon);
                self.list_break_flat(policy, flat, Indent::ZERO);
            }
        }
        self.close();
    }

    /// `assert cond : message;`, breaking before the `:` under `before-assert-colon`.
    pub(super) async fn visit_assert(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.assert_statement;
        let before = self.style.cfg.wrapping.before_assert_colon;
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        for child in Self::children(node) {
            if child.as_token().is_some_and(|tok| tok.kind() == S::COLON) {
                let spacing = &self.style.cfg.spacing;
                if before {
                    let flat = Self::flat_space(spacing.before_assert_colon);
                    self.list_break_flat(policy, flat, Indent::ZERO);
                }
                self.visit_element(&child).await;
                if !before {
                    let flat = Self::flat_space(spacing.after_assert_colon);
                    self.list_break_flat(policy, flat, Indent::ZERO);
                }
                continue;
            }
            self.visit_element(&child).await;
        }
        self.close_indent(&continuation);
    }

    /// The wrap policy a `try`-with-resources list uses.
    pub(super) const fn resource_policy(&self) -> WrapPolicy {
        self.style.cfg.wrapping.resource_list
    }
}
