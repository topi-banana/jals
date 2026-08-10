//! Type declarations, modifiers, annotations, and the type-list clauses.
//!
//! A type declaration is a *header* followed by a *body*. The header is one level, so
//! `extends` / `implements` / `permits` break together at the highest syntactic level — Google
//! Java Style §4.5.1's "break at the highest level first" is exactly this level structure, not a
//! special case in the engine.

use alloc::vec::Vec;

use jals_config::fmt::{BraceStyle, InlineAnnotations, WrapPolicy};
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

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
        // The modifiers go *outside* the header level, as `typeDeclarationModifiers` puts them:
        // a vertical annotation run breaks between the annotations and the declaration, and a
        // forced break inside the header would make the header unable to fit whatever it says —
        // breaking `extends` and `implements` on a line that had room for them.
        if let Some(modifiers) = Self::child_of(node, S::MODIFIERS) {
            self.visit(&modifiers).await;
        }
        // Everything after the declared name is one level at the continuation indent, exactly
        // where `visitClassDeclaration` opens `plusFour`: the type parameters and the
        // `extends` / `implements` / `permits` clauses break from *there*, and each clause's own
        // type list breaks one step further in.
        let continuation = self.style.continuation();
        let children = Self::children(node);
        let name = children.iter().position(|child| {
            child
                .as_token()
                .is_some_and(|tok| matches!(tok.kind(), S::IDENT | S::UNDERSCORE))
        });
        let mut opened = false;
        for (nth, child) in children.iter().enumerate() {
            if child
                .as_node()
                .is_some_and(|node| node.kind() == S::MODIFIERS)
            {
                continue;
            }
            if child.as_node().is_some_and(|node| node.kind() == body_kind) {
                if opened {
                    self.close_indent(&continuation);
                }
                self.brace_before(self.style.cfg.braces.type_declaration);
                self.visit_element(child).await;
                return;
            }
            self.visit_element(child).await;
            if !opened && name == Some(nth) {
                self.open(continuation.clone());
                opened = true;
            }
        }
        if opened {
            self.close_indent(&continuation);
        }
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
        // A *trailing* run of well-known type annotations annotates the type that follows, not
        // the declaration, so it stays on the type's line however the declaration's own
        // annotations are placed — `splitModifiers`.
        // A declaration with no type of its own — a constructor — has nothing for a type
        // annotation to annotate, so `visitMethod` sends the whole run vertical instead.
        let typed = node
            .parent()
            .is_some_and(|owner| owner.children().any(|child| child.kind() == S::TYPE));
        let types_start = if typed {
            children
                .iter()
                .rposition(|child| {
                    !child
                        .as_node()
                        .is_some_and(|node| self.is_type_annotation(node))
                })
                .map_or(0, |at| at + 1)
        } else {
            children.len()
        };

        // The declaration's own annotations share a level — `visitModifiers`' `builder.open(ZERO)`
        // — so the run packs onto one line while it fits and moves *as a run* when it does not.
        // The break that separates the run from what it annotates lives outside that level.
        let mut leading_run = true;
        let mut previous_annotation = false;
        let mut run_open = false;
        for (nth, child) in children.iter().enumerate() {
            let is_annotation = child
                .as_node()
                .is_some_and(|node| matches!(node.kind(), S::ANNOTATION | S::ATTRIBUTE));
            if is_annotation && leading_run && !run_open && nth < types_start {
                self.open_flat(Indent::ZERO);
                run_open = true;
            }
            if previous_annotation && leading_run {
                // The break *before* the first type annotation still belongs to the declaration
                // run — it is `visitModifiers`' trailing break, not one between type annotations.
                if nth > types_start {
                    self.type_annotation_break();
                } else {
                    // The run's level holds only the annotations; the break that separates them
                    // from what they annotate is measured against the whole declaration.
                    if run_open && (!is_annotation || nth >= types_start) {
                        self.close();
                        run_open = false;
                    }
                    self.annotation_break(policy);
                }
            }
            self.visit_element(child).await;
            leading_run = leading_run && is_annotation;
            previous_annotation = is_annotation;
        }
        if run_open {
            self.close();
        }
        // A declaration whose modifiers are *only* annotations still needs separating from the
        // `class` / `void` that follows, and that keyword is not part of this node.
        if previous_annotation && leading_run {
            if types_start < children.len() {
                self.type_annotation_break();
            } else {
                self.annotation_break(policy);
            }
        }
    }

    /// The separation after a type annotation.
    ///
    /// `visitMethod` emits these *inside* the header level, where they only ever break with the
    /// return type itself. Emitted here — outside it, among the forced breaks that separate
    /// members — even a fill break would be taken every time, so the space is what reproduces
    /// google-java-format's placement.
    fn type_annotation_break(&mut self) {
        self.space();
    }

    /// The separation after a leading annotation, by its `[wrapping]` rule.
    pub(super) fn annotation_break(&mut self, policy: WrapPolicy) {
        match policy {
            WrapPolicy::AlwaysPerItem => self.forced_break(Indent::ZERO),
            WrapPolicy::IfLong | WrapPolicy::IfLongPerItem => self.break_op(Indent::ZERO),
            WrapPolicy::Never => self.space(),
        }
    }

    /// Which `[wrapping]` rule governs this `MODIFIERS` node's leading annotations.
    fn annotation_policy(&self, node: &SyntaxNode) -> WrapPolicy {
        let wrapping = &self.style.cfg.wrapping;
        let parent = node.parent().map(|parent| parent.kind());
        // `inline-argumentless-annotations` decides from the annotations themselves, so it is
        // answered before the per-kind rule it overrides.
        let inlined = match wrapping.inline_argumentless_annotations {
            InlineAnnotations::Never => false,
            InlineAnnotations::Locals => matches!(
                parent,
                Some(S::LOCAL_VAR_DECL | S::RESOURCE | S::PARAM | S::RECORD_COMPONENT)
            ),
            InlineAnnotations::Declarations => matches!(
                parent,
                Some(
                    S::FIELD_DECL
                        | S::LOCAL_VAR_DECL
                        | S::RESOURCE
                        | S::PARAM
                        | S::RECORD_COMPONENT
                )
            ),
        };
        if inlined && !Self::any_annotation_has_arguments(node) {
            // Horizontal is not *pinned* horizontal for a declaration: `fieldAnnotationDirection`'s
            // `breakList` is a UNIFIED break, so an argumentless run shares the declaration's line
            // while it fits and moves above it when it does not. A parameter has no line of its
            // own to move off, so it stays put.
            return WrapPolicy::IfLongPerItem;
        }
        match parent {
            Some(
                S::CLASS_DECL
                | S::INTERFACE_DECL
                | S::ENUM_DECL
                | S::RECORD_DECL
                | S::ANNOTATION_TYPE_DECL
                | S::MODULE_DECL
                // `visitPackage` forces a break after each package annotation.
                | S::PACKAGE_DECL,
            ) => wrapping.type_annotations,
            Some(S::METHOD_DECL | S::CONSTRUCTOR_DECL) => wrapping.method_annotations,
            // `visitEnumConstantDeclaration` forces a break after every constant annotation,
            // whether or not it takes arguments — the constant is a field of the enum.
            Some(S::FIELD_DECL | S::ENUM_CONSTANT) => wrapping.field_annotations,
            Some(S::PARAM | S::RECORD_COMPONENT | S::LAMBDA_PARAMS) => {
                wrapping.parameter_annotations
            }
            Some(S::LOCAL_VAR_DECL | S::RESOURCE) => wrapping.variable_annotations,
            // A type-use annotation is inline by definition — it sits in the middle of a type.
            _ => WrapPolicy::Never,
        }
    }

    /// Whether any annotation in this `MODIFIERS` node carries an argument list.
    ///
    /// The whole run answers together, as google-java-format's `fieldAnnotationDirection` does:
    /// one `@SuppressWarnings("x")` puts every annotation on the run onto its own line.
    fn any_annotation_has_arguments(node: &SyntaxNode) -> bool {
        node.children()
            .filter(|child| matches!(child.kind(), S::ANNOTATION | S::ATTRIBUTE))
            .any(|annotation| {
                annotation
                    .children()
                    .any(|child| matches!(child.kind(), S::ANNOTATION_ARG_LIST | S::ATTR_ARG_LIST))
            })
    }

    /// One member-value pair of an annotation: `name = value`.
    ///
    /// `visitAnnotationArgument` breaks after the `=` all-or-nothing, and not at all when the
    /// value is an array initializer — a `{` has nowhere better to go than the `=`'s line.
    pub(super) async fn visit_annotation_pair(&mut self, node: &SyntaxNode) {
        let array = node.children().any(|child| child.kind() == S::ARRAY_INIT);
        let continuation = self.style.continuation();
        let indent = if array {
            Indent::ZERO
        } else {
            continuation.clone()
        };
        self.open(indent.clone());
        for child in Self::children(node) {
            self.visit_element(&child).await;
            if child.as_token().is_some_and(|tok| tok.kind() == S::EQ) && !array {
                self.list_break(WrapPolicy::IfLongPerItem, Indent::ZERO);
            }
        }
        self.close_indent(&indent);
    }

    /// One annotation use: `@Name` with an optional argument list.
    pub(super) async fn visit_annotation(&mut self, node: &SyntaxNode) {
        self.visit_children(node).await;
    }

    /// `extends` / `implements` / `permits` / `throws`.
    ///
    /// The keyword starts the continuation line when the clause wraps, which is what puts the
    /// break at the highest level of the declaration rather than inside the type list. The break
    /// *before* the keyword is a fill, so `class A extends S` keeps its superclass on the header's
    /// line while a long `implements` moves down on its own — `classDeclarationTypeList`.
    ///
    /// A clause with more than one type opens a level of its own, so its list breaks one step
    /// further in than the keyword: the reader sees the clause, then its members.
    pub(super) async fn visit_type_clause(&mut self, node: &SyntaxNode) {
        let policy = match node.kind() {
            S::THROWS_CLAUSE => self.style.cfg.wrapping.throws_list,
            _ => self.style.cfg.wrapping.extends_list,
        };
        // Both a type declaration and a method open a header level, so a clause breaks from
        // there and its own type list breaks one step further in.
        let continuation = self.style.continuation();
        self.clause_break(policy, Indent::ZERO);
        let types = node.children().count();
        let indent = if types > 1 {
            continuation.clone()
        } else {
            Indent::ZERO
        };
        self.open(indent.clone());
        let children = Self::children(node);
        for (nth, child) in children.iter().enumerate() {
            // A `throws` keyword is followed by a fill break rather than a space: its list is a
            // continuation of the method header, not a clause of its own. `visitThrowsClause`.
            if nth == 0
                && node.kind() == S::THROWS_CLAUSE
                && child
                    .as_token()
                    .is_some_and(|tok| tok.kind() == S::THROWS_KW)
            {
                self.visit_element(child).await;
                self.clause_break(policy, Indent::ZERO);
                continue;
            }
            if nth > 0
                && matches!(
                    children[nth - 1].as_token().map(SyntaxToken::kind),
                    Some(S::COMMA)
                )
            {
                self.list_break(policy, Indent::ZERO);
            }
            self.visit_element(child).await;
        }
        self.close_indent(&indent);
    }

    /// The break standing before a clause keyword — a fill, so one clause may wrap while the
    /// next stays put.
    fn clause_break(&mut self, policy: WrapPolicy, plus_indent: Indent) {
        match policy {
            WrapPolicy::Never => self.space(),
            WrapPolicy::AlwaysPerItem => self.forced_break(plus_indent),
            WrapPolicy::IfLong | WrapPolicy::IfLongPerItem => {
                self.ops.brk(FillMode::Independent, " ", plus_indent, None);
                self.space_already_emitted();
            }
        }
    }

    /// A type-argument or type-parameter list — `<K, V>`, `<T extends A & B>`.
    pub(super) async fn visit_type_list(&mut self, node: &SyntaxNode) {
        let policy = if node.kind() == S::TYPE_PARAMS {
            self.style.cfg.wrapping.type_parameters
        } else {
            self.style.cfg.wrapping.type_arguments
        };
        // A parameterized *type* and a type-parameter list may break right after their `<`,
        // which lets a list too wide for its line move down whole instead of splitting between
        // two of its members — `visitParameterizedType` and `typeParametersRest`. The explicit
        // type arguments of a call are written against the name they qualify and get no such
        // break (`addTypeArguments`).
        let breaks_after_open = node.kind() == S::TYPE_PARAMS
            || (node.kind() == S::TYPE_ARGS
                && node
                    .parent()
                    .is_some_and(|parent| matches!(parent.kind(), S::TYPE | S::TYPE_ARGS)));
        // `typeParametersRest` indents a type-declaration's parameters only when the declaration
        // also has an `extends` / `implements` / `permits` clause to line them up against; a
        // method's always indent.
        let owner = node.parent();
        // A call's explicit type arguments indent nothing of their own: `addTypeArguments` is
        // called with `ZERO`, because the chain already opened the level they break into.
        let qualifier = node.kind() == S::TYPE_ARGS
            && owner.as_ref().is_some_and(|owner| {
                matches!(
                    owner.kind(),
                    S::CALL_EXPR | S::METHOD_REF_EXPR | S::FIELD_ACCESS
                )
            });
        let indented = !qualifier
            && (node.kind() != S::TYPE_PARAMS
                || owner.is_some_and(|owner| {
                    matches!(owner.kind(), S::METHOD_DECL | S::CONSTRUCTOR_DECL)
                        || owner.children().any(|child| {
                            matches!(
                                child.kind(),
                                S::EXTENDS_CLAUSE | S::IMPLEMENTS_CLAUSE | S::PERMITS_CLAUSE
                            )
                        })
                }));
        let continuation = if indented {
            self.style.continuation()
        } else {
            Indent::ZERO
        };
        self.open_flat(Indent::ZERO);
        let children = Self::children(node);
        for (nth, child) in children.iter().enumerate() {
            let is_open = matches!(child.as_token().map(SyntaxToken::kind), Some(S::LT));
            let is_close = matches!(child.as_token().map(SyntaxToken::kind), Some(S::GT));
            if is_open {
                if breaks_after_open {
                    self.open(continuation.clone());
                    self.visit_element(child).await;
                    self.list_break_tight(policy, Indent::ZERO);
                    self.open_flat(Indent::ZERO);
                } else {
                    self.visit_element(child).await;
                    self.open(continuation.clone());
                }
                continue;
            }
            if is_close {
                if breaks_after_open {
                    self.close();
                    self.visit_element(child).await;
                    self.close_indent(&continuation);
                } else {
                    self.close_indent(&continuation);
                    self.visit_element(child).await;
                }
                continue;
            }
            if nth > 0
                && matches!(
                    children[nth - 1].as_token().map(SyntaxToken::kind),
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
                    children[nth - 1].as_token().map(SyntaxToken::kind),
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
