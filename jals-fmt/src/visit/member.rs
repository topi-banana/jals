//! Type bodies and their members: fields, methods, constructors, initializers, enum constants.
//!
//! Every braced body shares one shape — `{`, an indented run of items separated by enforced
//! blank lines, `}` — so it shares one helper. What differs between a class body, a method body,
//! and a lambda body is which `[braces] keep-*-on-one-line` rule decides whether the body may
//! collapse, and which `[blank-lines]` counts apply.

use alloc::vec::Vec;

use jals_config::fmt::{KeepOnOneLine, WrapPolicy};
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::ir::{FillMode, Indent};
use crate::visit::Ctx;

impl Ctx<'_> {
    /// A class / interface / record / `@interface` body.
    ///
    /// "Members" here means every child between the braces, not just declarations: a stray `;`
    /// is legal in a class body and is a member of the token multiset like any other, so it gets
    /// its own line rather than being quietly dropped.
    pub(super) async fn visit_class_body(&mut self, node: &SyntaxNode) {
        let members: Vec<SyntaxElement> = Self::children(node)
            .into_iter()
            .filter(|child| {
                !matches!(
                    child.as_token().map(SyntaxToken::kind),
                    Some(S::LBRACE | S::RBRACE)
                )
            })
            .collect();
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
        members: &[SyntaxElement],
        keep: KeepOnOneLine,
    ) {
        let lbrace = Self::token_of(node, S::LBRACE);
        let rbrace = Self::token_of(node, S::RBRACE);
        let dangling = rbrace
            .as_ref()
            .is_some_and(|brace| self.comments.has_dangling(brace))
            || lbrace
                .as_ref()
                .is_some_and(|brace| self.comments.touches(brace));

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
        // `around-*` means *around*: the rule separates a member from its neighbour on either
        // side, so the gap before member N answers to N's rule and to N-1's. Reading only N's
        // would put a blank line before a method and none after it.
        let mut previous_rule = 0usize;
        for (nth, member) in members.iter().enumerate() {
            let rule = member
                .as_node()
                .map_or(0, |member| self.enforced_around_member(member, node));
            let enforced = if nth == 0 {
                blank.at_type_body_start
            } else {
                rule.max(previous_rule)
            };
            previous_rule = rule;
            let source = member.as_node().map_or(0, |member| {
                self.blank_lines_before(member)
                    .min(blank.max_in_declarations)
            });
            if let Some(at) = self.disabled_region_of(member) {
                if self.take_disabled_region(at) {
                    self.body_break(collapsible, enforced.max(source), Indent::ZERO);
                    self.emit_disabled(at);
                }
                continue;
            }
            // A stray `;` after a nested type terminates that declaration, so it is written
            // against its closing brace rather than given a line of its own.
            if member
                .as_token()
                .is_some_and(|tok| tok.kind() == S::SEMICOLON)
                && nth > 0
                && members[nth - 1].as_node().is_some_and(|previous| {
                    matches!(
                        previous.kind(),
                        S::CLASS_DECL
                            | S::INTERFACE_DECL
                            | S::ENUM_DECL
                            | S::RECORD_DECL
                            | S::ANNOTATION_TYPE_DECL
                    )
                })
            {
                self.visit_element(member).await;
                continue;
            }
            self.body_break(collapsible, enforced.max(source), Indent::ZERO);
            self.visit_element(member).await;
        }
        // Comments written just before the closing brace stay inside the body level, so they keep
        // the members' indent (see [`Ctx::hoist_comments_before`]).
        let dangling_before_brace = rbrace
            .as_ref()
            .is_some_and(|brace| self.hoist_comments_before(brace));
        self.close_indent(&indent);

        if let Some(brace) = &rbrace {
            let trailing = if dangling_before_brace {
                0
            } else {
                blank.at_type_body_end.max(
                    self.blank_lines_before_token(brace)
                        .min(blank.max_before_closing_brace),
                )
            };
            self.body_break(collapsible, trailing, Indent::ZERO);
            self.token(brace);
        }
        self.close();
    }

    /// Whether a body of `items` may share a line, given the rule and whether comments dangle.
    ///
    /// A body carrying a comment never collapses: a `//` would swallow the closing brace, and a
    /// block comment moved onto the header's line changes what it appears to describe.
    const fn collapses(items: usize, dangling: bool, keep: KeepOnOneLine) -> bool {
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
        // A documented member is separated whatever its kind says — see
        // `[blank-lines] around-documented-member`.
        if self.has_javadoc(member) {
            return blank.around_documented_member;
        }
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

    /// Whether `node` carries any own-line comment above it.
    fn is_documented(&self, node: &SyntaxNode) -> bool {
        Self::first_token(node).is_some_and(|first| !self.comments.leading(&first).is_empty())
    }

    /// Whether `member` carries a Javadoc comment of its own.
    fn has_javadoc(&self, member: &SyntaxNode) -> bool {
        Self::first_token(member).is_some_and(|first| {
            self.comments
                .leading(&first)
                .iter()
                .any(|comment| comment.kind == S::DOC_COMMENT)
        })
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
        let has_members = node
            .children()
            .any(|child| child.kind() != S::ENUM_CONSTANT);
        // An enum with no member section and no constant body is laid out like an array
        // initializer — one line if it fits. A `;` marks a member section even when the section
        // turns out to be empty, and google-java-format keeps such an enum multi-line.
        // A commented constant is never trivial either: an own-line comment has to start a line,
        // so a body holding one cannot collapse — the same reason `Ctx::collapses` refuses a
        // dangling comment.
        let trivial = !has_members
            && Self::token_of(node, S::SEMICOLON).is_none()
            && constants
                .iter()
                .all(|constant| Self::child_of(constant, S::CLASS_BODY).is_none())
            && !constants
                .iter()
                .any(|constant| self.is_documented(constant));
        let policy = self.style.cfg.wrapping.enum_constants;
        let blank = self.style.cfg.blank_lines;
        let indent = self.style.indent();

        let children = Self::children(node);
        let has_content = children.iter().any(|child| {
            !matches!(
                child.as_token().map(SyntaxToken::kind),
                Some(S::LBRACE | S::RBRACE)
            )
        });

        // One level for the whole body, so a trivial enum can collapse to `{A, B, C}` the way
        // google-java-format lays out an array initializer.
        self.open_flat(Indent::ZERO);
        let mut opened = false;
        let mut past_constants = false;
        let mut first_constant = true;
        // Whether a break is owed before the next item. A `;` that directly follows a constant
        // terminates it (`B;`) and takes no break; a further `;` is an item of its own and does.
        let mut pending = false;

        for child in &children {
            match child.as_token().map(SyntaxToken::kind) {
                Some(S::LBRACE) => {
                    self.visit_element(child).await;
                    self.open(indent.clone());
                    opened = true;
                    pending = has_content;
                    continue;
                }
                Some(S::RBRACE) => {
                    // A comment written just before the brace documents the body, so it keeps the
                    // constants' indent — the same rule a class body follows.
                    let dangling = child
                        .as_token()
                        .is_some_and(|brace| self.hoist_comments_before(brace));
                    if opened {
                        self.close_indent(&indent);
                        opened = false;
                    }
                    if has_content && !dangling {
                        self.enum_break(trivial, policy);
                    } else if dangling {
                        self.forced_break(Indent::ZERO);
                    }
                    self.visit_element(child).await;
                    continue;
                }
                Some(S::COMMA) => {
                    self.visit_element(child).await;
                    pending = true;
                    continue;
                }
                Some(S::SEMICOLON) => {
                    if pending {
                        self.enum_break(trivial, policy);
                    }
                    self.visit_element(child).await;
                    past_constants = true;
                    pending = true;
                    continue;
                }
                _ => {}
            }
            let Some(member) = child.as_node() else {
                self.visit_element(child).await;
                pending = false;
                continue;
            };
            if pending {
                if past_constants {
                    let enforced = self.enforced_around_member(member, node);
                    let source = self
                        .blank_lines_before(member)
                        .min(blank.max_in_declarations);
                    self.blank_lines(enforced.max(source), Indent::ZERO);
                } else {
                    self.enum_break(trivial, policy);
                    // Constants have no `around-*` rule of their own, but the blank lines an
                    // author grouped them with are preserved — google-java-format's
                    // `BlankLineWanted.PRESERVE` between enum constants. Not above the *first*
                    // one: `visitEnumDeclaration` asks for `NO` right after the `{`.
                    let source = if first_constant {
                        0
                    } else {
                        self.blank_lines_before(member)
                            .min(blank.max_in_declarations)
                    };
                    first_constant = false;
                    if source > 0 {
                        self.ensure_blank_lines(source, Indent::ZERO);
                    }
                }
            }
            self.visit(member).await;
            // A constant is separated by the `,` that follows it, which sets this itself. A
            // member is not: the next one has to ask for its own separation.
            pending = past_constants;
        }
        if opened {
            self.close_indent(&indent);
        }
        self.close();
    }

    /// The break between two items of an enum body: negotiable while the enum is still shaped
    /// like an array initializer, forced once it has a member section.
    fn enum_break(&mut self, trivial: bool, policy: WrapPolicy) {
        if trivial {
            let flat = Self::flat_space(self.style.cfg.spacing.after_comma);
            self.list_break_flat(policy, flat, Indent::ZERO);
        } else {
            self.forced_break(Indent::ZERO);
        }
    }

    /// One enum constant: `NAME(args)` with an optional class body.
    pub(super) async fn visit_enum_constant(&mut self, node: &SyntaxNode) {
        // A constant carries its annotations directly rather than in a `MODIFIERS` node, so the
        // break after each one is placed here. `visitEnumConstantDeclaration` forces it whether
        // or not the annotation takes arguments, which is `[wrapping] field-annotations`.
        let policy = self.style.cfg.wrapping.field_annotations;
        let mut previous_annotation = false;
        for child in Self::children(node) {
            let is_annotation = child
                .as_node()
                .is_some_and(|child| matches!(child.kind(), S::ANNOTATION | S::ATTRIBUTE));
            if previous_annotation {
                self.annotation_break(policy);
            }
            previous_annotation = is_annotation;
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
    ///
    /// The modifiers are emitted *outside* the continuation level. A vertical annotation run
    /// breaks between the annotations and the declaration, and that break belongs to the
    /// declaration's own indent — inside the continuation level it would push `private int x;`
    /// four columns past the `@Deprecated` above it.
    pub(super) async fn visit_field(&mut self, node: &SyntaxNode) {
        // The declaration is a level of its own, *outside* the header — `declareOne`'s first
        // `open`. That is what a horizontal annotation run's break is measured against: a run
        // that shares the declaration's line while the declaration fits, and moves above it when
        // it does not. Emitted into the body's level instead, the break would sit among the
        // forced breaks that separate members and fire unconditionally.
        self.open_flat(Indent::ZERO);
        if let Some(modifiers) = Self::child_of(node, S::MODIFIERS) {
            self.visit(&modifiers).await;
        }
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        self.emit_declarators(node).await;
        self.close_indent(&continuation);
        self.close();
    }

    /// The `type name = init, name2 = init2` part shared by fields, locals, and resources.
    ///
    /// The break falls *after* `=` (google-java-format's assignment rule), so the initializer
    /// starts the continuation line; `before-assignment-operator` moves it to the front instead.
    ///
    /// The `MODIFIERS` child is **not** emitted here: a vertical annotation run breaks at the
    /// declaration's own indent, so the caller emits it before opening the continuation level.
    pub(super) async fn emit_declarators(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.assignment;
        let before = self.style.cfg.wrapping.before_assignment_operator;
        let children: Vec<SyntaxElement> = Self::children(node)
            .into_iter()
            .filter(|child| {
                child
                    .as_node()
                    .is_none_or(|node| node.kind() != S::MODIFIERS)
            })
            .collect();
        // `declareOne`'s `typeBreak`: when the type had to span lines, the name and the
        // initializer indent one step further than the type did, so the declaration still reads
        // as one construct rather than as two columns.
        let continuation = self.style.continuation();
        let type_tag = self.ops.new_tag();
        let typed = children
            .iter()
            .any(|child| child.as_node().is_some_and(|child| child.kind() == S::TYPE));
        let name_at = children.iter().position(|child| {
            child
                .as_token()
                .is_some_and(|tok| matches!(tok.kind(), S::IDENT | S::UNDERSCORE))
        });
        let conditional = || Indent::when_broken(type_tag, continuation.clone(), Indent::ZERO);
        let mut open_levels = 0usize;
        // The type and the name are one group, so a long initializer breaking the declaration
        // does not by itself put the name on a line of its own.
        if typed && name_at.is_some() {
            self.open_flat(Indent::ZERO);
            open_levels += 1;
        }
        for (nth, child) in children.iter().enumerate() {
            let kind = child.as_token().map(SyntaxToken::kind);
            if typed && Some(nth) == name_at {
                self.ops
                    .brk(FillMode::Independent, " ", Indent::ZERO, Some(type_tag));
                self.space_already_emitted();
                self.open(conditional());
                open_levels += 1;
                self.visit_element(child).await;
                // Close the name level and the type-and-name group: what follows — an
                // initializer, a dimension, the `;` — hangs off the tag instead.
                while open_levels > 0 {
                    self.close();
                    open_levels -= 1;
                }
                continue;
            }
            if kind == Some(S::EQ) {
                // A bare array initializer is *block-shaped*: it opens on this line and closes on
                // its own, so it has nowhere better to go and breaking before it would leave `=`
                // dangling above an opening brace.
                let block_shaped = children
                    .get(nth + 1)
                    .and_then(|next| next.as_node())
                    .is_some_and(|next| next.kind() == S::ARRAY_INIT);
                if !block_shaped {
                    self.open(conditional());
                    open_levels += 1;
                }
                if before && !block_shaped {
                    self.list_break(policy, Indent::ZERO);
                }
                self.visit_element(child).await;
                if !before && !block_shaped {
                    self.list_break(policy, Indent::ZERO);
                }
                continue;
            }
            if matches!(kind, Some(S::SEMICOLON | S::COMMA)) {
                while open_levels > 0 {
                    self.close();
                    open_levels -= 1;
                }
            }
            if nth > 0
                && matches!(
                    children[nth - 1].as_token().map(SyntaxToken::kind),
                    Some(S::COMMA)
                )
            {
                // One declarator per line once they stop fitting: `declareMany` separates them
                // with a UNIFIED break, so `int a = 1, b = 2, c = 3;` is all on one line or all
                // on lines of its own.
                self.list_break(WrapPolicy::IfLongPerItem, Indent::ZERO);
            }
            self.visit_element(child).await;
        }
        while open_levels > 0 {
            self.close();
            open_levels -= 1;
        }
    }

    /// A method or constructor declaration.
    ///
    /// The signature groups without indenting, for the same reason a type header does: the
    /// parameter list and the `throws` clause each carry the continuation indent on their own
    /// break.
    pub(super) async fn visit_method(&mut self, node: &SyntaxNode) {
        // The modifiers go outside the header level, for the reason `visit_type_decl` gives: a
        // forced break between a vertical annotation run and the declaration would otherwise make
        // the header unable to fit whatever it says.
        if let Some(modifiers) = Self::child_of(node, S::MODIFIERS) {
            self.visit(&modifiers).await;
        }
        // The header is one level at the continuation indent, which is `visitMethod`'s
        // `builder.open(plusFour)`: everything that can break inside a method header — the return
        // type, the name, the parameters, the `throws` clause — breaks one step in from the
        // declaration. The parameter list therefore adds nothing of its own (`visitFormals` opens
        // `ZERO`), or its parameters would land two steps in.
        let continuation = self.style.continuation();
        self.open(continuation.clone());

        // Two correlated decisions, exactly `visitMethod`'s two `BreakTag`s: whether the return
        // type moved onto a line of its own, and whether the name did. Each, when taken, indents
        // everything after the name — the parameters, the `throws` clause — one step further, so
        // a signature that wrapped at its type reads as one construct rather than as two columns.
        let type_tag = self.ops.new_tag();
        let name_tag = self.ops.new_tag();
        let children = Self::children(node);
        let name_at = children.iter().position(|child| {
            child
                .as_token()
                .is_some_and(|tok| matches!(tok.kind(), S::IDENT | S::UNDERSCORE))
        });
        let type_at = children
            .iter()
            .position(|child| child.as_node().is_some_and(|child| child.kind() == S::TYPE));

        self.open_flat(Indent::ZERO);
        let mut scoped = false;
        let mut header_open = true;
        let mut written = false;
        for (nth, child) in children.iter().enumerate() {
            if child
                .as_node()
                .is_some_and(|node| node.kind() == S::MODIFIERS)
            {
                continue;
            }
            if Some(nth) == type_at {
                if written {
                    self.tagged_break(type_tag);
                }
                self.open(Indent::when_broken(
                    type_tag,
                    continuation.clone(),
                    Indent::ZERO,
                ));
                scoped = true;
                self.visit_element(child).await;
                written = true;
                continue;
            }
            if Some(nth) == name_at {
                if written {
                    self.tagged_break(name_tag);
                }
                self.visit_element(child).await;
                // The name closes the type's scope and the group the header opened with; what
                // follows hangs off the two tags instead.
                if scoped {
                    self.close();
                    scoped = false;
                }
                self.close();
                self.open(Indent::when_broken(
                    name_tag,
                    continuation.clone(),
                    Indent::ZERO,
                ));
                self.open(Indent::when_broken(
                    type_tag,
                    continuation.clone(),
                    Indent::ZERO,
                ));
                self.open_flat(Indent::ZERO);
                written = true;
                continue;
            }
            let is_body = child
                .as_node()
                .is_some_and(|child| child.kind() == S::BLOCK);
            if is_body && header_open {
                header_open = false;
                if scoped {
                    self.close();
                    scoped = false;
                }
                self.close();
                if name_at.is_some() {
                    self.close();
                    self.close();
                }
                self.close_indent(&continuation);
                self.brace_before(self.style.cfg.braces.method_declaration);
            }
            if child
                .as_node()
                .is_some_and(|child| matches!(child.kind(), S::PARAM_LIST | S::RECORD_HEADER))
            {
                self.list_indent = Some(Indent::ZERO);
            }
            self.visit_element(child).await;
            written = true;
        }
        if header_open {
            if scoped {
                self.close();
            }
            self.close();
            if name_at.is_some() {
                self.close();
                self.close();
            }
            self.close_indent(&continuation);
        }
    }

    /// An `independent` break that records its decision, so a level after it can indent from it.
    fn tagged_break(&mut self, tag: crate::ir::BreakTag) {
        self.ops
            .brk(FillMode::Independent, " ", Indent::ZERO, Some(tag));
        self.space_already_emitted();
    }

    /// One parameter or record component.
    ///
    /// A parameter carrying a *declaration* annotation opens a level of its own —
    /// `declareOne`'s `kind == PARAMETER && hasDeclarationAnnotation ? plusFour : ZERO`. That is
    /// what indents the type under the annotations when a parameter's annotation run wraps,
    /// instead of leaving the type flush with them.
    pub(super) async fn visit_param(&mut self, node: &SyntaxNode) {
        let annotated = Self::child_of(node, S::MODIFIERS).is_some_and(|modifiers| {
            modifiers
                .children()
                .any(|child| matches!(child.kind(), S::ANNOTATION | S::ATTRIBUTE))
        });
        let continuation = self.style.continuation();
        if annotated {
            self.open(continuation.clone());
        }
        if node.kind() == S::RESOURCE {
            self.visit_field(node).await;
        } else {
            self.visit_children(node).await;
        }
        if annotated {
            self.close_indent(&continuation);
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
        // The value gets a level of its own so it moves down whole rather than breaking inside
        // itself on the `default` line — `methodDeclaration`'s `open(ZERO) { breakToFill(" ") … }`.
        // An array initializer is the exception: it is block-shaped and cancels the continuation
        // indent, so it stays where the declaration put it.
        let children = Self::children(node);
        let array = children.iter().any(|child| {
            child
                .as_node()
                .is_some_and(|value| value.kind() == S::ARRAY_INIT)
        });
        let continuation = self.style.continuation();
        let mut opened = false;
        for child in &children {
            if child
                .as_token()
                .is_some_and(|tok| tok.kind() == S::DEFAULT_KW)
            {
                self.space();
                self.visit_element(child).await;
                if array {
                    self.space();
                } else {
                    self.open(continuation.clone());
                    opened = true;
                    self.ops.brk(FillMode::Independent, " ", Indent::ZERO, None);
                    self.space_already_emitted();
                }
                continue;
            }
            self.visit_element(child).await;
        }
        if opened {
            self.close_indent(&continuation);
        }
    }

    /// The one-line rule a block should use, chosen by what owns it.
    pub(super) fn keep_for_block(&self, block: &SyntaxNode) -> KeepOnOneLine {
        let braces = &self.style.cfg.braces;
        // A block that has a sibling clause never collapses: `try {} catch (E e) {}` reads as one
        // statement with the braces gone, so google-java-format writes `try {\n} catch (E e) {\n}`
        // and only collapses a `try` with nothing after it (`CollapseEmptyOrNot.valueOf(
        // !trailingClauses)` in `visitTry`).
        if Self::has_sibling_clause(block) {
            return KeepOnOneLine::Never;
        }
        match block.parent().map(|parent| parent.kind()) {
            Some(S::METHOD_DECL | S::CONSTRUCTOR_DECL | S::INITIALIZER) => {
                braces.keep_method_body_on_one_line
            }
            Some(S::LAMBDA_EXPR) => braces.keep_lambda_body_on_one_line,
            _ => braces.keep_block_on_one_line,
        }
    }

    /// Whether `block` belongs to a `try` statement that has a `catch` or a `finally`.
    fn has_sibling_clause(block: &SyntaxNode) -> bool {
        let Some(parent) = block.parent() else {
            return false;
        };
        // A `catch`'s and a `finally`'s own block never collapse.
        if matches!(parent.kind(), S::CATCH_CLAUSE | S::FINALLY_CLAUSE) {
            return true;
        }
        if parent.kind() != S::TRY_STMT {
            return false;
        }
        parent
            .children()
            .any(|child| matches!(child.kind(), S::CATCH_CLAUSE | S::FINALLY_CLAUSE))
    }
}
