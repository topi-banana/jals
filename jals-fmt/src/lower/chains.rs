//! Method chains (`a.b().c().d()`).
//!
//! A chain with at least two method calls is laid out breakable: the receiver and any leading
//! field accesses stay on the first line, the first call hugs them, and each later `.call()` /
//! `.field` wraps onto its own indented line when the chain does not fit `max-width` or its flat
//! width exceeds `chain-width`. Anything else (a lone call, a pure field path, a malformed node)
//! falls back to inline emission, byte-for-byte unchanged.

use alloc::vec;
use alloc::vec::Vec;

use jals_syntax::{SyntaxKind as S, SyntaxNode};

use crate::doc::{Doc, concat, continuation_indent, group_within, softline};
use crate::lower::{Ctx, lower, lower_elements, lower_inline};

/// One `.selector` step of a method chain. `callee` is the `FIELD_ACCESS` carrying the dot,
/// optional type witness, and member name; `call` is the enclosing `CALL_EXPR` when the step
/// is a method invocation (it holds the `ARG_LIST`), or `None` for a plain field access.
struct ChainLink {
    callee: SyntaxNode,
    call: Option<SyntaxNode>,
}

impl ChainLink {
    fn is_call(&self) -> bool {
        self.call.is_some()
    }
}

/// Lower a `FIELD_ACCESS` / `CALL_EXPR`. A chain with at least two method calls is laid out
/// breakable: the receiver and any leading field accesses stay on the first line, the first
/// call hugs them, and each later `.call()` / `.field` wraps onto its own indented line when
/// the chain does not fit `max-width` or its flat width exceeds `chain-width`. Anything else
/// (a lone call, a pure field path `a.b.c`, a malformed node) falls back to inline emission,
/// byte-for-byte unchanged.
pub(crate) fn lower_chain(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let Some((head, links)) = flatten_chain(node) else {
        return lower_inline(node, ctx, false);
    };
    // Count every method invocation in the chain — the head itself if it is a call/`new`, plus
    // each call link — so `a.b.c` (no calls) and `foo.bar()` (one) stay inline.
    let calls = links.iter().filter(|l| l.is_call()).count()
        + usize::from(matches!(head.kind(), S::CALL_EXPR | S::NEW_EXPR));
    if calls < 2 {
        return lower_inline(node, ctx, false);
    }

    // Leading field accesses (before the first call) ride on the head's line, so
    // `this.config.foo().bar()` keeps `this.config` together instead of breaking every dot.
    let first_call = links.iter().position(ChainLink::is_call).unwrap_or(0);
    let (lead, rest) = links.split_at(first_call);

    let mut head_line = vec![lower(&head, ctx)];
    for link in lead {
        head_line.push(lower_link(link, ctx));
    }
    // The first call hugs the head; subsequent steps wrap one per line.
    let mut rest = rest.iter();
    if let Some(first) = rest.next() {
        head_line.push(lower_link(first, ctx));
    }
    let mut wrapped: Vec<Doc> = Vec::new();
    for link in rest {
        wrapped.push(softline());
        wrapped.push(lower_link(link, ctx));
    }

    let doc = concat(vec![
        concat(head_line),
        continuation_indent(concat(wrapped)),
    ]);
    group_within(doc, ctx.cfg.chain_width)
}

/// Lower one chain step: its `.selector`, plus the argument list when it is a call.
fn lower_link(link: &ChainLink, ctx: &Ctx<'_>) -> Doc {
    let selector = lower_after_first_node(&link.callee, ctx);
    match &link.call {
        Some(call) => concat(vec![selector, lower_after_first_node(call, ctx)]),
        None => selector,
    }
}

/// Flatten a left-nested chain into its head (base) expression and the `.selector` steps in
/// source order. Returns `None` when `node` applies no `.`-selector to a receiver (so it is
/// not a chain), letting the caller fall back to inline emission.
fn flatten_chain(node: &SyntaxNode) -> Option<(SyntaxNode, Vec<ChainLink>)> {
    let mut links: Vec<ChainLink> = Vec::new();
    let mut cur = node.clone();
    let head = loop {
        match cur.kind() {
            S::FIELD_ACCESS => {
                let recv = first_child_node(&cur)?;
                links.push(ChainLink {
                    callee: cur.clone(),
                    call: None,
                });
                cur = recv;
            }
            S::CALL_EXPR => {
                let callee = first_child_node(&cur)?;
                // `foo(...)` (callee is a bare name, not `recv.method`) is the chain head.
                if callee.kind() != S::FIELD_ACCESS {
                    break cur;
                }
                let recv = first_child_node(&callee)?;
                links.push(ChainLink {
                    callee,
                    call: Some(cur.clone()),
                });
                cur = recv;
            }
            _ => break cur,
        }
    };
    if links.is_empty() {
        return None;
    }
    links.reverse();
    Some((head, links))
}

/// The first child node (skipping tokens) of `node`.
fn first_child_node(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.children().next()
}

/// Lower every child of `node` except its first child node, reusing the inline element loop.
/// For a chain step the dropped child is the receiver / callee (the spine continuation, lowered
/// separately), so the emitted part is exactly this step's `.`, type witness, name, and — for a
/// `CALL_EXPR` — its argument list. Every token is still emitted exactly once.
fn lower_after_first_node(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let mut dropped = false;
    let els = node.children_with_tokens().filter(move |el| {
        if !dropped && el.as_node().is_some() {
            dropped = true;
            return false;
        }
        true
    });
    lower_elements(els, ctx, false)
}
