//! What constant a constant expression denotes (JLS §15.29).
//!
//! A `case` label is a constant, and a jump table is built at compile time, so the label's value
//! has to be known *now*. That makes "what is this expression's constant value" a source fact —
//! and it was the one the two backends most obviously disagreed about. The JVM side matched the
//! unary operator token run exactly and evaluated `+` / `-`; the wasm side asked only whether a
//! `MINUS` token was *present*. So `case ~5:` — a legal Java constant expression whose value is
//! `-6` — was rejected by one backend and silently compiled as `5` by the other, and `case --5:`
//! became `5` rather than the error it is.
//!
//! # Java's arithmetic, not Rust's
//!
//! Everything here is JLS semantics: `byte` / `short` / `char` promote to `int`; integer overflow
//! wraps; a shift distance is masked with `0x1f` for an `int` and `0x3f` for a `long`; `>>>` is
//! logical; and a narrowing cast keeps the low bits. Getting one of these wrong is a jump table
//! whose keys do not match the values the program computes at run time, in a class file that
//! verifies.
//!
//! # What it refuses
//!
//! Division by zero is not a constant expression, so it is reported rather than folded. So is a
//! constant declared in another file: this layer holds one file's tree, and inventing a value for
//! a declaration it cannot read is exactly the guess the rest of the crate refuses to make. An
//! enum constant gets its own refusal, because `case RED:` names an arm by identity rather than by
//! value, and a caller that saw a generic "not a constant" would look for the wrong thing.

use alloc::string::String;
use alloc::vec::Vec;

use jals_hir::DefKind;
use jals_syntax::SyntaxKind::{
    AMP, AMP_AMP, BANG, BANG_EQ, CARET, CHAR_LITERAL, EQ, EQ_EQ, FALSE_KW, FLOAT_LITERAL, GT,
    IDENT, INT_LITERAL, LSHIFT, LT, LT_EQ, MINUS, PERCENT, PIPE, PIPE_PIPE, PLUS, SLASH, STAR,
    STRING_LITERAL, TRUE_KW,
};
use jals_syntax::ast::{self, AstNode as _};

use super::literal::{Literal, Width};
use super::{FactError, Facts, Result};

/// A constant expression's value, at the width Java gives it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConstValue {
    /// `byte`, `short`, and `int` — all already promoted to `int`.
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Bool(bool),
    /// A `char`. Numerically an `int`, but `'a' + "b"` is `"ab"` and not `"97b"`, so what the
    /// source wrote has to survive as far as concatenation.
    Char(char),
    Text(String),
}

/// The constant a `case` label names.
///
/// Only two of the seven: a `switch` selector is a `char` / `byte` / `short` / `int` / `String` /
/// enum / pattern (JLS §14.11), so a `long` or a `boolean` is legal *inside* the expression and
/// never its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaseKey {
    Int(i32),
    Text(String),
}

impl CaseKey {
    /// The integral key, or `None` for a `String` one.
    pub(crate) const fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(value) => Some(*value),
            Self::Text(_) => None,
        }
    }
}

/// How much work one constant expression may cost before it is reported instead.
const BUDGET: u32 = 10_000;
/// How deep a chain of constant declarations may run.
const DEPTH: usize = 64;

impl Facts<'_> {
    /// The constant `expr` denotes.
    pub(crate) fn constant(self, expr: &ast::Expr) -> Result<ConstValue> {
        Const {
            facts: self,
            active: Vec::new(),
            budget: BUDGET,
        }
        .eval(expr)
    }

    /// The constant a `case` label names, narrowed to what a `switch` can dispatch on.
    pub(crate) fn case_key(self, expr: &ast::Expr) -> Result<CaseKey> {
        match self.constant(expr)? {
            ConstValue::Int(value) => Ok(CaseKey::Int(value)),
            ConstValue::Char(value) => Ok(CaseKey::Int(value as i32)),
            ConstValue::Text(value) => Ok(CaseKey::Text(value)),
            ConstValue::Long(_) => Err(FactError::Unsupported("a `case` outside an `int`")),
            ConstValue::Bool(_) => Err(FactError::Unsupported("a `boolean` `case`")),
            ConstValue::Float(_) | ConstValue::Double(_) => {
                Err(FactError::Unsupported("a floating-point `case`"))
            }
        }
    }
}

