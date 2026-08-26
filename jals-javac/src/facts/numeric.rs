//! Numeric promotion — JLS §5.6.
//!
//! The rule both lowerings apply before any arithmetic: which type an operand computes as, and
//! which one type two operands meet in. It is a statement about the *source program* and names no
//! instruction, so it belongs here — and it was written three times.
//!
//! Two of the copies were the backends', identical down to the JLS citations in their doc comments
//! and differing only in the name of the enum they matched on. The third was inside this very
//! module: [`constant`](super::constant)'s `case`-label evaluator carried its own `Promoted` and its
//! own two rules, so `facts` held a private duplicate of the fact it exists to single-source.
//!
//! What each backend keeps is the *representation* the promoted type has on its target — the
//! verification type on the JVM's operand stack, the `ValType` in a wasm local. Those are answers
//! about a target, so they stay with the target, as extension traits over this type.

use jals_hir::Primitive;

/// A primitive type as a *conversion* names it.
///
/// Deliberately narrower than any target's stack vocabulary: `byte`, `char`, and `short` all
/// compute as `int` (JVMS §2.11.1, and `i32` in wasm), so a conversion between two of them changes
/// the value without changing the representation at all. A pair of *representations* could not say
/// which of `i2b` / `i2c` / `i2s` was meant, because both sides would read the same.
///
/// `boolean` is not one of these. It is not a numeric type (JLS §4.2), so nothing here promotes it
/// and [`of`](Self::of) refuses it. That a `boolean` happens to share `int`'s representation on
/// both targets is a fact about each target, not about the language, and each backend says so where
/// it decides its own layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numeric {
    /// `byte`
    Byte,
    /// `short`
    Short,
    /// `char`
    Char,
    /// `int`
    Int,
    /// `long`
    Long,
    /// `float`
    Float,
    /// `double`
    Double,
}

impl Numeric {
    /// The numeric type a primitive is, or `None` for `boolean`.
    pub(crate) const fn of(primitive: Primitive) -> Option<Self> {
        Some(match primitive {
            Primitive::Byte => Self::Byte,
            Primitive::Short => Self::Short,
            Primitive::Char => Self::Char,
            Primitive::Int => Self::Int,
            Primitive::Long => Self::Long,
            Primitive::Float => Self::Float,
            Primitive::Double => Self::Double,
            // Not a numeric type (JLS §4.2). Whether it shares `int`'s representation is a
            // question about a target, and each backend answers it for itself.
            Primitive::Boolean => return None,
        })
    }

    /// Binary numeric promotion (JLS §5.6.2): the one type both operands are converted to.
    pub(crate) const fn promote(left: Self, right: Self) -> Self {
        match (left, right) {
            (Self::Double, _) | (_, Self::Double) => Self::Double,
            (Self::Float, _) | (_, Self::Float) => Self::Float,
            (Self::Long, _) | (_, Self::Long) => Self::Long,
            // Everything narrower than `long` computes as `int`.
            _ => Self::Int,
        }
    }

    /// Unary numeric promotion (JLS §5.6.1): everything narrower than `int` becomes `int`.
    pub(crate) const fn promote_one(numeric: Self) -> Self {
        match numeric {
            Self::Byte | Self::Short | Self::Char | Self::Int => Self::Int,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use jals_hir::Primitive;

    use super::Numeric;

    /// Every ordered pair of numeric types meets where JLS §5.6.2 says, and the rule is symmetric.
    ///
    /// Written as the *table* rather than as examples because the three copies this replaced were
    /// each a four-arm `match` whose arms overlap: `(Double, _) | (_, Double)` before
    /// `(Float, _) | (_, Float)` and so on, so an arm in the wrong order is wrong for exactly one
    /// pair and right for the other twenty-three. Enumerating all forty-nine is what makes an
    /// ordering mistake impossible to write, and asserting `promote(a, b) == promote(b, a)` is what
    /// pins the symmetry the overlapping arms are there to give.
    #[test]
    fn binary_promotion_is_the_widest_of_the_two_and_never_narrower_than_int() {
        use Numeric::{Byte, Char, Double, Float, Int, Long, Short};
        const ALL: [Numeric; 7] = [Byte, Short, Char, Int, Long, Float, Double];

        for left in ALL {
            for right in ALL {
                let met = Numeric::promote(left, right);
                let expected = if left == Double || right == Double {
                    Double
                } else if left == Float || right == Float {
                    Float
                } else if left == Long || right == Long {
                    Long
                } else {
                    Int
                };
                assert_eq!(met, expected, "promote({left:?}, {right:?})");
                assert_eq!(
                    met,
                    Numeric::promote(right, left),
                    "promotion is symmetric: {left:?} / {right:?}"
                );
            }
        }
    }

    /// Unary promotion widens the three sub-`int` types and leaves everything else alone.
    ///
    /// `char` is the one that catches a copy written from memory: it is unsigned and narrower than
    /// `int`, so it promotes like `byte` and `short` even though it converts back differently.
    #[test]
    fn unary_promotion_widens_exactly_byte_short_char_and_int() {
        use Numeric::{Byte, Char, Double, Float, Int, Long, Short};
        let promoted: Vec<Numeric> = [Byte, Short, Char, Int, Long, Float, Double]
            .into_iter()
            .map(Numeric::promote_one)
            .collect();
        assert_eq!(promoted, [Int, Int, Int, Int, Long, Float, Double]);
    }

    /// Every primitive maps to itself, and `boolean` maps to nothing.
    ///
    /// `boolean` is not a numeric type (JLS §4.2). Both targets happen to give it `int`'s
    /// representation, and one backend folded that into its own `of` — which is a statement about
    /// the target wearing the shape of a statement about the language. The refusal is here so that
    /// each backend has to write its representation choice down where it makes it.
    #[test]
    fn boolean_is_not_a_numeric_type() {
        use Numeric::{Byte, Char, Double, Float, Int, Long, Short};
        let mapped: Vec<Option<Numeric>> = [
            Primitive::Byte,
            Primitive::Short,
            Primitive::Char,
            Primitive::Int,
            Primitive::Long,
            Primitive::Float,
            Primitive::Double,
            Primitive::Boolean,
        ]
        .into_iter()
        .map(Numeric::of)
        .collect();
        assert_eq!(
            mapped,
            [
                Some(Byte),
                Some(Short),
                Some(Char),
                Some(Int),
                Some(Long),
                Some(Float),
                Some(Double),
                None,
            ]
        );
    }
}
