//! Lower the CST into a [`Doc`].
//!
//! Every token is a direct child of exactly one node, so as long as each rule emits all
//! of its direct-child significant tokens (and recurses into child nodes via [`lower`]),
//! every token is emitted exactly once — guaranteeing the significant-token invariant
//! regardless of how much bespoke coverage exists. Comments are attached to tokens by a
//! pre-pass ([`crate::comments`]) and injected when those tokens are emitted, so they are
//! never dropped or duplicated either.
//!
//! Structural nodes (source file, bodies, blocks) get multi-line layout; delimited lists
//! (params, args, array initializers) wrap all-or-nothing (except that
//! `overflow-delimited-expr` may hang a call's trailing lambda / anonymous class / array
//! initializer past the call line); binary/unary expressions get
//! canonical operator spacing, and a binary expression that overflows `max-width` wraps at
//! its operators (placement per `binop-separator`); everything else falls back to
//! [`lower_generic`], which lays a node out inline with normalized spacing. Source-file
//! layout, including import reordering/grouping, lives in [`crate::rules::imports`]; modifier
//! reordering (`reorder-modifiers`) lives in [`crate::rules::modifiers`].
//!
//! This module is the dispatch core ([`Ctx::lower`]); the per-shape lowering lives in submodules:
//! [`tokens`] (token emission and spacing), [`inline`] (the generic fallback), [`chains`]
//! (method chains), [`blocks`] (braced bodies / item sequences), [`delimited`] (delimited
//! lists), and [`expr`] (binary / ternary / unary expressions). All lowering is expressed as
//! methods / associated functions on [`Ctx`] (spread across several `impl` blocks in the
//! submodules), so they are reached through the context rather than as free functions.

use alloc::vec;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode};

use crate::comments::CommentMap;
use crate::config::Config;
use crate::doc::Doc;
use crate::rules::Registry;

mod blocks;
mod chains;
mod delimited;
mod enums;
mod expr;
mod inline;
mod tokens;

/// Lowering context shared (immutably) across the walk.
pub(crate) struct Ctx<'a> {
    pub(crate) comments: CommentMap,
    pub(crate) cfg: &'a Config,
    /// The opt-in rules (literal rewrites, structural reordering) resolved from `cfg`.
    pub(crate) rules: Registry,
    /// Amortized-yield countdown shared by the whole lowering walk (a `Cell`, not a `Yielder`,
    /// because the walk holds `&self` — every lowered node draws on the one budget).
    yield_left: core::cell::Cell<u32>,
}

impl<'a> Ctx<'a> {
    /// Lower the whole tree.
    pub(crate) async fn lower_root(root: &SyntaxNode, cfg: &'a Config) -> Doc {
        let ctx = Self {
            comments: CommentMap::build(
                root,
                cfg.normalize_parameter_comments,
                cfg.inline_block_comments,
            )
            .await,
            cfg,
            rules: Registry::from_config(cfg),
            yield_left: core::cell::Cell::new(jals_exec::Yielder::DEFAULT_PERIOD),
        };
        let body = ctx.lower(root).await;
        // Append any orphan comments (a file containing only comments has no token to anchor
        // them to). `orphan_doc` is empty unless the file has no significant tokens.
        Doc::concat(vec![body, ctx.comments.orphan_doc()])
    }

    /// One unit of lowering work: yields once per [`jals_exec::Yielder::DEFAULT_PERIOD`] nodes.
    async fn tick(&self) {
        let left = self.yield_left.get();
        if left <= 1 {
            self.yield_left.set(jals_exec::Yielder::DEFAULT_PERIOD);
            jals_exec::yield_now().await;
        } else {
            self.yield_left.set(left - 1);
        }
    }

    /// Lower a node, dispatching on its kind.
    ///
    /// The one boxed shim of the lowering recursion: every per-shape method recurses back through
    /// here, so the async cycle has a single `Box::pin` choke point.
    pub(crate) fn lower<'n>(&'n self, node: &'n SyntaxNode) -> jals_exec::LocalBoxFuture<'n, Doc> {
        alloc::boxed::Box::pin(self.lower_impl(node))
    }