/// One constant expression being evaluated.
struct Const<'a> {
    facts: Facts<'a>,
    /// The constant declarations on the current path, so `A = B; B = A;` terminates rather than
    /// recursing until the stack runs out.
    active: Vec<usize>,
    budget: u32,
}

impl Const<'_> {
    fn eval(&mut self, expr: &ast::Expr) -> Result<ConstValue> {
        self.budget = self
            .budget
            .checked_sub(1)
            .ok_or(FactError::Unsupported("a constant expression this large"))?;
        match expr {
            ast::Expr::Paren(paren) => {
                let inner = paren
                    .expr()
                    .ok_or(FactError::Unsupported("a parenthesis with no expression"))?;
                self.eval(&inner)
            }
            ast::Expr::Literal(literal) => Self::literal(literal),
            ast::Expr::Unary(unary) => self.unary(unary),
            ast::Expr::Binary(binary) => self.binary(binary),
            ast::Expr::Ternary(ternary) => self.ternary(ternary),
            ast::Expr::Cast(cast) => self.cast(cast),
            ast::Expr::NameRef(_) | ast::Expr::FieldAccess(_) => self.named(expr),
            _ => Err(FactError::Unsupported("a non-literal `case`")),
        }
    }

    fn literal(node: &ast::Literal) -> Result<ConstValue> {
        let token = node
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| !token.kind().is_trivia())
            .ok_or(FactError::Unsupported("a literal with no value"))?;
        let raw = token.text();
        match token.kind() {
            INT_LITERAL => {
                let (value, width) = Literal::integer(raw)?;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "an unsuffixed literal is an `int`, and `0x8000_0000` is one whose \
                              value is negative — the source spells the bit pattern"
                )]
                Ok(match width {
                    Width::Long => ConstValue::Long(value),
                    // A decimal literal wider than an `int` is only legal directly under a unary
                    // `-`, where the wrapping conversion below is the value the source meant. That
                    // is a *checking* question, and this crate does not check.
                    Width::Int => ConstValue::Int(value as i32),
                })
            }
            FLOAT_LITERAL => {
                let (value, is_float) = Literal::floating(raw)?;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "an `f` suffix makes the literal a `float`, which is that narrowing"
                )]
                Ok(if is_float {
                    ConstValue::Float(value as f32)
                } else {
                    ConstValue::Double(value)
                })
            }
            CHAR_LITERAL => Literal::text(raw)?
                .chars()
                .next()
                .map(ConstValue::Char)
                .ok_or(FactError::Unsupported("an empty character literal")),
            STRING_LITERAL => Ok(ConstValue::Text(Literal::text(raw)?)),
            TRUE_KW => Ok(ConstValue::Bool(true)),
            FALSE_KW => Ok(ConstValue::Bool(false)),
            _ => Err(FactError::Unsupported("a `case` of this literal kind")),
        }
    }

    fn unary(&mut self, node: &ast::UnaryExpr) -> Result<ConstValue> {
        use jals_syntax::SyntaxKind::TILDE;
        let operand = node
            .operand()
            .ok_or(FactError::Unsupported("a unary with no operand"))?;
        let value = self.eval(&operand)?;
        // The token *run*, matched exactly. `--` is its own `MINUS_MINUS` kind, so a rule that
        // merely asks whether a `MINUS` is present reads `--5` as a negation — which is how one
        // backend compiled `case --5:` as `5`.
        match Facts::operator(node.syntax()).as_slice() {
            [PLUS] => Ok(Self::promote(value)),
            [MINUS] => match Self::promote(value) {
                ConstValue::Int(v) => Ok(ConstValue::Int(v.wrapping_neg())),
                ConstValue::Long(v) => Ok(ConstValue::Long(v.wrapping_neg())),
                ConstValue::Float(v) => Ok(ConstValue::Float(-v)),
                ConstValue::Double(v) => Ok(ConstValue::Double(-v)),
                _ => Err(FactError::Unsupported("a `-` on a non-numeric constant")),
            },
            [TILDE] => match Self::promote(value) {
                ConstValue::Int(v) => Ok(ConstValue::Int(!v)),
                ConstValue::Long(v) => Ok(ConstValue::Long(!v)),
                _ => Err(FactError::Unsupported("a `~` on a non-integral constant")),
            },
            [BANG] => match value {
                ConstValue::Bool(v) => Ok(ConstValue::Bool(!v)),
                _ => Err(FactError::Unsupported("a `!` on a non-boolean constant")),
            },
            _ => Err(FactError::Unsupported("a `case` this cannot evaluate")),
        }
    }

    fn ternary(&mut self, node: &ast::TernaryExpr) -> Result<ConstValue> {
        let mut parts = node.parts();
        let condition = parts
            .next()
            .ok_or(FactError::Unsupported("a `?:` with no condition"))?;
        let then = parts
            .next()
            .ok_or(FactError::Unsupported("a `?:` with no branches"))?;
        let otherwise = parts
            .next()
            .ok_or(FactError::Unsupported("a `?:` with one branch"))?;
        let ConstValue::Bool(taken) = self.eval(&condition)? else {
            return Err(FactError::Unsupported("a `?:` on a non-boolean constant"));
        };
        // Both branches must be constant expressions for the whole to be one (§15.29), so both are
        // evaluated even though only one is kept.
        let (a, b) = (self.eval(&then)?, self.eval(&otherwise)?);
        Ok(if taken { a } else { b })
    }

    fn cast(&mut self, node: &ast::CastExpr) -> Result<ConstValue> {
        let ty = node
            .ty()
            .ok_or(FactError::Unsupported("a cast with no type"))?;
        let inner = node
            .expr()
            .ok_or(FactError::Unsupported("a cast with no operand"))?;
        let value = self.eval(&inner)?;
        // Only a primitive cast, or one to `String`, keeps an expression constant (§15.29).
        let Some(primitive) = Facts::primitive_of(&ty) else {
            return match value {
                ConstValue::Text(_) if Self::ends_with_string(&ty) => Ok(value),
                _ => Err(FactError::Unsupported("a cast that is no constant")),
            };
        };
        Self::convert(value, primitive)
    }
}

