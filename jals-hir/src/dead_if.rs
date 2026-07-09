//! `constant-condition` analysis: an `if` statement whose condition always evaluates to the same
//! value, making one of its branches dead.
//!
//! A small constant evaluator folds the condition expression: `true` / `false` and integer
//! literals, parentheses, `!` / unary `-` / `+`, the short-circuit operators `&&` / `||` (by
//! three-valued logic — short-circuiting never changes the *value*, so `f() && false` is soundly
//! `false`), equality and integer comparisons, and — through file-local [`Resolved`] bindings —
//! references to *constant variables*: a local or field declared `final` in the same file whose
//! initializer itself folds.
//!
//! **Conservative — never a false positive.** Anything that cannot be proven constant evaluates
//! to "unknown" and is not reported: names that resolve to a non-`final` binding, a parameter, or
//! a declarator without an initializer; member accesses (`this.DEBUG`); calls; assignments
//! (side effects); casts; and every literal shape outside `boolean` / integer. Known misses, by
//! design: an interface field (implicitly `final` without the keyword) and any constant declared
//! in another file. Only `if` statements are examined — `while (true)` / `do … while (false)` are
//! idiomatic and never reported. Cyclic initializers (broken code) terminate via a visiting set;
//! nothing here panics.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_syntax::SyntaxKind::{
    AMP_AMP, BANG, BANG_EQ, EQ, EQ_EQ, FALSE_KW, FIELD_DECL, FINAL_KW, GT, INT_LITERAL,
    LOCAL_VAR_DECL, LT, LT_EQ, MINUS, PIPE_PIPE, PLUS, TRUE_KW,
};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxElement, SyntaxNode};

use crate::def::DefId;
use crate::infer::{declarator_initializers, node_span, op_kinds, token_start};
use crate::resolve::Resolved;
use crate::resolve::collect::first_ident_token;

/// An `if` statement whose condition is provably constant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadIf {
    /// The byte range of the constant condition expression.
    pub condition_range: Range<usize>,
    /// The condition's constant value.
    pub value: bool,
    /// The branch that never executes — the then-branch of an always-false `if`, the else-branch
    /// of an always-true one. `None` when that branch is absent (an always-true `if` with no
    /// `else`).
    pub dead_range: Option<Range<usize>>,
}

impl DeadIf {
    /// The human-readable diagnostic message.
    pub fn message(&self) -> String {
        format!(
            "`if` condition is always {}",
            if self.value { "true" } else { "false" }
        )
    }
}

/// Every `if` statement in `root` whose condition folds to a constant, in source order.
/// `resolved` is the file's name resolution, used to fold `final` constant variables.
pub fn dead_ifs(root: &SyntaxNode, resolved: &Resolved) -> Vec<DeadIf> {
    let mut evaluator = Evaluator {
        root,
        resolved,
        decls: None,
        visiting: Vec::new(),
    };
    let mut out = Vec::new();
    for if_stmt in root.descendants().filter_map(ast::IfStmt::cast) {
        let Some(condition) = if_stmt.condition() else {
            continue; // broken parse (`if () {}`) — nothing to evaluate.
        };
        // A non-boolean constant condition (`if (1)`) is a type error, not a dead branch.
        let Some(ConstValue::Bool(value)) = evaluator.eval(&condition) else {
            continue;
        };
        let mut branches = if_stmt.branches();
        let then_branch = branches.next();
        let else_branch = branches.next();
        let dead = if value { else_branch } else { then_branch };
        out.push(DeadIf {
            condition_range: trimmed_span(condition.syntax()),
            value,
            dead_range: dead.map(|stmt| trimmed_span(stmt.syntax())),
        });
    }
    out
}

/// The byte span of `node` with the leading trivia it carries trimmed off (this CST attaches the
/// trivia between two siblings to the *following* node, so a branch statement's range would
/// otherwise start at the space before it).
fn trimmed_span(node: &SyntaxNode) -> Range<usize> {
    let full = node_span(node);
    let start = node
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|t| !t.kind().is_trivia())
        .map_or(full.start, |t| usize::from(t.text_range().start()));
    start..full.end
}