    /// The per-node dispatch behind [`Ctx::lower`].
    async fn lower_impl(&self, node: &SyntaxNode) -> Doc {
        self.tick().await;
        // A structural rule (import / modifier reordering) owns its node's layout wholesale; the
        // lookup is a static O(1) match returning `None` for every other kind.
        if let Some(rule) = self.rules.structural(node.kind()) {
            return rule.lower(node, self).await;
        }
        match node.kind() {
            S::CLASS_BODY | S::MODULE_BODY | S::BLOCK | S::SWITCH_BLOCK => {
                self.lower_braced(node).await
            }
            S::SWITCH_GROUP => self.lower_switch_group(node).await,
            S::SWITCH_RULE => self.lower_switch_rule(node).await,
            S::SWITCH_LABEL => self.lower_switch_label(node).await,
            S::ENUM_BODY => self.lower_enum_body(node).await,
            S::PARAM_LIST
            | S::ARG_LIST
            | S::RECORD_HEADER
            | S::ANNOTATION_ARG_LIST
            | S::ARRAY_INIT => self.lower_delimited(node).await,
            S::IF_STMT | S::TRY_STMT | S::DO_WHILE_STMT => self.lower_control_flow(node).await,
            S::BINARY_EXPR => self.lower_binary(node).await,
            S::TERNARY_EXPR => self.lower_ternary(node).await,
            S::UNARY_EXPR => self.lower_unary(node).await,
            S::CALL_EXPR | S::FIELD_ACCESS => self.lower_chain(node).await,
            S::NON_SEALED_KW => self.lower_non_sealed(node),
            S::IMPORT_GROUP => self.lower_import_group(node).await,
            _ => self.lower_generic(node).await,
        }
    }

    /// Lower the `non-sealed` modifier. Its three tokens (`non` `-` `sealed`) form one keyword, so
    /// they are emitted tight (no spaces) — the generic path would insert spaces and produce the
    /// non-keyword `non - sealed`. Comments attached to any of the tokens are preserved.
    fn lower_non_sealed(&self, node: &SyntaxNode) -> Doc {
        let parts: Vec<Doc> = node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|t| !t.kind().is_trivia())
            .map(|t| self.tok(&t))
            .collect();
        Doc::concat(parts)
    }

    /// Lower a jals grouped import's `.{ ... }` to the canonical compact form `.{A, B}`: no padding
    /// inside the braces, exactly one space after each separating comma, and **no trailing comma**.
    /// Members are emitted in authored order (the enclosing import run's reordering keys off the
    /// shared prefix, not the members). Comments attached to any delimiter are preserved via
    /// [`Ctx::tok`].
    ///
    /// Dropping the trailing comma is the formatter's one *unconditional* departure from the
    /// significant-token invariant (see the crate docs) — every other exception is an opt-in config
    /// flag, off by default. It is confined to this node kind, where the comma separates nothing:
    /// the group is always laid out flat, so there is no vertical form for a trailing comma to
    /// serve, and the desugarer already ignores it.
    ///
    /// Unlike [`TrailingCommaRule`](crate::rules::trailing_comma), a *commented* trailing comma is
    /// not kept: only the comma's own text is dropped, while its comments are still emitted
    /// through [`CommentMap::token`](crate::comments::CommentMap::token). Keeping the comma alive
    /// because of its comment would cost idempotency — the comment re-anchors elsewhere in the
    /// output, so the second pass would find a comment-free comma and drop it after all.
    async fn lower_import_group(&self, node: &SyntaxNode) -> Doc {
        let els: Vec<SyntaxElement> = node
            .children_with_tokens()
            .filter(|el| !el.as_token().is_some_and(|t| t.kind().is_trivia()))
            .collect();
        // A comma past the last member separates nothing — that is the group's trailing comma.
        // Found by position rather than by "is the next token `}`" so that error-recovery debris
        // between the two (an `ERROR` node) cannot disguise it as a separator.
        let last_member = els
            .iter()
            .rposition(|el| el.as_node().is_some_and(|n| n.kind() == S::QUALIFIED_NAME));
        let mut parts: Vec<Doc> = Vec::new();
        for (index, el) in els.iter().enumerate() {
            if let Some(child) = el.as_node() {
                parts.push(self.lower(child).await);
            } else if let Some(t) = el.as_token() {
                if t.kind() == S::COMMA && last_member.is_none_or(|last| index > last) {
                    // Emit the token's comments with an empty body: the comma's text goes, its
                    // comments stay.
                    parts.push(self.comments.token(t, Doc::nil()));
                    continue;
                }
                parts.push(self.tok(t));
                if t.kind() == S::COMMA {
                    parts.push(Doc::text(" "));
                }
            }
        }
        Doc::concat(parts)
    }
}