impl Const<'_> {
    /// Unary numeric promotion (§5.6.1): everything narrower than an `int` becomes one.
    fn promote(value: ConstValue) -> ConstValue {
        match value {
            ConstValue::Char(c) => ConstValue::Int(c as i32),
            other => other,
        }
    }

    /// A narrowing / widening primitive conversion (§5.1.2–§5.1.3), which is what a cast applies.
    ///
    /// Rust's `as` on a float is saturating and maps NaN to zero, which is exactly what §5.1.3
    /// specifies — the same fact the wasm backend records about `trunc_sat`.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "every conversion here is a Java narrowing cast, whose whole meaning is to truncate"
    )]
    fn convert(value: ConstValue, target: jals_hir::Primitive) -> Result<ConstValue> {
        use jals_hir::Primitive;
        let as_long = match &value {
            ConstValue::Int(v) => i64::from(*v),
            ConstValue::Long(v) => *v,
            ConstValue::Char(c) => *c as i64,
            ConstValue::Float(v) => *v as i64,
            ConstValue::Double(v) => *v as i64,
            ConstValue::Bool(_) | ConstValue::Text(_) => {
                return match (target, value) {
                    (Primitive::Boolean, ConstValue::Bool(v)) => Ok(ConstValue::Bool(v)),
                    _ => Err(FactError::Unsupported("a cast of a non-numeric constant")),
                };
            }
        };
        let as_double = match &value {
            ConstValue::Int(v) => f64::from(*v),
            ConstValue::Long(v) => *v as f64,
            ConstValue::Char(c) => f64::from(u32::from(*c)),
            ConstValue::Float(v) => f64::from(*v),
            ConstValue::Double(v) => *v,
            ConstValue::Bool(_) | ConstValue::Text(_) => unreachable!("handled above"),
        };
        Ok(match target {
            Primitive::Byte => ConstValue::Int(i32::from(as_long as i8)),
            Primitive::Short => ConstValue::Int(i32::from(as_long as i16)),
            Primitive::Char => {
                ConstValue::Char(char::from_u32(u32::from(as_long as u16)).unwrap_or('\u{0}'))
            }
            Primitive::Int => ConstValue::Int(as_long as i32),
            Primitive::Long => ConstValue::Long(as_long),
            Primitive::Float => ConstValue::Float(as_double as f32),
            Primitive::Double => ConstValue::Double(as_double),
            Primitive::Boolean => {
                return Err(FactError::Unsupported("a cast of a number to `boolean`"));
            }
        })
    }
}
/// The two numeric kinds a binary operator runs at, after binary numeric promotion (§5.6.2).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Promoted {
    Int,
    Long,
    Float,
    Double,
}

