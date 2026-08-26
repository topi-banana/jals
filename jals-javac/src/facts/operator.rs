//! Which operator a token run spells.
//!
//! [`Facts::operator`](super::Facts::operator) hands back a node's own tokens as a *run*, because
//! the lexer never joins a `>` to what follows — that is what lets `List<List<T>>` close as two of
//! them, and it is why `>>` is `[GT, GT]` and `>=` is `[GT, EQ]`. Reading that run was the part
//! both backends then wrote for themselves: eight decoders, and the sentence explaining the `>`
//! split copied beside four of them.
//!
//! The decode is a statement about the source. What each backend does with the answer is not: the
//! JVM splits arithmetic from comparison (`BinOp` plus `Compare`, because `iadd` and `if_icmplt`
//! are different shapes of instruction) while wasm fuses them (`NumOp`, because `i32.add` and
//! `i32.lt_s` are the same shape). Both spellings map from this one vocabulary instead of deriving
//! it, so an operator added to the grammar is read in one place and projected in two.

use jals_syntax::{SyntaxKind, SyntaxNode};

use super::Facts;

/// A binary operator, as the source spells it.
///
/// Nineteen, not seventeen: `&&` and `||` are here even though neither backend lowers them as an
/// operator over two values — the right operand may not run at all — and so is `instanceof`, whose
/// right side is a type or a pattern rather than an expression. A decoder that answered `None` for
/// the three would push the run-reading rule back out to every caller that has to recognise them
/// *before* it evaluates operands, which is exactly the split that produced the copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operator {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `&`
    And,
    /// `|`
    Or,
    /// `^`
    Xor,
    /// `<<`
    Shl,
    /// `>>`, the arithmetic shift that keeps the sign bit.
    Shr,
    /// `>>>`, the logical shift that does not.
    Ushr,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&&`, which evaluates its right operand only if the left is true.
    AndAnd,
    /// `||`, which evaluates its right operand only if the left is false.
    OrOr,
    /// `instanceof`, whose right side is a type or a pattern and not a value.
    InstanceOf,
}

/// A unary operator, as the source spells it.
///
/// `++` and `--` carry no position: the *token* is the same whether it was written before or after
/// its operand, and which one it was is the node's kind rather than the operator's. Both backends
/// decoded the two forms separately and reached the same two tokens from each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unary {
    /// `+`, which is unary numeric promotion and nothing else.
    Plus,
    /// `-`
    Minus,
    /// `!`
    Not,
    /// `~`
    BitNot,
    /// `++`
    Increment,
    /// `--`
    Decrement,
}

impl Operator {
    /// The binary operator `node` applies, if its token run spells one.
    pub(crate) fn binary(node: &SyntaxNode) -> Option<Self> {
        use SyntaxKind::{
            AMP, AMP_AMP, BANG_EQ, CARET, EQ, EQ_EQ, GT, INSTANCEOF_KW, LSHIFT, LT, LT_EQ, MINUS,
            PERCENT, PIPE, PIPE_PIPE, PLUS, SLASH, STAR,
        };
        Some(match Facts::operator(node).as_slice() {
            [PLUS] => Self::Add,
            [MINUS] => Self::Sub,
            [STAR] => Self::Mul,
            [SLASH] => Self::Div,
            [PERCENT] => Self::Rem,
            [AMP] => Self::And,
            [PIPE] => Self::Or,
            [CARET] => Self::Xor,
            [LSHIFT] => Self::Shl,
            // `>>` and `>>>` are separate `>` tokens: the lexer never joins a `>` to what follows,
            // so that `List<List<T>>` still closes as two of them. This is the one place that
            // sentence has to be true, and the one place it is written.
            [GT, GT] => Self::Shr,
            [GT, GT, GT] => Self::Ushr,
            [EQ_EQ] => Self::Eq,
            [BANG_EQ] => Self::Ne,
            [LT] => Self::Lt,
            [LT_EQ] => Self::Le,
            [GT] => Self::Gt,
            [GT, EQ] => Self::Ge,
            [AMP_AMP] => Self::AndAnd,
            [PIPE_PIPE] => Self::OrOr,
            // The keyword leads; a pattern's own tokens follow it inside this node.
            [INSTANCEOF_KW, ..] => Self::InstanceOf,
            _ => return None,
        })
    }

    /// The operator a compound assignment applies, if its token run spells one. `=` is not one.
    ///
    /// Never a comparison or a short-circuit: Java has no `===` and no `&&=`, so the eleven here are
    /// the whole set.
    pub(crate) fn compound(node: &SyntaxNode) -> Option<Self> {
        use SyntaxKind::{
            AMP_EQ, CARET_EQ, EQ, GT, LSHIFT_EQ, MINUS_EQ, PERCENT_EQ, PIPE_EQ, PLUS_EQ, SLASH_EQ,
            STAR_EQ,
        };
        Some(match Facts::operator(node).as_slice() {
            [PLUS_EQ] => Self::Add,
            [MINUS_EQ] => Self::Sub,
            [STAR_EQ] => Self::Mul,
            [SLASH_EQ] => Self::Div,
            [PERCENT_EQ] => Self::Rem,
            [AMP_EQ] => Self::And,
            [PIPE_EQ] => Self::Or,
            [CARET_EQ] => Self::Xor,
            [LSHIFT_EQ] => Self::Shl,
            // Split for the same reason `>>` is, and the reason applies to `>>=` too.
            [GT, GT, EQ] => Self::Shr,
            [GT, GT, GT, EQ] => Self::Ushr,
            _ => return None,
        })
    }
}

impl Unary {
    /// The unary operator `node` applies, if its token run spells one.
    ///
    /// Matching the run *exactly* is what keeps `--5` from reading as a negation of `-5`: `--` is
    /// its own `MINUS_MINUS` kind, so a rule that merely asks whether a `MINUS` is present answers
    /// wrongly — which is how one backend once compiled `case --5:` as `5`.
    pub(crate) fn of(node: &SyntaxNode) -> Option<Self> {
        use SyntaxKind::{BANG, MINUS, MINUS_MINUS, PLUS, PLUS_PLUS, TILDE};
        Some(match Facts::operator(node).as_slice() {
            [PLUS] => Self::Plus,
            [MINUS] => Self::Minus,
            [BANG] => Self::Not,
            [TILDE] => Self::BitNot,
            [PLUS_PLUS] => Self::Increment,
            [MINUS_MINUS] => Self::Decrement,
            _ => return None,
        })
    }

    /// The step an increment or decrement applies, or `None` for the other four.
    ///
    /// `i8` because that is the width both targets take one in: the JVM's `iinc` carries a signed
    /// byte constant, and the wasm lowering threads the same value.
    pub(crate) const fn step(self) -> Option<i8> {
        match self {
            Self::Increment => Some(1),
            Self::Decrement => Some(-1),
            Self::Plus | Self::Minus | Self::Not | Self::BitNot => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    use jals_exec::block_on_inline;
    use jals_syntax::ast::{self, AstNode as _};

    use super::{Operator, Unary};

    /// Every binary expression in `source`, decoded, in source order.
    fn binaries(source: &str) -> Vec<Option<Operator>> {
        block_on_inline(jals_syntax::Parse::parse(source))
            .syntax()
            .descendants()
            .filter_map(ast::BinaryExpr::cast)
            .map(|binary| Operator::binary(binary.syntax()))
            .collect()
    }

    /// `source` wrapped in a method that can hold statements.
    fn program(body: &str) -> String {
        format!("class C {{ void m(int a, int b, Object o) {{ {body} }} }}")
    }

    /// Every binary operator the grammar spells, decoded once.
    ///
    /// The three that are not operators over two values are here too — `&&`, `||`, and
    /// `instanceof` — because each backend has to recognise them *before* it evaluates operands,
    /// and a decoder that refused them would push the run-reading rule back out to the callers that
    /// do.
    #[test]
    fn every_binary_operator_decodes_from_its_token_run() {
        let source = program(
            "boolean t = a + b == a - b
                && a * b != a / b
                || a % b < (a & b)
                || (a | b) <= (a ^ b)
                || (a << b) > (a >> b)
                || (a >>> b) >= a
                || o instanceof String;",
        );
        let seen: Vec<Operator> = binaries(&source).into_iter().flatten().collect();
        for expected in [
            Operator::Add,
            Operator::Sub,
            Operator::Mul,
            Operator::Div,
            Operator::Rem,
            Operator::And,
            Operator::Or,
            Operator::Xor,
            Operator::Shl,
            Operator::Shr,
            Operator::Ushr,
            Operator::Eq,
            Operator::Ne,
            Operator::Lt,
            Operator::Le,
            Operator::Gt,
            Operator::Ge,
            Operator::AndAnd,
            Operator::OrOr,
            Operator::InstanceOf,
        ] {
            assert!(seen.contains(&expected), "{expected:?} was not decoded");
        }
    }

    /// `>>`, `>>>`, and `>=` are runs of `>` tokens, and each is a different operator.
    ///
    /// The lexer never joins a `>` to what follows, so that `List<List<T>>` closes as two of them.
    /// That makes the *length and shape* of the run the whole answer — and it is the rule each
    /// backend used to restate, with the sentence explaining it copied beside four separate
    /// matches. Getting `[GT, GT]` and `[GT, EQ]` the wrong way round is one arm's worth of typing
    /// and silently compiles a shift as a comparison.
    #[test]
    fn a_run_of_angle_brackets_spells_three_different_operators() {
        assert_eq!(
            binaries(&program(
                "int x = a >> b; int y = a >>> b; boolean z = a >= b;"
            )),
            [
                Some(Operator::Shr),
                Some(Operator::Ushr),
                Some(Operator::Ge)
            ]
        );
        // …and a generic type closing with two `>` is not an operator at all.
        assert_eq!(
            binaries("class C { java.util.List<java.util.List<String>> f; }"),
            []
        );
    }

    /// Every compound assignment, decoded — and `=` itself is not one of them.
    #[test]
    fn every_compound_assignment_decodes_and_plain_assignment_does_not() {
        let source = program(
            "a = b; a += b; a -= b; a *= b; a /= b; a %= b;
             a &= b; a |= b; a ^= b; a <<= b; a >>= b; a >>>= b;",
        );
        let decoded: Vec<Option<Operator>> = block_on_inline(jals_syntax::Parse::parse(&source))
            .syntax()
            .descendants()
            .filter_map(ast::AssignmentExpr::cast)
            .map(|assignment| Operator::compound(assignment.syntax()))
            .collect();
        assert_eq!(
            decoded,
            [
                None, // `=` applies no operator
                Some(Operator::Add),
                Some(Operator::Sub),
                Some(Operator::Mul),
                Some(Operator::Div),
                Some(Operator::Rem),
                Some(Operator::And),
                Some(Operator::Or),
                Some(Operator::Xor),
                Some(Operator::Shl),
                Some(Operator::Shr),
                Some(Operator::Ushr),
            ]
        );
    }

    /// The six unary operators, and the `--5` case the run-matching rule exists for.
    ///
    /// `--` is its own token kind, so `- -5` and `--5` are different programs. A decoder that asked
    /// "is a `MINUS` present" answers the same for both, which is how a `case --5:` label once
    /// folded to `5`.
    #[test]
    fn a_double_minus_is_its_own_operator_and_not_two_negations() {
        let decoded: Vec<Option<Unary>> = block_on_inline(jals_syntax::Parse::parse(&program(
            "int p = +a; int n = -a; boolean q = !(a > b); int t = ~a; ++a; --a;",
        )))
        .syntax()
        .descendants()
        .filter_map(ast::UnaryExpr::cast)
        .map(|unary| Unary::of(unary.syntax()))
        .collect();
        assert_eq!(
            decoded,
            [
                Some(Unary::Plus),
                Some(Unary::Minus),
                Some(Unary::Not),
                Some(Unary::BitNot),
                Some(Unary::Increment),
                Some(Unary::Decrement),
            ]
        );
        assert_eq!(Unary::Increment.step(), Some(1));
        assert_eq!(Unary::Decrement.step(), Some(-1));
        assert_eq!(Unary::Minus.step(), None);
    }
}