/// A folded constant value. Java's `boolean` and its integral types as evaluated at `int` / `long`
/// width (both carried as `i64`; width is applied when the literal is parsed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConstValue {
    Bool(bool),
    Int(i64),
}

/// Cap on the `final`-constant chain [`Evaluator::eval_name`] follows. Expression recursion is
/// bounded by the parse tree, but a chain (`final int b = a; final int c = b; …`) is bounded only
/// by the file's definition count — stack insurance for pathological files.
const MAX_CONST_CHAIN: usize = 64;

struct Evaluator<'a> {
    root: &'a SyntaxNode,
    resolved: &'a Resolved,
    /// Each `final` declarator's initializer, keyed by the declaring `IDENT` token start (the same
    /// key as [`Def::name_range`](crate::Def) — how a resolved reference finds its initializer).
    /// Built lazily on the first name lookup — most conditions contain no names. A non-`final`
    /// declarator or one without an initializer is absent (absence means "unknown").
    decls: Option<BTreeMap<usize, ast::Expr>>,
    /// Definitions currently being folded through — terminates cyclic initializers (broken code).
    visiting: Vec<DefId>,
}

impl Evaluator<'_> {
    /// Folds `expr` to a constant, or `None` when it cannot be proven constant.
    fn eval(&mut self, expr: &ast::Expr) -> Option<ConstValue> {
        match expr {
            ast::Expr::Literal(l) => literal_value(l),
            ast::Expr::Paren(p) => self.eval(&p.expr()?),
            ast::Expr::Unary(u) => {
                let value = self.eval(&u.operand()?)?;
                match (*op_kinds(u.syntax()).first()?, value) {
                    (BANG, ConstValue::Bool(b)) => Some(ConstValue::Bool(!b)),
                    // Wrapping matches Java two's complement (`-0x8000000000000000L` is `Long.MIN_VALUE`).
                    (MINUS, ConstValue::Int(v)) => Some(ConstValue::Int(v.wrapping_neg())),
                    (PLUS, ConstValue::Int(v)) => Some(ConstValue::Int(v)),
                    // `~` is out of scope; `++` / `--` have side effects.
                    _ => None,
                }
            }
            ast::Expr::Binary(b) => self.eval_binary(b),
            ast::Expr::NameRef(n) => self.eval_name(n),
            // Everything else either has side effects (assignments, calls), needs types we do not
            // track (member access, casts), or is out of scope (ternaries, switches, …).
            _ => None,
        }
    }

    fn eval_binary(&mut self, expr: &ast::BinaryExpr) -> Option<ConstValue> {
        // Operator spellings as in `infer`: `>` is `GT`, `>=` is `GT EQ`; `instanceof`, shifts
        // (`GT GT`), arithmetic, and non-short-circuit `&` / `|` / `^` all bail here, before
        // either operand is folded.
        let value = match op_kinds(expr.syntax()).as_slice() {
            // Three-valued: one provably-`false` side decides `&&` even if the other side is
            // unknown — short-circuiting affects evaluation, never the value. A deciding lhs
            // skips folding the rhs at all.
            [AMP_AMP] => match self.eval_bool(expr.lhs()) {
                Some(false) => false,
                lhs => match (lhs, self.eval_bool(expr.rhs())) {
                    (_, Some(false)) => false,
                    (Some(true), Some(true)) => true,
                    _ => return None,
                },
            },
            [PIPE_PIPE] => match self.eval_bool(expr.lhs()) {
                Some(true) => true,
                lhs => match (lhs, self.eval_bool(expr.rhs())) {
                    (_, Some(true)) => true,
                    (Some(false), Some(false)) => false,
                    _ => return None,
                },
            },
            [EQ_EQ] => equal(self.eval_opt(expr.lhs())?, self.eval_opt(expr.rhs())?)?,
            [BANG_EQ] => !equal(self.eval_opt(expr.lhs())?, self.eval_opt(expr.rhs())?)?,
            [LT] => self.compare(expr, |a, b| a < b)?,
            [LT_EQ] => self.compare(expr, |a, b| a <= b)?,
            [GT] => self.compare(expr, |a, b| a > b)?,
            [GT, EQ] => self.compare(expr, |a, b| a >= b)?,
            _ => return None,
        };
        Some(ConstValue::Bool(value))
    }

    /// [`eval`](Self::eval) over an optional operand (a missing operand is a broken parse).
    fn eval_opt(&mut self, expr: Option<ast::Expr>) -> Option<ConstValue> {
        self.eval(&expr?)
    }

    /// [`eval_opt`](Self::eval_opt) narrowed to `boolean` — an `Int` is not a Java truth value.
    fn eval_bool(&mut self, expr: Option<ast::Expr>) -> Option<bool> {
        match self.eval_opt(expr)? {
            ConstValue::Bool(b) => Some(b),
            ConstValue::Int(_) => None,
        }
    }

    /// Java's ordering comparisons are integer-only (`boolean` has no `<`).
    fn compare(&mut self, expr: &ast::BinaryExpr, op: fn(i64, i64) -> bool) -> Option<bool> {
        match (self.eval_opt(expr.lhs())?, self.eval_opt(expr.rhs())?) {
            (ConstValue::Int(a), ConstValue::Int(b)) => Some(op(a, b)),
            _ => None,
        }
    }

    /// Folds a name that resolves to a *constant variable*: a same-file local / field declared
    /// `final` whose initializer itself folds. Anything else — a non-`final` binding, a parameter
    /// or other declarator-less definition (absent from `decls`), an unresolved name — is unknown.
    fn eval_name(&mut self, name: &ast::NameRef) -> Option<ConstValue> {
        // References are keyed by the identifier *token* start (a `NAME_REF` node may carry
        // leading trivia), exactly as `infer` looks them up.
        let token = first_ident_token(name.syntax())?;
        let def_id = self
            .resolved
            .reference_at(token_start(&token))?
            .resolution
            .def_id()?;
        if self.visiting.contains(&def_id) || self.visiting.len() >= MAX_CONST_CHAIN {
            return None;
        }
        let name_start = self.resolved.def(def_id).name_range.start;
        let root = self.root;
        let decls = self.decls.get_or_insert_with(|| final_initializers(root));
        let init = decls.get(&name_start)?.clone();
        self.visiting.push(def_id);
        let value = self.eval(&init);
        self.visiting.pop();
        value
    }
}