impl Const<'_> {
    fn binary(&mut self, node: &ast::BinaryExpr) -> Result<ConstValue> {
        let operator = Facts::operator(node.syntax());
        let left = node
            .lhs()
            .ok_or(FactError::Unsupported("a binary with no left operand"))?;
        let right = node
            .rhs()
            .ok_or(FactError::Unsupported("a binary with no right operand"))?;
        let (a, b) = (self.eval(&left)?, self.eval(&right)?);

        // A `+` with a `String` operand is concatenation, and it is the one operator that reads a
        // `char` as a character rather than as its code.
        if operator.as_slice() == [PLUS]
            && matches!(a, ConstValue::Text(_)) | matches!(b, ConstValue::Text(_))
        {
            return Self::concat(&a, &b);
        }
        if let (ConstValue::Bool(x), ConstValue::Bool(y)) = (&a, &b) {
            let (x, y) = (*x, *y);
            return match operator.as_slice() {
                // `&`, `|`, and `^` are also the boolean operators, without short-circuiting.
                [AMP | AMP_AMP] => Ok(ConstValue::Bool(x && y)),
                [PIPE | PIPE_PIPE] => Ok(ConstValue::Bool(x || y)),
                // `^` and `!=` are the same operation on two booleans.
                [CARET | BANG_EQ] => Ok(ConstValue::Bool(x != y)),
                [EQ_EQ] => Ok(ConstValue::Bool(x == y)),
                _ => Err(FactError::Unsupported("this operator on a `boolean`")),
            };
        }
        if matches!(a, ConstValue::Text(_)) || matches!(b, ConstValue::Text(_)) {
            // `==` on two `String` constants answers by interning rather than by value, which is a
            // fact about the constant pool and not about the source.
            return Err(FactError::Unsupported("this operator on a `String`"));
        }

        // A shift promotes each side on its own (§5.6.1 twice, not §5.6.2 once): the result has the
        // *left* operand's promoted type, and the distance is masked.
        match operator.as_slice() {
            [LSHIFT] | [GT, GT] | [GT, GT, GT] => {
                return Self::shift(&Self::promote(a), &Self::promote(b), operator.as_slice());
            }
            _ => {}
        }

        let (x, y, kind) = Self::numeric(Self::promote(a), Self::promote(b))?;
        Self::arithmetic(&x, &y, kind, operator.as_slice())
    }

    /// The constant a name refers to — a `final` variable initialised with a constant expression,
    /// which JLS §4.12.4 calls a *constant variable*.
    fn named(&mut self, expr: &ast::Expr) -> Result<ConstValue> {
        let node = expr.syntax();
        let index = self.facts.index();

        // A member access reaches its declaration through inference; a bare name through the
        // file's own resolution.
        let declaration = if let ast::Expr::FieldAccess(_) = expr {
            let member = self
                .facts
                .typed()
                .field_target_of(Facts::span(node))
                .ok_or_else(|| FactError::Unresolved(Self::text_of(node)))?;
            let info = index.member(member);
            if info.kind == DefKind::EnumConstant {
                return Err(FactError::Unsupported("a `case` naming an enum constant"));
            }
            if info.file != self.facts.file() {
                return Err(FactError::Unsupported(
                    "a constant declared in another file",
                ));
            }
            info.name_range.start
        } else {
            let id = self
                .facts
                .def_at(node)
                .ok_or_else(|| FactError::Unresolved(Self::text_of(node)))?;
            let def = self.facts.typed().analysis().def(id);
            match def.kind {
                DefKind::EnumConstant => {
                    return Err(FactError::Unsupported("a `case` naming an enum constant"));
                }
                DefKind::Field | DefKind::Local => {}
                _ => return Err(FactError::Unsupported("a non-literal `case`")),
            }
            def.name_range.start
        };

        if self.active.contains(&declaration) {
            return Err(FactError::Unsupported("a constant that refers to itself"));
        }
        if self.active.len() >= DEPTH {
            return Err(FactError::Unsupported("a constant chain this deep"));
        }

        let decl = self
            .facts
            .declaration_of(declaration)
            .ok_or(FactError::Unsupported("a non-literal `case`"))?;
        if !Facts::is_constant_declaration(&decl) {
            // Not `final`, so not a constant variable — the value may change before the switch runs.
            return Err(FactError::Unsupported("a non-literal `case`"));
        }
        let initialiser = Facts::declarator_initialiser(&decl, declaration)
            .ok_or(FactError::Unsupported("a non-literal `case`"))?;

        self.active.push(declaration);
        let value = self.eval(&initialiser);
        self.active.pop();
        value
    }
}

