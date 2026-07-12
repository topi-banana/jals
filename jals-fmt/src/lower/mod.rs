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
}

impl<'a> Ctx<'a> {
    /// Lower the whole tree.
    pub(crate) fn lower_root(root: &SyntaxNode, cfg: &'a Config) -> Doc {
        let ctx = Self {
            comments: CommentMap::build(
                root,
                cfg.normalize_parameter_comments,
                cfg.inline_block_comments,
            ),
            cfg,
            rules: Registry::from_config(cfg),
        };
        let body = ctx.lower(root);
        // Append any orphan comments (a file containing only comments has no token to anchor
        // them to). `orphan_doc` is empty unless the file has no significant tokens.
        Doc::concat(vec![body, ctx.comments.orphan_doc()])
    }

    /// Lower a node, dispatching on its kind.
    pub(crate) fn lower(&self, node: &SyntaxNode) -> Doc {
        // A structural rule (import / modifier reordering) owns its node's layout wholesale; the
        // lookup is a static O(1) match returning `None` for every other kind.
        if let Some(rule) = self.rules.structural(node.kind()) {
            return rule.lower(node, self);
        }
        match node.kind() {
            S::CLASS_BODY | S::MODULE_BODY | S::BLOCK | S::SWITCH_BLOCK => self.lower_braced(node),
            S::SWITCH_GROUP => self.lower_switch_group(node),
            S::SWITCH_RULE => self.lower_switch_rule(node),
            S::SWITCH_LABEL => self.lower_switch_label(node),
            S::ENUM_BODY => self.lower_enum_body(node),
            S::PARAM_LIST
            | S::ARG_LIST
            | S::RECORD_HEADER
            | S::ANNOTATION_ARG_LIST
            | S::ARRAY_INIT => self.lower_delimited(node),
            S::IF_STMT | S::TRY_STMT | S::DO_WHILE_STMT => self.lower_control_flow(node),
            S::BINARY_EXPR => self.lower_binary(node),
            S::TERNARY_EXPR => self.lower_ternary(node),
            S::UNARY_EXPR => self.lower_unary(node),
            S::CALL_EXPR | S::FIELD_ACCESS => self.lower_chain(node),
            S::NON_SEALED_KW => self.lower_non_sealed(node),
            _ => self.lower_generic(node),
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
}