/// Every `final` declarator's initializer in the file, keyed by the declaring `IDENT` token start.
/// Declarators are paired with their initializers by the same walk `infer`'s initializer check
/// uses ([`declarator_initializers`]).
fn final_initializers(root: &SyntaxNode) -> BTreeMap<usize, ast::Expr> {
    let mut decls = BTreeMap::new();
    for node in root.descendants() {
        if !matches!(node.kind(), LOCAL_VAR_DECL | FIELD_DECL) {
            continue;
        }
        let is_final = node
            .children()
            .find_map(ast::Modifiers::cast)
            .is_some_and(|m| m.has(FINAL_KW));
        if !is_final {
            continue;
        }
        for (name, value) in declarator_initializers(&node) {
            decls.insert(token_start(&name), value);
        }
    }
    decls
}

fn literal_value(literal: &ast::Literal) -> Option<ConstValue> {
    let token = literal.token()?;
    match token.kind() {
        TRUE_KW => Some(ConstValue::Bool(true)),
        FALSE_KW => Some(ConstValue::Bool(false)),
        INT_LITERAL => parse_int_literal(token.text()).map(ConstValue::Int),
        // `char` / `String` / float / `null` literals are out of scope.
        _ => None,
    }
}

/// Java `==` on two constants of the same kind; a mixed comparison (`1 == true`) is a type error
/// and stays unknown.
const fn equal(lhs: ConstValue, rhs: ConstValue) -> Option<bool> {
    match (lhs, rhs) {
        (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(a == b),
        (ConstValue::Int(a), ConstValue::Int(b)) => Some(a == b),
        _ => None,
    }
}

/// Parses a Java integer literal (JLS §3.10.1) to the `i64` Java evaluates it as: decimal, hex
/// `0x`, binary `0b`, and octal `0…` radixes, `_` separators, and an `l` / `L` suffix. An `int`
/// literal is evaluated at 32-bit width and sign-extended (`0xFFFFFFFF` is `-1`, as in Java);
/// out-of-range and malformed literals are `None`. A bare decimal `2147483648` (legal only under
/// unary `-`) is conservatively `None`, so `-2147483648` does not fold.
fn parse_int_literal(text: &str) -> Option<i64> {
    let (body, is_long) = match text.as_bytes().last() {
        Some(b'l' | b'L') => (&text[..text.len() - 1], true),
        _ => (text, false),
    };
    let body: String = body.chars().filter(|&c| c != '_').collect();
    // A radix-prefix chain reads far more clearly as an if/else ladder than nested `map_or_else`.
    #[allow(clippy::option_if_let_else)]
    let (radix, digits, is_decimal) =
        if let Some(rest) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (16, rest, false)
        } else if let Some(rest) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (2, rest, false)
        } else if body.len() > 1 && body.starts_with('0') {
            (8, &body[1..], false)
        } else {
            (10, body.as_str(), true)
        };
    if digits.is_empty() {
        return None;
    }
    let value = u64::from_str_radix(digits, radix).ok()?;
    match (is_decimal, is_long) {
        // A decimal literal is written as a magnitude — it must fit the positive range.
        (true, false) => i32::try_from(value).ok().map(i64::from),
        (true, true) => i64::try_from(value).ok(),
        // A hex / binary / octal literal is a bit pattern — sign-extend from its width.
        (false, false) => u32::try_from(value)
            .ok()
            .map(|bits| i64::from(bits.cast_signed())),
        (false, true) => Some(value.cast_signed()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_decimal_literals() {
        assert_eq!(parse_int_literal("0"), Some(0));
        assert_eq!(parse_int_literal("42"), Some(42));
        assert_eq!(parse_int_literal("1_000"), Some(1000));
        assert_eq!(parse_int_literal("2147483647"), Some(i64::from(i32::MAX)));
        // Legal only under unary `-`, so conservatively unparsed.
        assert_eq!(parse_int_literal("2147483648"), None);
    }

    #[test]
    fn parses_long_literals() {
        assert_eq!(parse_int_literal("42L"), Some(42));
        assert_eq!(parse_int_literal("42l"), Some(42));
        assert_eq!(parse_int_literal("9223372036854775807L"), Some(i64::MAX));
        assert_eq!(parse_int_literal("9223372036854775808L"), None);
    }

    #[test]
    fn parses_hex_binary_octal() {
        assert_eq!(parse_int_literal("0xFF"), Some(255));
        assert_eq!(parse_int_literal("0Xff"), Some(255));
        assert_eq!(parse_int_literal("0b1010"), Some(10));
        assert_eq!(parse_int_literal("0B1010"), Some(10));
        assert_eq!(parse_int_literal("010"), Some(8));
        assert_eq!(parse_int_literal("0xFF_FF"), Some(0xFFFF));
    }

    #[test]
    fn sign_extends_int_width_bit_patterns() {
        // Java evaluates an `int` hex literal at 32-bit width: `0xFFFFFFFF == -1`.
        assert_eq!(parse_int_literal("0xFFFFFFFF"), Some(-1));
        assert_eq!(parse_int_literal("0x80000000"), Some(i64::from(i32::MIN)));
        // Five bytes overflow `int`.
        assert_eq!(parse_int_literal("0x1FFFFFFFF"), None);
        // At `long` width the full pattern wraps instead.
        assert_eq!(parse_int_literal("0xFFFFFFFFFFFFFFFFL"), Some(-1));
        assert_eq!(parse_int_literal("0x8000000000000000L"), Some(i64::MIN));
    }

    #[test]
    fn rejects_malformed_literals() {
        assert_eq!(parse_int_literal(""), None);
        assert_eq!(parse_int_literal("L"), None);
        assert_eq!(parse_int_literal("0x"), None);
        assert_eq!(parse_int_literal("0b"), None);
        assert_eq!(parse_int_literal("08"), None);
        assert_eq!(parse_int_literal("abc"), None);
    }
}