impl Const<'_> {
    /// Binary numeric promotion (§5.6.2), with both operands already unary-promoted.
    fn numeric(a: ConstValue, b: ConstValue) -> Result<(ConstValue, ConstValue, Promoted)> {
        let kind = match (&a, &b) {
            (ConstValue::Double(_), _) | (_, ConstValue::Double(_)) => Promoted::Double,
            (ConstValue::Float(_), _) | (_, ConstValue::Float(_)) => Promoted::Float,
            (ConstValue::Long(_), _) | (_, ConstValue::Long(_)) => Promoted::Long,
            (ConstValue::Int(_), ConstValue::Int(_)) => Promoted::Int,
            _ => {
                return Err(FactError::Unsupported(
                    "an operator on a non-numeric constant",
                ));
            }
        };
        Ok((a, b, kind))
    }

    /// `a op b` at `kind`, with Java's wrapping and its refusal to fold a division by zero.
    fn arithmetic(
        a: &ConstValue,
        b: &ConstValue,
        kind: Promoted,
        operator: &[jals_syntax::SyntaxKind],
    ) -> Result<ConstValue> {
        let unsupported = || FactError::Unsupported("a `case` this cannot evaluate");
        let zero = || FactError::Unsupported("a constant division by zero");
        match kind {
            Promoted::Int | Promoted::Long => {
                let (Some(x), Some(y)) = (Self::as_i64(a), Self::as_i64(b)) else {
                    return Err(unsupported());
                };
                let long = kind == Promoted::Long;
                if let Some(compared) = Self::compare(x, y, operator) {
                    return Ok(ConstValue::Bool(compared));
                }
                let value = match operator {
                    [PLUS] => x.wrapping_add(y),
                    [MINUS] => x.wrapping_sub(y),
                    [STAR] => x.wrapping_mul(y),
                    [SLASH | PERCENT] if y == 0 => return Err(zero()),
                    [SLASH] => x.wrapping_div(y),
                    [PERCENT] => x.wrapping_rem(y),
                    [AMP] => x & y,
                    [PIPE] => x | y,
                    [CARET] => x ^ y,
                    _ => return Err(unsupported()),
                };
                Ok(if long {
                    ConstValue::Long(value)
                } else {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "an `int` operation wraps at 32 bits, which is the truncation"
                    )]
                    ConstValue::Int(value as i32)
                })
            }
            Promoted::Float | Promoted::Double => {
                let (Some(x), Some(y)) = (Self::as_f64(a), Self::as_f64(b)) else {
                    return Err(unsupported());
                };
                if let Some(compared) = Self::compare(x, y, operator) {
                    return Ok(ConstValue::Bool(compared));
                }
                let value = match operator {
                    [PLUS] => x + y,
                    [MINUS] => x - y,
                    [STAR] => x * y,
                    [SLASH] => x / y,
                    // `%` on a float needs `fmod` from `compiler_builtins`, and no `case` label can
                    // reach one — so it is refused rather than linked for.
                    _ => return Err(unsupported()),
                };
                Ok(if kind == Promoted::Float {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "a `float` operation runs at 32-bit width"
                    )]
                    ConstValue::Float(value as f32)
                } else {
                    ConstValue::Double(value)
                })
            }
        }
    }

    /// A shift, whose result takes the *left* operand's width and whose distance is masked (§15.19).
    fn shift(
        a: &ConstValue,
        b: &ConstValue,
        operator: &[jals_syntax::SyntaxKind],
    ) -> Result<ConstValue> {
        let Some(distance) = Self::as_i64(b) else {
            return Err(FactError::Unsupported("a shift by a non-integral constant"));
        };
        match *a {
            ConstValue::Int(x) => {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "§15.19 masks an `int` shift distance to five bits"
                )]
                let d = (distance as u32) & 0x1f;
                Ok(ConstValue::Int(match operator {
                    [LSHIFT] => x.wrapping_shl(d),
                    [GT, GT] => x.wrapping_shr(d),
                    #[expect(
                        clippy::cast_possible_wrap,
                        clippy::cast_sign_loss,
                        reason = "`>>>` is the logical shift, which is the unsigned one"
                    )]
                    [GT, GT, GT] => ((x as u32) >> d) as i32,
                    _ => return Err(FactError::Unsupported("this shift operator")),
                }))
            }
            ConstValue::Long(x) => {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "§15.19 masks a `long` shift distance to six bits"
                )]
                let d = (distance as u32) & 0x3f;
                Ok(ConstValue::Long(match operator {
                    [LSHIFT] => x.wrapping_shl(d),
                    [GT, GT] => x.wrapping_shr(d),
                    #[expect(
                        clippy::cast_possible_wrap,
                        clippy::cast_sign_loss,
                        reason = "`>>>` is the logical shift, which is the unsigned one"
                    )]
                    [GT, GT, GT] => ((x as u64) >> d) as i64,
                    _ => return Err(FactError::Unsupported("this shift operator")),
                }))
            }
            _ => Err(FactError::Unsupported("a shift of a non-integral constant")),
        }
    }

    /// A node's source text, for an error that names what did not resolve.
    fn text_of(node: &jals_syntax::SyntaxNode) -> String {
        use core::fmt::Write as _;
        let mut out = String::new();
        let _ = write!(out, "{}", node.text());
        out
    }

    /// Whether a cast names `String`, which is the one reference cast §15.29 keeps constant.
    fn ends_with_string(ty: &ast::Type) -> bool {
        ty.syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == IDENT)
            .last()
            .is_some_and(|token| token.text() == "String")
    }

    /// Java's string conversion of a constant, for a `+` with a `String` operand.
    fn concat(a: &ConstValue, b: &ConstValue) -> Result<ConstValue> {
        let mut out = Self::render(a)?;
        out.push_str(&Self::render(b)?);
        Ok(ConstValue::Text(out))
    }

    fn render(value: &ConstValue) -> Result<String> {
        use core::fmt::Write as _;
        let mut out = String::new();
        match value {
            ConstValue::Text(text) => out.push_str(text),
            // A `char` renders as its character, which is why the variant exists: `'a' + "b"` is
            // `"ab"`, not `"97b"`.
            ConstValue::Char(c) => out.push(*c),
            ConstValue::Int(v) => {
                let _ = write!(out, "{v}");
            }
            ConstValue::Long(v) => {
                let _ = write!(out, "{v}");
            }
            ConstValue::Bool(v) => {
                let _ = write!(out, "{v}");
            }
            // Java's `Double.toString` is not Rust's `Display` — `1.0` prints as `1` here — and a
            // silently wrong string constant is the defect class this module exists to remove.
            ConstValue::Float(_) | ConstValue::Double(_) => {
                return Err(FactError::Unsupported(
                    "a floating-point constant in a concatenation",
                ));
            }
        }
        Ok(out)
    }

    /// A constant's integral value, or `None` when it has none.
    fn as_i64(value: &ConstValue) -> Option<i64> {
        Some(match value {
            ConstValue::Int(v) => i64::from(*v),
            ConstValue::Long(v) => *v,
            ConstValue::Char(c) => *c as i64,
            ConstValue::Float(_)
            | ConstValue::Double(_)
            | ConstValue::Bool(_)
            | ConstValue::Text(_) => {
                return None;
            }
        })
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "a `long` widened to `double` loses precision in Java too (§5.1.2)"
    )]
    /// A constant's floating-point value, or `None` when it has none.
    fn as_f64(value: &ConstValue) -> Option<f64> {
        Some(match value {
            ConstValue::Int(v) => f64::from(*v),
            ConstValue::Long(v) => *v as f64,
            ConstValue::Float(v) => f64::from(*v),
            ConstValue::Double(v) => *v,
            ConstValue::Char(c) => f64::from(u32::from(*c)),
            ConstValue::Bool(_) | ConstValue::Text(_) => return None,
        })
    }

    /// A relational or equality operator's answer, at whichever width the operands were promoted to.
    ///
    /// One table for both widths: the arms differ only in the type they compare, and two copies of an
    /// operator table is what this module exists to stop. On a floating-point operand `==` is exact —
    /// Java compares two constants bit for bit, not within a tolerance.
    fn compare<T: PartialOrd + Copy>(
        x: T,
        y: T,
        operator: &[jals_syntax::SyntaxKind],
    ) -> Option<bool> {
        Some(match operator {
            [LT] => x < y,
            [GT] => x > y,
            [LT_EQ] => x <= y,
            // `>=` is two tokens, so that `List<List<T>>` still closes as two `>`.
            [GT, EQ] => x >= y,
            [EQ_EQ] => x == y,
            [BANG_EQ] => x != y,
            _ => return None,
        })
    }
}

