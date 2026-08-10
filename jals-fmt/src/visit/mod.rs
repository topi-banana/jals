//! L2 — lowering the CST into a layout document.
//!
//! This is where the ~50 syntax rules live, and it is the only layer that is genuinely
//! *parallel*: each node contributes its own part of the document independently, so a rule can be
//! written, read, and changed without knowing what the others do. (Resolution — deciding which
//! breaks are taken — is the opposite and lives in [`engine`](crate::engine).)
//!
//! # The contract every rule keeps
//!
//! Emit all of your node's direct significant tokens; recurse into all of your child nodes. A
//! rule adds levels and breaks *around* that, never instead of it. Because each token in a
//! `rowan` tree has exactly one parent, the significant-token multiset is then preserved
//! structurally: a node with no bespoke rule falls through to [`Ctx::visit_children`] and still
//! emits everything, and an `ERROR` node emits its tokens too.
//!
//! Inter-token spacing is not each rule's business — [`Spacing`] decides it centrally from the
//! token pair and their parents, so a `[spacing]` rule cannot be honored in one construct and
//! forgotten in another.
//!
//! # What this module does *not* do
//!
//! It does not decide what runs. [`Ctx::new`] → [`Ctx::visit`] → [`Ctx::finish`] is one stage of a
//! pipeline that [`Formatter`](crate::passes::Formatter) drives; the L0 plans this lowering reads
//! and the L4 passes that follow it are sequenced there, so a rule here never has to know the
//! order.

mod chain;
mod decl;
mod delimited;
mod dialect;
mod expr;
mod member;
mod spacing;
mod stmt;
mod ty;
mod unit;

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use jals_exec::{LocalBoxFuture, Yielder};
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};
use text_size::TextRange;

use crate::comments::{Comment, CommentMap};
use crate::ir::Indent;
use crate::javadoc::CommentFormatter;
use crate::ops::Ops;
use crate::passes::{LiteralRewrite, OffOn};
use crate::style::Style;

pub(crate) use spacing::Spacing;

/// The emission context threaded through the whole lowering walk.
pub(crate) struct Ctx<'a> {
    /// The resolved style — the only thing a rule reads to change what it emits.
    style: &'a Style,
    /// The original source, for verbatim regions.
    src: &'a str,
    /// Where each comment is anchored.
    comments: CommentMap,
    /// The document under construction.
    ops: Ops,
    /// The used-name set, when unused-import removal is on.
    used: Option<BTreeSet<String>>,
    /// Formatter-disabled regions, in source order.
    disabled: Vec<TextRange>,
    /// Whether each disabled region has been emitted yet.
    emitted: Vec<bool>,
    /// The structural indent in columns, used as the comment reflow budget's base.
    indent: usize,
    /// The last significant token emitted, for the spacing decision.
    previous: Option<SyntaxToken>,
    /// Whitespace has already been emitted, so no space is owed.
    spaced: bool,
    /// The branch just emitted got its braces from `[braces] force-*`, so the continuation
    /// keyword after it cuddles a `}` the source never had.
    braced_branch: bool,
    /// A significant token has been emitted, so the file's header region is over.
    ///
    /// The header is not "the first comment": it is every comment before the first declaration —
    /// JDT's `comment.format_header` covers the whole run, and an OpenJDK file routinely opens
    /// with a licence block, a blank line and a second attribution block.
    header_seen: bool,
    /// Token offsets whose own-line leading comments were already hoisted by an enclosing node.
    hoisted: BTreeSet<usize>,
    /// Separation an item asked for that has no gap above it yet, to be applied *after* its own
    /// leading comments. This is what `[blank-lines] before-package` names: nothing precedes the
    /// first item of a file but its header comment, so the blank lines it wants fall between the
    /// two.
    owed_after_comments: usize,
    /// Simple names imported from a well-known type-annotation package. A trailing run of these
    /// on a declaration annotates the *type*, not the declaration, so it stays on the type's line.
    type_annotations: BTreeSet<String>,
    /// The indent the next delimited list opens at, when the construct that owns it decides
    /// rather than the list itself — `addArguments`' `plusIndent` parameter, and a declaration
    /// header whose own level already took the step.
    list_indent: Option<Indent>,
    /// How many comments were emitted, checked against the map in debug builds.
    emitted_comments: usize,
    /// Amortized cooperative yielding across the walk.
    yielder: Yielder,
}

