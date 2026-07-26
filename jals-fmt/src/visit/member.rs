//! Type bodies and their members: fields, methods, constructors, initializers, enum constants.
//!
//! Every braced body shares one shape — `{`, an indented run of items separated by enforced
//! blank lines, `}` — so it shares one helper. What differs between a class body, a method body,
//! and a lambda body is which `[braces] keep-*-on-one-line` rule decides whether the body may
//! collapse, and which `[blank-lines]` counts apply.

use alloc::vec::Vec;

use jals_config::fmt::{KeepOnOneLine, WrapPolicy};
use jals_syntax::{SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::ir::Indent;
use crate::visit::Ctx;

impl Ctx<'_> {
    /// A class / interface / record / `@interface` body.
    pub(super) async fn visit_class_body(&mut self, node: &SyntaxNode) {
        let members: Vec<SyntaxNode> = node.children().collect();
        let keep = self.keep_for_type_body(node);
        self.emit_member_body(node, &members, keep).await;
    }

    /// Which one-line rule a type body follows, by the declaration that owns it.
    fn keep_for_type_body(&self, node: &SyntaxNode) -> KeepOnOneLine {
        let braces = &self.style.cfg.braces;
        match node.parent().map(|parent| parent.kind()) {
            Some(S::RECORD_DECL) => braces.keep_record_declaration_on_one_line,
            Some(S::ANNOTATION_TYPE_DECL) => braces.keep_annotation_declaration_on_one_line,
            Some(S::ENUM_DECL) => braces.keep_enum_declaration_on_one_line,
            _ => braces.keep_type_body_on_one_line,
        }
    }

    /// The shared braced-body emitter.
    ///
    /// An empty body collapses to `{}` unless the rule is `never`; a body holding only comments
    /// is never empty, because the comments have to land somewhere.
    async fn emit_member_body(
        &mut self,
        node: &SyntaxNode,
        members: &[SyntaxNode],
        keep: KeepOnOneLine,
    ) {
        let lbrace = Self::token_of(node, S::LBRACE);
        let rbrace = Self::token_of(node, S::RBRACE);
        let dangling = rbrace
            .as_ref()
            .is_some_and(|brace| self.comments.has_dangling(brace));

        if members.is_empty() && !dangling {
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
        let collapsible = Self::collapses(members.len(), dangling, keep);
        let indent = if self.style.cfg.layout.indent_type_members {
            self.style.indent()
        } else {
            Indent::ZERO
        };

        // Both braces and the members share **one** level, so "collapse when it fits" is a
        // decision the engine can actually make. Split across two levels, the closing brace would
        // belong to the enclosing level and break there whatever the contents did.
        self.open_flat(Indent::ZERO);
        if let Some(brace) = &lbrace {
            self.token(brace);
        }
        self.open(indent.clone());
        for (nth, member) in members.iter().enumerate() {
            let enforced = if nth == 0 {
                blank.at_type_body_start
            } else {
                self.enforced_around_member(member, node)
            };
            let source = self
                .blank_lines_before(member)
                .min(blank.max_in_declarations);
            self.body_break(collapsible, enforced.max(source), Indent::ZERO);
            self.visit(member).await;
        }
        self.close_indent(&indent);

        if let Some(brace) = &rbrace {
            let trailing = blank.at_type_body_end.max(
                self.blank_lines_before_token(brace)
                    .min(blank.max_before_closing_brace),
            );
            self.body_break(collapsible, trailing, Indent::ZERO);
            self.token(brace);
        }
        self.close();
    }

    /// Whether a body of `items` may share a line, given the rule and whether comments dangle.
    ///
    /// A body carrying a comment never collapses: a `//` would swallow the closing brace, and a
    /// block comment moved onto the header's line changes what it appears to describe.
    fn collapses(items: usize, dangling: bool, keep: KeepOnOneLine) -> bool {
        if dangling {
            return false;
        }
        match keep {
            KeepOnOneLine::Never => false,
            KeepOnOneLine::IfEmpty => items == 0,
            // `preserve` is rounded to `if-single-item` in `Style::reify`, so both land here.
            KeepOnOneLine::IfSingleItem | KeepOnOneLine::Preserve => items <= 1,
            KeepOnOneLine::Always => true,
        }
    }

    /// A break inside a body: negotiable when the body may collapse, forced otherwise.
    fn body_break(&mut self, collapsible: bool, blanks: usize, plus_indent: Indent) {
        if collapsible && blanks == 0 {
            self.break_op(plus_indent);
        } else {
            self.blank_lines(blanks, plus_indent);
        }
    }

    /// The blank lines `[blank-lines]` enforces around a member.
    fn enforced_around_member(&self, member: &SyntaxNode, body: &SyntaxNode) -> usize {
        let blank = &self.style.cfg.blank_lines;
        let in_interface = body
            .parent()
            .is_some_and(|parent| parent.kind() == S::INTERFACE_DECL);
        match member.kind() {
            S::FIELD_DECL => {
                if in_interface {
                    blank.around_field_in_interface
                } else {
                    blank.around_field
                }
            }
            S::METHOD_DECL | S::CONSTRUCTOR_DECL => {
                if in_interface {
                    blank.around_method_in_interface
                } else {
                    blank.around_method
                }
            }
            S::INITIALIZER => blank.around_initializer,
            S::CLASS_DECL
            | S::INTERFACE_DECL
            | S::ENUM_DECL
            | S::RECORD_DECL
            | S::ANNOTATION_TYPE_DECL => blank.around_type,
            _ => 0,
        }
    }

    /// Blank lines the source had before a token.
    fn blank_lines_before_token(&self, tok: &SyntaxToken) -> usize {
        if !self.comments.leading(tok).is_empty() {
            return self.comments.blank_lines_before(tok);
        }
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

    /// An `enum` body: the constant list, then an optional `;` and member section.
    ///
    /// google-java-format treats an enum with no members and no documented constant as an array
    /// initializer — it goes on one line if it fits. That is why the constant list is its own
    /// level rather than a forced-break run.
    pub(super) async fn visit_enum_body(&mut self, node: &SyntaxNode) {
        let constants: Vec<SyntaxNode> = node
            .children()
            .filter(|child| child.kind() == S::ENUM_CONSTANT)
            .collect();
        let members: Vec<SyntaxNode> = node
            .children()
            .filter(|child| child.kind() != S::ENUM_CONSTANT)
            .collect();
        let trivial = members.is_empty()
            && constants
                .iter()
                .all(|constant| Self::child_of(constant, S::CLASS_BODY).is_none());
        let policy = self.style.cfg.wrapping.enum_constants;
        let blank = self.style.cfg.blank_lines;
        let indent = self.style.indent();

        let mut opened = false;
        let mut past_constants = false;
        // One level for the whole body, so a trivial enum can collapse to `{A, B, C}` the way
        // google-java-format lays out an array initializer.
        self.open_flat(Indent::ZERO);
        for child in Self::children(node) {
            match child.as_token().map(|tok| tok.kind()) {
                Some(S::LBRACE) => {
                    self.visit_element(&child).await;
                    self.open(indent.clone());
                    opened = true;
                    if !constants.is_empty() || !members.is_empty() {
                        self.enum_break(trivial, Indent::ZERO);
                    }
                    continue;
                }
                Some(S::RBRACE) => {
                    if opened {
                        self.close_indent(&indent);
                        opened = false;
                    }
                    if trivial && members.is_empty() {
                        self.break_tight(Indent::ZERO);
                    } else {
                        self.forced_break(Indent::ZERO);
                    }
                    self.visit_element(&child).await;
                    continue;
                }
                Some(S::COMMA) => {
                    self.visit_element(&child).await;
                    if trivial {
                        self.list_break(policy, Indent::ZERO);
                    } else {
                        self.forced_break(Indent::ZERO);
                    }
                    continue;
                }
                Some(S::SEMICOLON) => {
                    self.visit_element(&child).await;
                    past_constants = true;
                    continue;
                }
                _ => {}
            }
            let Some(member) = child.as_node() else {
                self.visit_element(&child).await;
                continue;
            };
            if past_constants {
                let enforced = self.enforced_around_member(member, node);
                let source = self
                    .blank_lines_before(member)
                    .min(blank.max_in_declarations);
                self.blank_lines(enforced.max(source), Indent::ZERO);
            }
            self.visit(member).await;
        }
        if opened {
            self.close_indent(&indent);
        }
        self.close();
    }

    /// The break between an enum's `{` and its first constant.
    fn enum_break(&mut self, trivial: bool, plus_indent: Indent) {
        if trivial {
            self.break_tight(plus_indent);
        } else {
            self.forced_break(plus_indent);
        }
    }

    /// One enum constant: `NAME(args)` with an optional class body.
    pub(super) async fn visit_enum_constant(&mut self, node: &SyntaxNode) {
        for child in Self::children(node) {
            if child
                .as_node()
                .is_some_and(|child| child.kind() == S::CLASS_BODY)
            {
                self.brace_before(self.style.cfg.braces.type_declaration);
            }
            self.visit_element(&child).await;
        }
    }

    /// A field declaration: modifiers, a type, one or more declarators, `;`.
    pub(super) async fn visit_field(&mut self, node: &SyntaxNode) {
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        self.emit_declarators(node).await;
        self.close_indent(&continuation);
    }

    /// The `type name = init, name2 = init2` part shared by fields, locals, and resources.
    ///
    /// The break falls *after* `=` (google-java-format's assignment rule), so the initializer
    /// starts the continuation line; `before-assignment-operator` moves it to the front instead.
    pub(super) async fn emit_declarators(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.assignment;
        let before = self.style.cfg.wrapping.before_assignment_operator;
        let children = Self::children(node);
        for (nth, child) in children.iter().enumerate() {
            let kind = child.as_token().map(|tok| tok.kind());
            if kind == Some(S::EQ) {
                if before {
                    self.list_break(policy, Indent::ZERO);
                }
                self.visit_element(child).await;
                if !before {
                    self.list_break(policy, Indent::ZERO);
                }
                continue;
            }
            if nth > 0
                && matches!(
                    children[nth - 1].as_token().map(|tok| tok.kind()),
                    Some(S::COMMA)
                )
            {
                self.list_break(WrapPolicy::IfLong, Indent::ZERO);
            }
            self.visit_element(child).await;
        }
    }

    /// A method or constructor declaration.
    ///
    /// The signature groups without indenting, for the same reason a type header does: the
    /// parameter list and the `throws` clause each carry the continuation indent on their own
    /// break.
    pub(super) async fn visit_method(&mut self, node: &SyntaxNode) {
        self.open_flat(Indent::ZERO);
        let mut header_open = true;
        for child in Self::children(node) {
            let is_body = child
                .as_node()
                .is_some_and(|child| child.kind() == S::BLOCK);
            if is_body && header_open {
                header_open = false;
                self.close();
                self.brace_before(self.style.cfg.braces.method_declaration);
            }
            self.visit_element(&child).await;
        }
        if header_open {
            self.close();
        }
    }

    /// An instance or `static` initializer block.
    pub(super) async fn visit_initializer(&mut self, node: &SyntaxNode) {
        for child in Self::children(node) {
            if child
                .as_node()
                .is_some_and(|child| child.kind() == S::BLOCK)
            {
                self.brace_before(self.style.cfg.braces.method_declaration);
            }
            self.visit_element(&child).await;
        }
    }

    /// An annotation element's `default value`.
    pub(super) async fn visit_annotation_default(&mut self, node: &SyntaxNode) {
        self.visit_children(node).await;
    }

    /// The one-line rule a block should use, chosen by what owns it.
    pub(super) fn keep_for_block(&self, block: &SyntaxNode) -> KeepOnOneLine {
        let braces = &self.style.cfg.braces;
        match block.parent().map(|parent| parent.kind()) {
            Some(S::METHOD_DECL | S::CONSTRUCTOR_DECL | S::INITIALIZER) => {
                braces.keep_method_body_on_one_line
            }
            Some(S::LAMBDA_EXPR) => braces.keep_lambda_body_on_one_line,
            _ => braces.keep_block_on_one_line,
        }
    }
}
