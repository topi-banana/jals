//! `missing-braces`: flag a control-flow body that is a bare statement instead of a `{ ... }`
//! block.
//!
//! Covers `if` / `else`, `while`, `for`, the enhanced `for`, and `do`. An `else if` chain is not
//! flagged for the `else` (the trailing `if` is itself checked on its own).
//!
//! [`BracePolicy`] chooses *when* a block is required. Under
//! [`MultiLine`](BracePolicy::MultiLine) a body that shares its keyword's line (`if (x) return;`)
//! passes and one written on the next line does not — the guard clause everyone writes, without
//! the dangling-body hazard braces exist to prevent. That is the only input the rule takes from
//! the source's whitespace, and it takes it from the tokens rather than from a line index, so the
//! rule stays a pure CST walk.

use alloc::format;
use alloc::vec::Vec;

use jals_exec::{LocalBoxFuture, Yielder};
use jals_syntax::SyntaxKind::{
    self, ASSERT_STMT, BLOCK, BREAK_STMT, CONTINUE_STMT, DO_WHILE_STMT, EMPTY_STMT, EXPR_STMT,
    FOR_EACH_STMT, FOR_STMT, IF_STMT, LABELED_STMT, LOCAL_VAR_DECL, RETURN_STMT, SWITCH_STMT,
    SYNCHRONIZED_STMT, THROW_STMT, TRY_STMT, WHILE_STMT, YIELD_STMT,
};
use jals_syntax::SyntaxNode;

use jals_config::Category;
use jals_config::lint::{BracePolicy, Config};

use crate::rules::{Checker, Finding, RuleMeta, Significant};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "missing-braces",
    category: Category::Style,
    level: |config| config.style.missing_braces.level,
    needs_clean_parse: false,
    check: Checker::Syntactic(MissingBraces::check),
};

/// The `missing-braces` rule.
struct MissingBraces;

impl MissingBraces {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(root: &'a SyntaxNode, config: &'a Config) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(root, config))
    }

    async fn check_impl(root: &SyntaxNode, config: &Config) -> Vec<Finding> {
        let policy = config.style.missing_braces.options.policy;
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            match node.kind() {
                IF_STMT => Self::check_if(&node, policy, &mut out),
                WHILE_STMT | FOR_STMT | FOR_EACH_STMT | DO_WHILE_STMT => {
                    // The body is the last statement-shaped child (a `for`'s init declaration is
                    // also a statement, but always precedes the body).
                    if let Some(body) = node.children().filter(|c| Self::is_stmt(c.kind())).last()
                        && body.kind() != BLOCK
                        && Self::requires_braces(&node, &body, policy)
                    {
                        out.push(Finding::at_node(
                            &body,
                            format!(
                                "`{}` body should be wrapped in braces",
                                Self::keyword(node.kind())
                            ),
                        ));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The two branches of an `if` are its statement-shaped children: `[then]` or `[then, else]`
    /// (the condition is an expression, not a statement).
    fn check_if(node: &SyntaxNode, policy: BracePolicy, out: &mut Vec<Finding>) {
        let branches: Vec<SyntaxNode> = node
            .children()
            .filter(|c| Self::is_stmt(c.kind()))
            .collect();
        for (i, branch) in branches.iter().enumerate() {
            if branch.kind() == BLOCK {
                continue;
            }
            // `else if`: the `else` branch is itself an `if`, which is the idiomatic chain.
            if i == 1 && branch.kind() == IF_STMT {
                continue;
            }
            if !Self::requires_braces(node, branch, policy) {
                continue;
            }
            let what = if i == 0 { "if" } else { "else" };
            out.push(Finding::at_node(
                branch,
                format!("`{what}` body should be wrapped in braces"),
            ));
        }
    }

    /// Whether `body` needs a block under `policy`.
    ///
    /// [`Always`](BracePolicy::Always) says yes without looking.
    /// [`MultiLine`](BracePolicy::MultiLine) says yes only when the statement does not fit on one
    /// line, which is read off the tokens between the statement's first significant token and the
    /// body's last: a newline anywhere in that window — in whitespace or inside a comment — means
    /// the body left its keyword's line.
    ///
    /// The window is [`Significant::range`] on both ends and **not** the node ranges. rowan parks a
    /// statement's leading trivia inside the statement, so `stmt.text_range()` begins at the newline
    /// that ended the *previous* line; measuring from there would report every guard clause not on
    /// the first line of its block, which is nearly all of them.
    fn requires_braces(stmt: &SyntaxNode, body: &SyntaxNode, policy: BracePolicy) -> bool {
        if policy == BracePolicy::Always {
            return true;
        }
        let (Some(head), Some(tail)) = (Significant::range(stmt), Significant::range(body)) else {
            return true;
        };
        stmt.descendants_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| {
                let range = token.text_range();
                usize::from(range.start()) >= head.start && usize::from(range.end()) <= tail.end
            })
            .any(|token| token.text().contains('\n'))
    }

    /// The keyword to name in the message for a loop statement.
    const fn keyword(kind: SyntaxKind) -> &'static str {
        match kind {
            WHILE_STMT => "while",
            FOR_STMT | FOR_EACH_STMT => "for",
            DO_WHILE_STMT => "do",
            _ => "loop",
        }
    }

    /// Whether `kind` is a statement node kind (the shapes that can appear as a control-flow body).
    const fn is_stmt(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            LOCAL_VAR_DECL
                | BLOCK
                | EXPR_STMT
                | RETURN_STMT
                | IF_STMT
                | WHILE_STMT
                | FOR_STMT
                | FOR_EACH_STMT
                | DO_WHILE_STMT
                | BREAK_STMT
                | CONTINUE_STMT
                | THROW_STMT
                | YIELD_STMT
                | ASSERT_STMT
                | SYNCHRONIZED_STMT
                | TRY_STMT
                | SWITCH_STMT
                | LABELED_STMT
                | EMPTY_STMT
        )
    }
}
