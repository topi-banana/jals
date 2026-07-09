//! `missing-braces`: flag a control-flow body that is a bare statement instead of a `{ ... }`
//! block.
//!
//! Covers `if` / `else`, `while`, `for`, the enhanced `for`, and `do`. An `else if` chain is not
//! flagged for the `else` (the trailing `if` is itself checked on its own).

use alloc::format;
use alloc::vec::Vec;

use jals_syntax::SyntaxKind::{
    self, ASSERT_STMT, BLOCK, BREAK_STMT, CONTINUE_STMT, DO_WHILE_STMT, EMPTY_STMT, EXPR_STMT,
    FOR_EACH_STMT, FOR_STMT, IF_STMT, LABELED_STMT, LOCAL_VAR_DECL, RETURN_STMT, SWITCH_STMT,
    SYNCHRONIZED_STMT, THROW_STMT, TRY_STMT, WHILE_STMT, YIELD_STMT,
};
use jals_syntax::SyntaxNode;

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub const RULE: RuleMeta = RuleMeta {
    name: "missing-braces",
    default: Severity::Warn,
    check: Checker::Syntactic(check),
};

fn check(root: &SyntaxNode) -> Vec<Finding> {
    let mut out = Vec::new();
    for node in root.descendants() {
        match node.kind() {
            IF_STMT => check_if(&node, &mut out),
            WHILE_STMT | FOR_STMT | FOR_EACH_STMT | DO_WHILE_STMT => {
                // The body is the last statement-shaped child (a `for`'s init declaration is also
                // a statement, but always precedes the body).
                if let Some(body) = node.children().filter(|c| is_stmt(c.kind())).last()
                    && body.kind() != BLOCK
                {
                    out.push(Finding::at_node(
                        &body,
                        format!(
                            "`{}` body should be wrapped in braces",
                            keyword(node.kind())
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
fn check_if(node: &SyntaxNode, out: &mut Vec<Finding>) {
    let branches: Vec<SyntaxNode> = node.children().filter(|c| is_stmt(c.kind())).collect();
    for (i, branch) in branches.iter().enumerate() {
        if branch.kind() == BLOCK {
            continue;
        }
        // `else if`: the `else` branch is itself an `if`, which is the idiomatic chain.
        if i == 1 && branch.kind() == IF_STMT {
            continue;
        }
        let what = if i == 0 { "if" } else { "else" };
        out.push(Finding::at_node(
            branch,
            format!("`{what}` body should be wrapped in braces"),
        ));
    }
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
