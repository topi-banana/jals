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
//! This module is the dispatch core ([`lower`]); the per-shape lowering lives in submodules:
//! [`tokens`] (token emission and spacing), [`inline`] (the generic fallback), [`chains`]
//! (method chains), [`blocks`] (braced bodies / item sequences), [`delimited`] (delimited
//! lists), and [`expr`] (binary / ternary / unary expressions). Items shared across submodules
//! (and with [`crate::rules`]) are re-exported below so they stay reachable as `crate::lower::*`.

use jals_syntax::{SyntaxKind as S, SyntaxNode};

use crate::comments::{self, CommentMap};
use crate::config::Config;
use crate::doc::{Doc, concat};
use crate::rules::Registry;

mod blocks;
mod chains;
mod delimited;
mod enums;
mod expr;
mod inline;
mod tokens;

pub(crate) use blocks::{
    blank_lines_before, break_before, item_separator, lower_braced, lower_items,
    lower_switch_group, lower_switch_rule,
};
pub(crate) use chains::lower_chain;
pub(crate) use delimited::lower_delimited;
pub(crate) use enums::lower_enum_body;
pub(crate) use expr::{lower_binary, lower_ternary, lower_unary};
pub(crate) use inline::{lower_control_flow, lower_elements, lower_generic, lower_inline};
pub(crate) use tokens::{first_sig_token, last_sig_token, sep, tight_sep, tok};

/// Lowering context shared (immutably) across the walk.
pub(crate) struct Ctx<'a> {
    pub(crate) comments: CommentMap,
    pub(crate) cfg: &'a Config,
    /// The opt-in rules (literal rewrites, structural reordering) resolved from `cfg`.
    pub(crate) rules: Registry,
}

/// Lower the whole tree.
pub(crate) fn lower_root(root: &SyntaxNode, cfg: &Config) -> Doc {
    let ctx = Ctx {
        comments: comments::build(
            root,
            cfg.normalize_parameter_comments,
            cfg.inline_block_comments,
        ),
        cfg,
        rules: Registry::from_config(cfg),
    };
    let body = lower(root, &ctx);
    // Append any orphan comments (a file containing only comments has no token to anchor
    // them to). `orphan_doc` is empty unless the file has no significant tokens.
    concat(vec![body, ctx.comments.orphan_doc()])
}

/// Lower a node, dispatching on its kind.
pub(crate) fn lower(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    // A structural rule (import / modifier reordering) owns its node's layout wholesale; the
    // lookup is a static O(1) match returning `None` for every other kind.
    if let Some(rule) = ctx.rules.structural(node.kind()) {
        return rule.lower(node, ctx);
    }
    match node.kind() {
        S::CLASS_BODY | S::MODULE_BODY | S::BLOCK | S::SWITCH_BLOCK => lower_braced(node, ctx),
        S::SWITCH_GROUP => lower_switch_group(node, ctx),
        S::SWITCH_RULE => lower_switch_rule(node, ctx),
        S::ENUM_BODY => lower_enum_body(node, ctx),
        S::PARAM_LIST | S::ARG_LIST | S::RECORD_HEADER | S::ANNOTATION_ARG_LIST | S::ARRAY_INIT => {
            lower_delimited(node, ctx)
        }
        S::IF_STMT | S::TRY_STMT | S::DO_WHILE_STMT => lower_control_flow(node, ctx),
        S::BINARY_EXPR => lower_binary(node, ctx),
        S::TERNARY_EXPR => lower_ternary(node, ctx),
        S::UNARY_EXPR => lower_unary(node, ctx),
        S::CALL_EXPR | S::FIELD_ACCESS => lower_chain(node, ctx),
        S::NON_SEALED_KW => lower_non_sealed(node, ctx),
        _ => lower_generic(node, ctx),
    }
}

/// Lower the `non-sealed` modifier. Its three tokens (`non` `-` `sealed`) form one keyword, so
/// they are emitted tight (no spaces) — the generic path would insert spaces and produce the
/// non-keyword `non - sealed`. Comments attached to any of the tokens are preserved.
fn lower_non_sealed(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let parts: Vec<Doc> = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| tok(&t, ctx))
        .collect();
    concat(parts)
}
