//! Type declarations, modifiers, annotations, and the type-list clauses.
//!
//! A type declaration is a *header* followed by a *body*. The header is one level, so
//! `extends` / `implements` / `permits` break together at the highest syntactic level — Google
//! Java Style §4.5.1's "break at the highest level first" is exactly this level structure, not a
//! special case in the engine.

use alloc::vec::Vec;

use jals_config::fmt::{BraceStyle, WrapPolicy};
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode};

use crate::ir::{FillMode, Indent};
use crate::passes::ModifierOrder;
use crate::visit::Ctx;

impl Ctx<'_> {
    /// `class` / `interface` / `enum` / `record` / `@interface`.
    pub(super) async fn visit_type_decl(&mut self, node: &SyntaxNode) {
        let body_kind = if node.kind() == S::ENUM_DECL {
            S::ENUM_BODY
        } else {
            S::CLASS_BODY
        };
        // The header groups without indenting: the pieces that can actually break — the
        // `extends` / `implements` clauses and the type-parameter list — carry the continuation
        // indent on their own break, so adding it here too would double it.
        self.open_flat(Indent::ZERO);
        for child in Self::children(node) {
            if child.as_node().is_some_and(|node| node.kind() == body_kind) {
                self.close();
                self.brace_before(self.style.cfg.braces.type_declaration);
                self.visit_element(&child).await;
                return;
            }
            self.visit_element(&child).await;
        }
        self.close();
    }

    /// Emit whatever separates a header from the `{` that follows it.
    ///
    /// `same-line` leaves the brace where the spacing rule puts it. The three Allman-family
    /// styles put it on its own line, and `next-line-shifted` indents it (and, for Whitesmiths,
    /// the body with it); `next-line-on-wrap` only moves it when the header itself broke, which
    /// is a correlated decision the engine already knows how to express.
    pub(super) fn brace_before(&mut self, style: BraceStyle) {
        match style {
            BraceStyle::SameLine => {}
            BraceStyle::NextLine => self.forced_break(Indent::ZERO),
            BraceStyle::NextLineShifted | BraceStyle::NextLineShiftedBraces => {
                let indent = self.style.indent();
                self.forced_break(indent);
            }
            BraceStyle::NextLineOnWrap => {
                // The header is the level that just closed; a break here renders as a space when
                // that level stayed flat and as a newline when it did not.
                self.break_op(Indent::ZERO);
            }
        }
    }

    /// A declaration's modifiers, reordered when `[imports] reorder-modifiers` is on.
    ///
    /// Annotation placement is a wrapping rule: `always-per-item` puts each leading annotation on
    /// its own line (the idiomatic Java convention and google-java-format's behavior for type,
    /// method, and field declarations), while `never` keeps them inline, which is what a
    /// parameter or a local variable wants.
    pub(super) async fn visit_modifiers(&mut self, node: &SyntaxNode) {
        let ordered = ModifierOrder::plan(node, self.style.cfg.imports.reorder_modifiers);
        let children = ordered.unwrap_or_else(|| Self::children(node));
        let policy = self.annotation_policy(node);

        // Only the *leading* run of annotations gets its own lines. An annotation written after a
        // keyword modifier (`non-sealed @A class B`) stays where the author put it — which is
        // what google-java-format does, and the reason this tracks a run rather than a predicate
        // on each child.
        let mut leading_run = true;
        let mut previous_annotation = false;
        for child in &children {
            let is_annotation = child
                .as_node()
                .is_some_and(|node| matches!(node.kind(), S::ANNOTATION | S::ATTRIBUTE));
            if previous_annotation && leading_run {
                self.annotation_break(policy);
            }
            self.visit_element(child).await;
            leading_run = leading_run && is_annotation;
            previous_annotation = is_annotation;
        }
        // A declaration whose modifiers are *only* annotations still needs separating from the
        // `class` / `void` that follows, and that keyword is not part of this node.
        if previous_annotation && leading_run {
            self.annotation_break(policy);
        }
    }

    /// The separation after a leading annotation, by its `[wrapping]` rule.
    fn annotation_break(&mut self, policy: WrapPolicy) {
        match policy {
            WrapPolicy::AlwaysPerItem => self.forced_break(Indent::ZERO),
            WrapPolicy::IfLong | WrapPolicy::IfLongPerItem => self.break_op(Indent::ZERO),
            WrapPolicy::Never => self.space(),
        }
    }

    /// Which `[wrapping]` rule governs this `MODIFIERS` node's leading annotations.
    fn annotation_policy(&self, node: &SyntaxNode) -> WrapPolicy {
        let wrapping = &self.style.cfg.wrapping;
        match node.parent().map(|parent| parent.kind()) {
            Some(
                S::CLASS_DECL
                | S::INTERFACE_DECL
                | S::ENUM_DECL
                | S::RECORD_DECL
                | S::ANNOTATION_TYPE_DECL,
            ) => wrapping.type_annotations,
            Some(S::METHOD_DECL | S::CONSTRUCTOR_DECL) => wrapping.method_annotations,
            Some(S::FIELD_DECL) => wrapping.field_annotations,
            Some(S::PARAM | S::RECORD_COMPONENT | S::LAMBDA_PARAMS) => {
                wrapping.parameter_annotations
            }
            Some(S::LOCAL_VAR_DECL | S::RESOURCE) => wrapping.variable_annotations,
            // A type-use annotation is inline by definition — it sits in the middle of a type.
            _ => WrapPolicy::Never,
        }
    }

    /// One annotation use: `@Name` with an optional argument list.
    pub(super) async fn visit_annotation(&mut self, node: &SyntaxNode) {
        self.visit_children(node).await;
    }

    /// `extends` / `implements` / `permits` / `throws`.
    ///
    /// The keyword starts the continuation line when the clause wraps, which is what puts the
    /// break at the highest level of the declaration rather than inside the type list.
    pub(super) async fn visit_type_clause(&mut self, node: &SyntaxNode) {
        let policy = match node.kind() {
            S::THROWS_CLAUSE => self.style.cfg.wrapping.throws_list,
            _ => self.style.cfg.wrapping.extends_list,
        };
        let continuation = self.style.continuation();
        self.break_op(continuation.clone());
        self.open(continuation.clone());
        self.emit_comma_list(node, policy, Indent::ZERO).await;
        self.close_indent(&continuation);
    }

    /// A type-argument or type-parameter list — `<K, V>`, `<T extends A & B>`.
    pub(super) async fn visit_type_list(&mut self, node: &SyntaxNode) {
        let policy = if node.kind() == S::TYPE_PARAMS {
            self.style.cfg.wrapping.type_parameters
        } else {
            self.style.cfg.wrapping.type_arguments
        };
        let continuation = self.style.continuation();
        self.open_flat(Indent::ZERO);
        let children = Self::children(node);
        for (nth, child) in children.iter().enumerate() {
            let is_open = matches!(child.as_token().map(|tok| tok.kind()), Some(S::LT));
            let is_close = matches!(child.as_token().map(|tok| tok.kind()), Some(S::GT));
            if is_open {
                self.visit_element(child).await;
                self.open(continuation.clone());
                continue;
            }
            if is_close {
                self.close_indent(&continuation);
                self.visit_element(child).await;
                continue;
            }
            if nth > 0
                && matches!(
                    children[nth - 1].as_token().map(|tok| tok.kind()),
                    Some(S::COMMA)
                )
            {
                self.list_break(policy, Indent::ZERO);
            }
            self.visit_element(child).await;
        }
        self.close();
    }

    /// Emit a comma-separated run of children under `policy`, breaking between items.
    pub(super) async fn emit_comma_list(
        &mut self,
        node: &SyntaxNode,
        policy: WrapPolicy,
        plus_indent: Indent,
    ) {
        let children: Vec<SyntaxElement> = Self::children(node);
        for (nth, child) in children.iter().enumerate() {
            let after_comma = nth > 0
                && matches!(
                    children[nth - 1].as_token().map(|tok| tok.kind()),
                    Some(S::COMMA)
                );
            if after_comma {
                self.list_break(policy, plus_indent.clone());
            }
            self.visit_element(child).await;
        }
    }

    /// The break between two items of a list, by policy.
    ///
    /// This is the one place `WrapPolicy`'s four values turn into engine vocabulary:
    /// `if-long` is a fill, `if-long-per-item` is all-or-nothing, `always-per-item` is forced,
    /// and `never` is a plain space.
    pub(super) fn list_break(&mut self, policy: WrapPolicy, plus_indent: Indent) {
        self.list_break_flat(policy, " ", plus_indent);
    }

    /// The same, for a break whose flat form is **empty** rather than a space.
    ///
    /// A method chain's `.` and a delimiter's inner edge sit against their neighbour when the
    /// construct stays on one line; using [`list_break`](Self::list_break) there would leave
    /// `a .b()` behind.
    pub(super) fn list_break_tight(&mut self, policy: WrapPolicy, plus_indent: Indent) {
        self.list_break_flat(policy, "", plus_indent);
    }

    /// A list break whose **flat** rendering is `flat`.
    ///
    /// A break stands where a space would otherwise be decided, so the two cannot both apply: if
    /// the break emitted a space of its own and [`Spacing`](super::Spacing) emitted another, every
    /// list would be double-spaced. The break therefore *carries* the spacing decision — its flat
    /// text is what the `[spacing]` rule for that position asks for — which is why a rule like
    /// `after-comma` reaches the output at all.
    pub(super) fn list_break_flat(&mut self, policy: WrapPolicy, flat: &str, plus_indent: Indent) {
        let fill = match policy {
            WrapPolicy::Never => {
                if !flat.is_empty() {
                    self.space();
                }
                return;
            }
            WrapPolicy::IfLong => FillMode::Independent,
            WrapPolicy::IfLongPerItem => FillMode::Unified,
            WrapPolicy::AlwaysPerItem => FillMode::Forced,
        };
        self.ops.brk(fill, flat, plus_indent, None);
        self.space_already_emitted();
    }

    /// The flat rendering a `[spacing]` decision asks for.
    pub(super) const fn flat_space(space: bool) -> &'static str {
        if space { " " } else { "" }
    }
}
