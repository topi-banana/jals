//! Generic (inline) lowering — the universal fallback.
//!
//! Lays a node out on one logical line with normalized spacing: child nodes recurse through
//! [`Ctx::lower`], tokens are separated per [`Ctx::sep`]. [`Ctx::lower_elements`] is the shared
//! element loop, reused both for a whole node's children ([`Ctx::lower_inline`]) and for
//! chain-selector emission (`crate::lower::chains`). Control-flow statements route through here
//! too, with `control_flow` set so `control-brace-style = next-line` can push a `}`-anchored
//! continuation to its own line.

use alloc::vec;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::ControlBraceStyle;
use crate::doc::Doc;
use crate::lower::Ctx;

impl Ctx<'_> {
    /// Lay a node out inline: child nodes are recursed, tokens are separated by single
    /// spaces per [`Ctx::want_space`]. Whitespace, newlines, and comment trivia are skipped
    /// here (comments are injected via [`Ctx::tok`]).
    pub(crate) async fn lower_generic(&self, node: &SyntaxNode) -> Doc {
        self.lower_inline(node, false).await
    }

    /// Lay out a control-flow statement (`if` / `try` / `do-while`) inline, honoring
    /// [`ControlBraceStyle`]: under `next-line`, a continuation that directly follows a closing
    /// brace (`} else`, `} catch`, `} finally`, `} while`) moves onto its own line. (The opening
    /// brace of each block is handled separately, by `lower_braced` via `opens_on_next_line`.)
    /// With the default `same-line` it is byte-for-byte identical to [`Ctx::lower_generic`].
    pub(crate) async fn lower_control_flow(&self, node: &SyntaxNode) -> Doc {
        self.lower_inline(node, true).await
    }

    /// Shared core of [`Ctx::lower_generic`] and [`Ctx::lower_control_flow`]. When `control_flow`
    /// is set, the separator before a `}`-anchored continuation becomes a forced break under
    /// `control-brace-style = next-line` (see [`Ctx::flow_sep`]).
    pub(crate) async fn lower_inline(&self, node: &SyntaxNode, control_flow: bool) -> Doc {
        self.lower_elements(node.children_with_tokens(), control_flow)
            .await
    }

    /// Lay out an arbitrary run of CST elements inline. The element loop is shared between
    /// [`Ctx::lower_inline`] (a whole node's children) and chain-selector emission, which feeds it
    /// a `FIELD_ACCESS`'s children minus the receiver (see `lower_after_first_node`); routing both
    /// through here keeps the type-witness hug below in one place.
    pub(crate) async fn lower_elements(
        &self,
        els: impl Iterator<Item = SyntaxElement>,
        control_flow: bool,
    ) -> Doc {
        let mut parts: Vec<Doc> = Vec::new();
        let mut prev: Option<SyntaxToken> = None;
        // A `.<T>` / `::<T>` explicit type witness hugs the method name that follows it
        // (`List.<String>of()`, `Foo::<String>bar`), unlike a type's own `<...>` (`List<T> x`).
        let mut hug_witness = false;

        for el in els {
            if let Some(child) = el.as_node() {
                // `switch-expression-on-new-line`: a `switch` expression that is the value of a `=`
                // (a variable / field initializer or an assignment) breaks onto its own
                // continuation-indented line, instead of riding on the `=` line. The `=`-then-switch
                // shape only occurs in assignment / initializer contexts (a switch is not a legal
                // annotation default / argument value), so this never misfires; `return switch …`
                // has no `=` and stays inline. Layout-only — only the inter-token whitespace changes.
                if self.cfg.switch_expression_on_new_line
                    && child.kind() == S::SWITCH_EXPR
                    && prev.as_ref().map(SyntaxToken::kind) == Some(S::EQ)
                {
                    parts.push(Doc::continuation_indent(Doc::concat(vec![
                        Doc::hardline(),
                        self.lower(child).await,
                    ])));
                    prev = Self::last_sig_token(child);
                    hug_witness = false;
                    continue;
                }
                // A reordered `MODIFIERS` node emits its tokens in a different order than the tree,
                // so the separators around it must use the *emitted* boundary tokens (see
                // `rules::modifiers::emitted_boundary_tokens`); every other node emits in tree order.
                let (emitted_first, emitted_last) = if child.kind() == S::MODIFIERS {
                    crate::rules::modifiers::ModifierRule::emitted_boundary_tokens(child, self.cfg)
                } else {
                    (Self::first_sig_token(child), Self::last_sig_token(child))
                };
                if let Some(first) = emitted_first.as_ref() {
                    let s = if hug_witness {
                        // Hug the name onto the type witness (`List.<String>of`), but keep the
                        // fusion-safety net: a malformed witness can end in a token that would fuse
                        // with the name (`<void` then `x` → `voidx`), so a space is still needed
                        // there.
                        Self::tight_sep(prev.as_ref(), first).await
                    } else {
                        self.flow_sep(control_flow, prev.as_ref(), child.kind(), first)
                            .await
                    };
                    parts.push(s);
                }
                hug_witness = child.kind() == S::TYPE_ARGS
                    && matches!(
                        prev.as_ref().map(SyntaxToken::kind),
                        Some(S::DOT | S::COLON_COLON)
                    );
                parts.push(self.lower(child).await);
                if let Some(last) = emitted_last {
                    prev = Some(last);
                }
            } else if let Some(t) = el.as_token() {
                if t.kind().is_trivia() {
                    continue;
                }
                let s = if hug_witness {
                    Self::tight_sep(prev.as_ref(), t).await
                } else {
                    self.flow_sep(control_flow, prev.as_ref(), t.kind(), t)
                        .await
                };
                parts.push(s);
                hug_witness = false;
                parts.push(self.tok(t));
                prev = Some(t.clone());
            }
        }
        Doc::concat(parts)
    }

    /// Whether `kind` identifies a control-flow continuation that `control-brace-style` may push
    /// to the next line: the `else` / `while` token of an `if` / `do-while`, or a `catch` /
    /// `finally` clause node of a `try`.
    const fn is_continuation(kind: S) -> bool {
        matches!(
            kind,
            S::ELSE_KW | S::WHILE_KW | S::CATCH_CLAUSE | S::FINALLY_CLAUSE
        )
    }

    /// The separator before a child element (`next`, of kind `next_kind`). When `control_flow`
    /// is set, `control-brace-style = next-line` forces a break before a continuation that
    /// directly follows a closing brace; otherwise the normal token spacing from [`Ctx::sep`].
    async fn flow_sep(
        &self,
        control_flow: bool,
        prev: Option<&SyntaxToken>,
        next_kind: S,
        next: &SyntaxToken,
    ) -> Doc {
        if control_flow
            && self.cfg.control_brace_style == ControlBraceStyle::NextLine
            && Self::is_continuation(next_kind)
            && prev.map(SyntaxToken::kind) == Some(S::RBRACE)
        {
            return Doc::hardline();
        }
        self.sep(prev, next).await
    }
}