impl<'a> Ctx<'a> {
    /// A context for one format run.
    pub(crate) async fn new(
        root: &SyntaxNode,
        src: &'a str,
        style: &'a Style,
        used: Option<BTreeSet<String>>,
        disabled: Vec<TextRange>,
    ) -> Self {
        let comments = CommentMap::build(
            root,
            style.cfg.comments.normalize_parameter_comments,
            style.cfg.comments.inline_block_comments,
            &disabled,
        )
        .await;
        Self {
            style,
            src,
            comments,
            ops: Ops::new(),
            used,
            emitted: alloc::vec![false; disabled.len()],
            disabled,
            indent: 0,
            previous: None,
            spaced: true,
            braced_branch: false,
            header_seen: false,
            hoisted: BTreeSet::new(),
            owed_after_comments: 0,
            list_indent: None,
            type_annotations: Self::imported_type_annotations(root),
            emitted_comments: 0,
            yielder: Yielder::new(),
        }
    }

    /// Finish, returning the document and its break-tag count.
    pub(crate) fn finish(mut self) -> (crate::ir::Doc, usize) {
        // Emit any disabled region that covered no significant token, so `@formatter:off` around
        // a comment-only span still reaches the output.
        for (at, region) in self.disabled.clone().iter().enumerate() {
            if !self.emitted[at] {
                self.emitted[at] = true;
                self.ops.forced_break(Indent::ZERO);
                let text = &self.src[usize::from(region.start())..usize::from(region.end())];
                self.ops.verbatim(text);
            }
        }
        for comment in self.comments.orphans().to_vec() {
            self.emit_comment_line(&comment);
        }
        debug_assert_eq!(
            self.emitted_comments,
            self.comments.anchored(),
            "every comment must be emitted exactly once",
        );
        self.ops.finish()
    }

    // ===== Dispatch =====

    /// Lower a node.
    ///
    /// The one boxed shim of the lowering recursion: every rule recurses back through here, so
    /// the async cycle has a single choke point rather than a box per call.
    pub(crate) fn visit<'n>(&'n mut self, node: &'n SyntaxNode) -> LocalBoxFuture<'n, ()> {
        Box::pin(self.visit_impl(node))
    }