impl Facts<'_> {
    /// The declaration whose declaring name starts at `name_start`.
    pub(crate) fn declaration_of(self, name_start: usize) -> Option<jals_syntax::SyntaxNode> {
        use jals_syntax::SyntaxKind::{FIELD_DECL, LOCAL_VAR_DECL};
        let offset = u32::try_from(name_start).ok()?;
        self.root()
            .descendants()
            .filter(|node| matches!(node.kind(), FIELD_DECL | LOCAL_VAR_DECL))
            .filter(|node| node.text_range().contains(offset.into()))
            .last()
    }

    /// Whether a declaration makes its variables *constant* — `final`, and for a field also
    /// `static`.
    ///
    /// Read off the CST rather than the index because `MemberModifiers` records only `is_static`
    /// and `is_private`: `final` changes no instruction, so nothing upstream keeps it. A field
    /// declared directly in an interface is implicitly `public static final` (§9.3), which is what
    /// makes `interface Flags { int A = 1; }` a constant without a modifier token.
    pub(crate) fn is_constant_declaration(decl: &jals_syntax::SyntaxNode) -> bool {
        use jals_syntax::SyntaxKind::{
            ANNOTATION_TYPE_DECL, FIELD_DECL, FINAL_KW, INTERFACE_DECL, STATIC_KW,
        };
        let modifiers: Vec<_> = decl
            .children()
            .flat_map(|child| child.children_with_tokens())
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .map(|token| token.kind())
            .collect();
        let in_interface = decl
            .ancestors()
            .any(|ancestor| matches!(ancestor.kind(), INTERFACE_DECL | ANNOTATION_TYPE_DECL));
        if decl.kind() == FIELD_DECL && !in_interface {
            return modifiers.contains(&FINAL_KW) && modifiers.contains(&STATIC_KW);
        }
        in_interface || modifiers.contains(&FINAL_KW)
    }
}