    /// The per-kind dispatch behind [`Ctx::visit`].
    #[allow(
        clippy::match_same_arms,
        reason = "a named arm documents that the kind was considered, even when it falls back"
    )]
    async fn visit_impl(&mut self, node: &SyntaxNode) {
        self.yielder.tick().await;
        self.hoist_leading_comments(node);
        match node.kind() {
            // --- compilation unit ---
            S::SOURCE_FILE => self.visit_source_file(node).await,
            S::MODULE_DECL => self.visit_module_decl(node).await,
            S::MODULE_BODY => self.visit_module_body(node).await,
            S::REQUIRES_DIRECTIVE
            | S::EXPORTS_DIRECTIVE
            | S::OPENS_DIRECTIVE
            | S::USES_DIRECTIVE
            | S::PROVIDES_DIRECTIVE => self.visit_directive(node).await,

            // --- declarations ---
            S::CLASS_DECL
            | S::INTERFACE_DECL
            | S::ENUM_DECL
            | S::RECORD_DECL
            | S::ANNOTATION_TYPE_DECL => self.visit_type_decl(node).await,
            S::MODIFIERS => self.visit_modifiers(node).await,
            S::ANNOTATION => self.visit_annotation(node).await,
            S::EXTENDS_CLAUSE | S::IMPLEMENTS_CLAUSE | S::PERMITS_CLAUSE | S::THROWS_CLAUSE => {
                self.visit_type_clause(node).await;
            }
            S::TYPE_PARAMS | S::TYPE_ARGS => self.visit_type_list(node).await,
            // `TYPE_PARAM` has no bespoke rule of its own — its `&` bound is a spacing decision —
            // but naming it here documents that the omission is deliberate.
            S::TYPE_PARAM => self.visit_type_param(node).await,
            S::TYPE => self.visit_type(node).await,
            S::PARAM | S::RECORD_COMPONENT => self.visit_param(node).await,
            S::ANNOTATION_PAIR => self.visit_annotation_pair(node).await,

            // --- members ---
            S::CLASS_BODY => self.visit_class_body(node).await,
            S::ENUM_BODY => self.visit_enum_body(node).await,
            S::FIELD_DECL => self.visit_field(node).await,
            S::METHOD_DECL | S::CONSTRUCTOR_DECL => self.visit_method(node).await,
            S::INITIALIZER => self.visit_initializer(node).await,
            S::ENUM_CONSTANT => self.visit_enum_constant(node).await,
            S::ANNOTATION_DEFAULT => self.visit_annotation_default(node).await,

            // --- delimited lists ---
            S::PARAM_LIST
            | S::ARG_LIST
            | S::RECORD_HEADER
            | S::ANNOTATION_ARG_LIST
            | S::ATTR_ARG_LIST
            | S::LAMBDA_PARAMS => self.visit_delimited(node).await,
            S::RESOURCE_LIST => self.visit_resource_list(node).await,
            // A resource is declared exactly as a parameter is — `visitTry` calls `declareOne`
            // with `DeclarationKind.PARAMETER` — so its initializer moves down whole rather than
            // breaking inside, and an annotation run on it indents what follows.
            S::RESOURCE => self.visit_param(node).await,
            S::ARRAY_INIT => self.visit_array_init(node).await,
            S::RECORD_PATTERN => self.visit_record_pattern(node).await,

            // --- statements ---
            S::BLOCK => self.visit_block(node).await,
            S::LOCAL_VAR_DECL => self.visit_local_var(node).await,
            S::EXPR_STMT | S::RETURN_STMT | S::THROW_STMT | S::YIELD_STMT => {
                self.visit_simple_stmt(node).await;
            }
            S::IF_STMT => self.visit_if(node).await,
            S::WHILE_STMT => self.visit_while(node).await,
            S::DO_WHILE_STMT => self.visit_do_while(node).await,
            S::FOR_STMT => self.visit_for(node).await,
            S::FOR_EACH_STMT => self.visit_for_each(node).await,
            S::TRY_STMT => self.visit_try(node).await,
            S::CATCH_CLAUSE => self.visit_catch(node).await,
            S::FINALLY_CLAUSE => self.visit_finally(node).await,
            S::SYNCHRONIZED_STMT => self.visit_synchronized(node).await,
            S::SWITCH_STMT | S::SWITCH_EXPR => self.visit_switch(node).await,
            S::SWITCH_BLOCK => self.visit_switch_block(node).await,
            S::SWITCH_RULE => self.visit_switch_rule(node).await,
            S::SWITCH_GROUP => self.visit_switch_group(node).await,
            S::SWITCH_LABEL => self.visit_switch_label(node).await,
            S::GUARD => self.visit_guard(node).await,
            S::LABELED_STMT => self.visit_labeled(node).await,
            S::ASSERT_STMT => self.visit_assert(node).await,

            // --- expressions ---
            S::BINARY_EXPR => self.visit_binary(node).await,
            S::ASSIGNMENT_EXPR => self.visit_assignment(node).await,
            S::TERNARY_EXPR => self.visit_ternary(node).await,
            S::LAMBDA_EXPR => self.visit_lambda(node).await,
            S::CAST_EXPR => self.visit_cast(node).await,
            S::CALL_EXPR | S::FIELD_ACCESS | S::METHOD_REF_EXPR | S::INDEX_EXPR => {
                self.visit_chain(node).await;
            }
            S::NEW_EXPR => self.visit_new(node).await,

            // --- jals dialect ---
            S::IMPORT_GROUP => self.visit_import_group(node).await,
            S::ATTRIBUTE => self.visit_attribute(node).await,

            // --- everything else ---
            _ => self.visit_children(node).await,
        }
    }

    /// The generic path: emit every direct token, recurse into every child node.
    async fn visit_children(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            self.visit_element(&child).await;
        }
    }

    /// Lower one child, whichever kind it is.
    async fn visit_element(&mut self, child: &SyntaxElement) {
        match child {
            SyntaxElement::Node(node) => self.visit(node).await,
            // Trivia is not emitted here: a comment rides with the token it is anchored to.
            SyntaxElement::Token(tok) if !tok.kind().is_trivia() => self.token(tok),
            SyntaxElement::Token(_) => {}
        }
    }

    // ===== Token emission =====

    /// Emit one significant token, with its comments and its spacing.
    fn token(&mut self, tok: &SyntaxToken) {
        if self.in_disabled_region(tok) {
            return;
        }
        self.emit_leading(tok);
        if !self.spaced
            && let Some(previous) = &self.previous
            && Spacing::between(previous, tok, self.style)
        {
            self.ops.space();
        }
        // A text block whose closing `\"\"\"` sits at column zero of its own content is written
        // *at* column zero: its incidental whitespace is already none, and indenting the opener
        // would change what the string says. `visitLiteral` spells this as a break with an
        // effectively infinite dedent.
        if tok.kind() == S::TEXT_BLOCK && Self::text_block_is_deindented(tok.text()) {
            self.ops.brk(
                crate::ir::FillMode::Unified,
                "",
                Indent::columns(-1024),
                None,
            );
            self.spaced = true;
        }
        let text = LiteralRewrite::apply(tok.text(), tok.kind(), self.style.cfg.literals);
        self.ops.token(&text);
        self.header_seen = true;
        self.spaced = false;
        self.previous = Some(tok.clone());
        self.emit_trailing(tok);
    }

    /// Whether a text block's closing delimiter sits at column zero of its own content.
    fn text_block_is_deindented(text: &str) -> bool {
        text.rsplit('\n')
            .next()
            .is_some_and(|last| last.starts_with("\"\"\""))
            && text.contains('\n')
    }

    /// Emit a token whose text the rule chose — a brace it is inserting, say.
    fn synthetic(&mut self, text: &str) {
        self.ops.token(text);
        self.spaced = false;
        self.previous = None;
    }

    /// A single space, suppressing the automatic decision for the next token.
    fn space(&mut self) {
        self.ops.space();
        self.spaced = true;
    }

    /// A space, or nothing, depending on a `[spacing]` rule.
    fn space_if(&mut self, yes: bool) {
        if yes {
            self.space();
        }
    }

    /// Record that whitespace has already been emitted, so the next token owes no space.
    ///
    /// Needed by the few rules that reach [`Ops::brk`](crate::ops::Ops::brk) directly to build a
    /// tagged or conditionally-indented break, which the [`break_op`](Self::break_op) family
    /// cannot express.
    const fn space_already_emitted(&mut self) {
        self.spaced = true;
    }

    /// A break that renders as a space when it stays on the line.
    fn break_op(&mut self, plus_indent: Indent) {
        self.ops.break_op(plus_indent);
        self.spaced = true;
    }

    /// A break that always goes.
    fn forced_break(&mut self, plus_indent: Indent) {
        self.ops.forced_break(plus_indent);
        self.spaced = true;
    }

    /// A forced break followed by `count` empty lines.
    fn blank_lines(&mut self, count: usize, plus_indent: Indent) {
        self.ops.blank_lines(count, plus_indent);
        self.spaced = true;
    }

    /// Raise the separation before the next item to at least `count` blank lines.
    fn ensure_blank_lines(&mut self, count: usize, plus_indent: Indent) {
        self.ops.ensure_blank_lines(count, plus_indent);
        self.spaced = true;
    }

    /// Open a level, tracking the structural indent used for comment budgets.
    fn open(&mut self, plus_indent: Indent) {
        if let Indent::Const(columns) = plus_indent {
            self.indent = self
                .indent
                .saturating_add(usize::try_from(columns.max(0)).unwrap_or(0));
        }
        self.ops.open(plus_indent);
    }

    /// Close the innermost level.
    fn close(&mut self) {
        self.ops.close();
    }

    /// Open a level, keeping the structural indent where it was — for a level that groups without
    /// indenting, so a comment inside it keeps the enclosing budget.
    fn open_flat(&mut self, plus_indent: Indent) {
        self.ops.open(plus_indent);
    }

    /// Close a level opened by [`open`](Self::open), restoring the structural indent.
    fn close_indent(&mut self, plus_indent: &Indent) {
        if let Indent::Const(columns) = plus_indent {
            self.indent = self
                .indent
                .saturating_sub(usize::try_from((*columns).max(0)).unwrap_or(0));
        }
        self.ops.close();
    }

    // ===== Comments =====

    /// Emit a node's own-line leading comments **before** the node's rule opens any level.
    ///
    /// Called from the dispatcher for every node, so a comment written above a declaration lands
    /// at the indent of whatever *contains* the declaration rather than at the continuation
    /// indent the declaration's header is about to open. Chains of nodes that share a first token
    /// (`EXPR_STMT` → `CALL_EXPR` → `FIELD_ACCESS` → …) hoist once: the outermost one wins, which
    /// is exactly where the comment belongs.
    ///
    /// google-java-format spells the same idea as `Token.plusIndentCommentsBefore`; hoisting is
    /// the shape that falls out of a visitor which opens its levels itself.
    fn hoist_leading_comments(&mut self, node: &SyntaxNode) {
        // The compilation unit opens no level of its own, so hoisting to it gains nothing — and
        // it would emit the first declaration's own Javadoc *before* that declaration negotiates
        // its separation, turning `around-type` into a blank line between a type and its doc.
        if node.kind() == S::SOURCE_FILE {
            return;
        }
        let Some(first) = Self::first_token(node) else {
            return;
        };
        let offset = usize::from(first.text_range().start());
        if self.hoisted.contains(&offset) || self.comments.leading(&first).is_empty() {
            return;
        }
        self.hoisted.insert(offset);
        self.emit_own_line_comments(&first);
    }

    /// Emit the comments anchored before `tok`.
    fn emit_leading(&mut self, tok: &SyntaxToken) {
        let offset = usize::from(tok.text_range().start());
        if !self.hoisted.contains(&offset) {
            self.emit_own_line_comments(tok);
        }
        // A hugging comment sits immediately before the token, on the same line, and takes that
        // token's own spacing on its left: `java.lang./* @A */ String` writes the comment against
        // the `.` because the name it annotates would be written there too.
        let hugging = self.comments.leading_inline(tok).to_vec();
        for comment in &hugging {
            // `OpsBuilder.build` puts a break in front of every comment it inserts before a
            // token — UNIFIED for a block comment, so a list whose items carry comments goes one
            // per line as soon as it wraps at all, and stays on one line while it fits.
            if self.spaced {
                if comment.breaks {
                    self.ops.unify_last_break();
                }
            } else if self.previous.is_some() {
                let space = self
                    .previous
                    .as_ref()
                    .is_some_and(|previous| Spacing::between(previous, tok, self.style));
                self.ops.brk(
                    crate::ir::FillMode::Unified,
                    Self::flat_space(space),
                    Indent::ZERO,
                    None,
                );
            }
            self.emit_comment(comment, false);
            self.space();
        }
    }

    /// Emit the own-line comments anchored before `tok`, each on its own line.
    ///
    /// The gap *between the last comment and `tok`* is its own blank line, separate from the one
    /// before the first comment: `// TODO` followed by a blank line and then a method means the
    /// comment heads the section, not the method. Only the leading gap reaches the caller through
    /// [`Ctx::blank_lines_before`], so this restores the trailing one.
    fn emit_own_line_comments(&mut self, tok: &SyntaxToken) {
        let owed = core::mem::take(&mut self.owed_after_comments);
        let comments = self.comments.leading(tok).to_vec();
        if comments.is_empty() {
            return;
        }
        for comment in &comments {
            if !self.ops.is_empty() {
                if self.ops.last_is_break() {
                    self.ops.force_last_break();
                } else {
                    self.forced_break(Indent::ZERO);
                }
            }
            self.emit_comment_line(comment);
            self.forced_break(Indent::ZERO);
        }
        // A Javadoc documents the declaration that follows, so how much of the gap between the
        // two survives is its own rule — zero is google-java-format's `allowBlankAfterLastComment`
        // returning false for a doc comment. Behind a `//` or a plain `/* … */` the ordinary
        // in-code cap applies, and a gap the caller is owed cannot be lost.
        let blanks = &self.style.cfg.blank_lines;
        let documenting = comments
            .last()
            .is_some_and(|comment| comment.kind == S::DOC_COMMENT);
        let after = if documenting {
            Self::source_blank_lines(tok).min(blanks.max_after_doc_comment)
        } else {
            Self::source_blank_lines(tok)
                .min(blanks.max_in_code)
                .max(owed)
        };
        if after > 0 {
            self.ops.ensure_blank_lines(after, Indent::ZERO);
        }
    }

    /// Emit the own-line comments before `tok` into the level that is open *now*, leaving the
    /// break that follows to the caller.
    ///
    /// A comment written just before a body's closing brace documents the body, not the brace, so
    /// it keeps the body's indent — google-java-format spells the same thing as the `plusTwo`
    /// argument of `token("}", plusTwo)`. Reaching it through the ordinary token path would emit
    /// the comment after the body level has already closed, at the brace's own indent.
    ///
    /// Returns whether anything was emitted.
    fn hoist_comments_before(&mut self, tok: &SyntaxToken) -> bool {
        let comments = self.comments.leading(tok).to_vec();
        if comments.is_empty() {
            return false;
        }
        self.hoisted.insert(usize::from(tok.text_range().start()));
        for comment in &comments {
            if !self.ops.is_empty() && !self.ops.last_is_break() {
                self.forced_break(Indent::ZERO);
            }
            self.emit_comment_line(comment);
        }
        true
    }

    /// Emit the comments anchored after `tok`.
    fn emit_trailing(&mut self, tok: &SyntaxToken) {
        for comment in self.comments.trailing(tok).to_vec() {
            // `tokenBreakTrailingComment`: a block comment written after an opening brace belongs
            // to the body that brace opens, so it takes a line at the body's indent rather than
            // sitting on the header's.
            // An unterminated comment is error recovery — it swallowed the rest of the file, so
            // there is no body for it to belong to.
            if tok.kind() == S::LBRACE && !comment.is_line() && comment.text.ends_with("*/") {
                let indent = self.style.indent();
                self.forced_break(indent);
                self.emit_comment(&comment, true);
                self.ops.force_next_break();
                continue;
            }
            self.space();
            self.emit_comment(&comment, false);
            if comment.is_line() {
                // A `//` swallows the rest of the line, so whatever follows must start a new one.
                self.ops.force_next_break();
            }
        }
        for comment in self.comments.trailing_below(tok).to_vec() {
            self.forced_break(Indent::ZERO);
            self.emit_comment_line(&comment);
        }
    }

    /// Emit a comment that occupies its own line.
    fn emit_comment_line(&mut self, comment: &Comment) {
        if comment.blank_lines_before > 0 {
            let kept = comment
                .blank_lines_before
                .min(self.style.cfg.blank_lines.max_in_code);
            if kept > 0 {
                self.ops.ensure_blank_lines(kept, Indent::ZERO);
            }
        }
        self.emit_comment(comment, true);
        if comment.is_line() {
            self.ops.force_next_break();
        }
    }

    /// Emit a comment's text, reflowed when its `[comments]` rule is on.
    ///
    /// `own_line` says the comment starts a line of its own. A `/* … */` written inside an
    /// expression does not, and the rule that gives a block comment's delimiters lines of their
    /// own would tear the expression across three lines if it applied there.
    fn emit_comment(&mut self, comment: &Comment, own_line: bool) {
        let is_header = !self.header_seen && !Self::documents_a_declaration(comment);
        let text = CommentFormatter::render(
            &comment.text,
            comment.kind,
            self.indent,
            comment.column,
            is_header,
            own_line,
            self.style,
        );
        self.ops.comment(&text);
        self.emitted_comments += 1;
        self.spaced = false;
        self.previous = None;
    }

    /// Whether `comment` is the Javadoc of the declaration it precedes rather than a comment
    /// standing above one.
    ///
    /// The header region ends at the first *declaration*, and a declaration's own doc comment is
    /// part of that declaration — JDT reads it the same way, since a `BodyDeclaration`'s start
    /// offset already includes its Javadoc. Without the distinction the region ends at the first
    /// significant token instead, and a file with no `package` made its own type's Javadoc the
    /// header comment: `format-header = false` then stopped formatting it, in every
    /// default-package file and every file whose licence block is followed straight by a type.
    ///
    /// A licence written as `/** … */` is therefore formatted by the Javadoc rule rather than
    /// held by `format-header`, which is the same answer the references give it.
    const fn documents_a_declaration(comment: &Comment) -> bool {
        matches!(comment.kind, S::DOC_COMMENT)
    }

    // ===== Formatter-disabled regions =====

    /// Whether `tok` falls in a disabled region — emitting the region verbatim the first time,
    /// and suppressing everything afterwards until the region ends.
    fn in_disabled_region(&mut self, tok: &SyntaxToken) -> bool {
        let Some(at) = OffOn::region_at(&self.disabled, tok.text_range().start()) else {
            if self.ops.is_suppressed() {
                self.ops.set_suppressed(false);
                // The region's last line may be a `//` comment, which would swallow whatever
                // followed it. Forcing the *next* break rather than emitting one here keeps the
                // indent decision with the structure that owns it.
                self.ops.force_next_break();
                self.spaced = true;
                self.previous = None;
            }
            return false;
        };
        if !self.emitted[at] {
            self.emitted[at] = true;
            self.ops.set_suppressed(false);
            if !self.ops.level_is_empty() {
                self.ops.forced_break(Indent::ZERO);
            }
            let region = self.disabled[at];
            let text = &self.src[usize::from(region.start())..usize::from(region.end())];
            self.ops.verbatim(text);
            self.spaced = false;
            self.previous = None;
        }
        self.ops.set_suppressed(true);
        true
    }

    /// The index of an unemitted disabled region covering `item`, or `None`.
    ///
    /// Bodies consult this *before* emitting an item's separator, so the region's verbatim text
    /// lands at the body's own indent rather than at whatever depth the item's rule would have
    /// opened.
    fn disabled_region_of(&self, item: &SyntaxElement) -> Option<usize> {
        if self.disabled.is_empty() {
            return None;
        }
        let first = match item {
            SyntaxElement::Node(node) => Self::first_token(node)?,
            SyntaxElement::Token(tok) => tok.clone(),
        };
        OffOn::region_at(&self.disabled, first.text_range().start())
    }

    /// Emit a disabled region verbatim, or skip it when it is already out.
    ///
    /// Returns `true` when the caller should emit a separator before it.
    fn take_disabled_region(&mut self, at: usize) -> bool {
        if self.emitted[at] {
            return false;
        }
        self.emitted[at] = true;
        true
    }

    /// The source text of a disabled region.
    fn disabled_text(&self, at: usize) -> &'a str {
        let region = self.disabled[at];
        &self.src[usize::from(region.start())..usize::from(region.end())]
    }

    /// Emit a disabled region's verbatim text.
    fn emit_disabled(&mut self, at: usize) {
        let text = self.disabled_text(at);
        self.ops.verbatim(text);
        self.spaced = false;
        self.previous = None;
    }

    // ===== Shared queries =====

    /// A node's direct children, with trivia tokens dropped.
    fn children(node: &SyntaxNode) -> Vec<SyntaxElement> {
        node.children_with_tokens()
            .filter(|child| !child.as_token().is_some_and(|tok| tok.kind().is_trivia()))
            .collect()
    }

    /// The node's first direct token of `kind`, if any.
    fn token_of(node: &SyntaxNode, kind: S) -> Option<SyntaxToken> {
        node.children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|tok| tok.kind() == kind)
    }

    /// The node's first child node of `kind`, if any.
    fn child_of(node: &SyntaxNode, kind: S) -> Option<SyntaxNode> {
        node.children().find(|child| child.kind() == kind)
    }

    /// The simple names of the well-known type annotations this file imports.
    ///
    /// google-java-format's `TYPE_ANNOTATIONS` / `checkForTypeAnnotation`: the judgement is by
    /// *import*, because `@Nullable` alone says nothing about which `Nullable` it is.
    fn imported_type_annotations(root: &SyntaxNode) -> BTreeSet<String> {
        const WELL_KNOWN: [&str; 4] = [
            "org.jspecify.annotations.NonNull",
            "org.jspecify.annotations.Nullable",
            "org.checkerframework.checker.nullness.qual.NonNull",
            "org.checkerframework.checker.nullness.qual.Nullable",
        ];
        let mut names = BTreeSet::new();
        for import in root.children().filter(|node| node.kind() == S::IMPORT_DECL) {
            let text: String = import
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .filter(|tok| !tok.kind().is_trivia())
                .skip(1)
                .fold(String::new(), |mut text, tok| {
                    text.push_str(tok.text());
                    text
                });
            let qualified = text.trim_end_matches(';');
            if WELL_KNOWN.contains(&qualified)
                && let Some(simple) = qualified.rsplit('.').next()
            {
                names.insert(simple.to_owned());
            }
        }
        names
    }

    /// Whether an annotation node is one of the imported type annotations.
    fn is_type_annotation(&self, node: &SyntaxNode) -> bool {
        if !matches!(node.kind(), S::ANNOTATION | S::ATTRIBUTE) {
            return false;
        }
        let mut names = node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| tok.kind() == S::IDENT);
        let Some(first) = names.next() else {
            return false;
        };
        // Only a bare simple name qualifies, as `isTypeAnnotation` requires.
        names.next().is_none() && self.type_annotations.contains(first.text())
    }

    /// How wide a node's *source text* is, ignoring the whitespace it starts with.
    ///
    /// A `rowan` node's range begins at its leading trivia, so an operand written on a
    /// continuation line measures a dozen columns wider than it reads. Every width test that asks
    /// "is this item short" has to start at the first token instead — otherwise formatting an
    /// already-wrapped expression decides differently from formatting the same expression written
    /// on one line, and the result is not idempotent.
    fn source_width(node: &SyntaxNode) -> usize {
        let Some(first) = Self::first_token(node) else {
            return 0;
        };
        let start = usize::from(first.text_range().start());
        let own = usize::from(node.text_range().end()).saturating_sub(start);
        // An item runs to the token that ends it, which is how javac reports an expression's
        // extent and therefore what google-java-format measures: a comment written after the item
        // counts as part of it, and `f(false /* why */, …)` goes one argument per line. The walk
        // stops at a newline so that an item the input had already wrapped measures the same as
        // one written inline — otherwise formatting would not be idempotent.
        let mut cursor = node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| !tok.kind().is_trivia())
            .last()
            .and_then(|tok| tok.next_token());
        while let Some(tok) = cursor {
            if tok.kind() == S::NEWLINE {
                break;
            }
            if !tok.kind().is_trivia() {
                return usize::from(tok.text_range().start()).saturating_sub(start);
            }
            cursor = tok.next_token();
        }
        own
    }

    /// The first significant token anywhere under `node`.
    fn first_token(node: &SyntaxNode) -> Option<SyntaxToken> {
        node.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|tok| !tok.kind().is_trivia())
    }

    /// How many blank lines the source had before `node`.
    ///
    /// The one fact the engine reads from input whitespace (`DESIGN.md` §17). When the node has
    /// leading comments the count belongs to the *first comment*, because that is what the blank
    /// line was separating.
    fn blank_lines_before(&self, node: &SyntaxNode) -> usize {
        let Some(first) = Self::first_token(node) else {
            return 0;
        };
        if !self.comments.leading(&first).is_empty() {
            return self.comments.blank_lines_before(&first);
        }
        Self::source_blank_lines(&first)
    }

    /// Blank lines immediately before a token in the source.
    fn source_blank_lines(tok: &SyntaxToken) -> usize {
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
}
