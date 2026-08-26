//! The instruction encoder: one method per operation a lowering wants to emit.
//!
//! Named after what they do rather than after their opcodes, for the same reason the JVM assembler
//! is: the caller is a code generator reasoning about Java, not about byte values.
//!
//! Control flow here is *structured* — `block` / `loop` / `if` / `br` — which is why the wasm
//! backend lowers from the syntax tree rather than from the JVM bytecode the other backend emits.
//! Recovering `while` from a `goto` would mean a relooper; keeping the tree means the nesting is
//! already right.

use crate::facts::Numeric;
use alloc::vec::Vec;

use crate::wasm::encode::{Bytes, HeapType, ValType};

/// A binary numeric operation, resolved to an opcode by the type it applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NumOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Xor,
    /// `<<`
    Shl,
    /// `>>`, the arithmetic shift that keeps the sign bit.
    Shr,
    /// `>>>`, the logical shift that does not.
    Ushr,
}

impl NumOp {
    /// Whether this operator's right operand is a shift count.
    ///
    /// wasm and the JVM disagree here, which is a real difference and not a spelling one: `i64.shl`
    /// takes **two `i64`s** where `lshl` takes a `long` and an `int`. So a Java `long << int` needs its
    /// count *extended* on the way in, where the JVM backend needs nothing.
    pub(crate) const fn is_shift(self) -> bool {
        matches!(self, Self::Shl | Self::Shr | Self::Ushr)
    }
}

/// The representation a promoted [`Numeric`] has in a wasm local or on the stack.
///
/// An extension trait rather than an inherent method: the type states a *source* fact (JLS §5.6)
/// and lives in `crate::facts`, while a `ValType` is an answer about this target. The two used to
/// be one enum per backend, identical but for the name, each carrying its own copy of the
/// promotion rules.
pub(crate) trait NumericVal {
    /// The wasm value type this occupies.
    fn val(self) -> ValType;
}

impl NumericVal for Numeric {
    fn val(self) -> ValType {
        match self {
            Self::Long => ValType::I64,
            Self::Float => ValType::F32,
            Self::Double => ValType::F64,
            // `byte` / `short` / `char` all compute as `i32` and differ only as narrowing targets.
            Self::Byte | Self::Short | Self::Char | Self::Int => ValType::I32,
        }
    }
}

/// Builds one function body.
#[derive(Debug, Default)]
pub(crate) struct Insn {
    out: Bytes,
    /// How many structured control instructions are currently open.
    ///
    /// A `br` names a *relative* depth, so the target of a `continue` shifts every time an `if` opens
    /// between the loop header and the branch. Nothing in the source says how deep that is; only the
    /// emitter knows, so it counts. A lowering records this at the point it opens a loop and takes the
    /// difference at the branch.
    depth: u32,
}

impl Insn {
    pub(crate) const fn new() -> Self {
        Self {
            out: Bytes::new(),
            depth: 0,
        }
    }

    /// How many structured control instructions are open here.
    pub(crate) const fn depth(&self) -> u32 {
        self.depth
    }

    pub(crate) fn into_body(self) -> Vec<u8> {
        self.out.into_vec()
    }

    // --- control ------------------------------------------------------------

    /// `block` with no result. Closed by [`end`](Self::end); `br 0` inside jumps *past* it.
    pub(crate) fn block(&mut self) -> &mut Self {
        self.out.byte(0x02).byte(0x40);
        self.depth += 1;
        self
    }

    /// `block` leaving one value of type `ty`. `br 0` out of it must carry that value, which is what
    /// makes a `switch` *expression* a block rather than a statement wrapped in one.
    pub(crate) fn block_typed(&mut self, ty: ValType) -> &mut Self {
        self.out.byte(0x02);
        ty.write(&mut self.out);
        self.depth += 1;
        self
    }

    /// `loop` with no result. `br 0` inside jumps back to its *start*, which is the whole
    /// difference between the two and what makes a `while` two nested labels rather than one.
    pub(crate) fn loop_(&mut self) -> &mut Self {
        self.out.byte(0x03).byte(0x40);
        self.depth += 1;
        self
    }

    /// `if` with no result, taking an `i32` condition.
    pub(crate) fn if_(&mut self) -> &mut Self {
        self.out.byte(0x04).byte(0x40);
        self.depth += 1;
        self
    }

    /// `if` leaving one value of type `ty`, which both arms must produce.
    ///
    /// This — not `select` — is how Java's `?:` and `&&` / `||` lower. `select` pops *both* value
    /// operands, so both arms would already have run: `c ? f() : g()` would call both, and a trapping
    /// arm would trap whether or not it was taken. §15.25 evaluates exactly one arm.
    pub(crate) fn if_typed(&mut self, ty: ValType) -> &mut Self {
        self.out.byte(0x04);
        ty.write(&mut self.out);
        self.depth += 1;
        self
    }

    pub(crate) fn else_(&mut self) -> &mut Self {
        self.out.byte(0x05);
        self
    }

    pub(crate) fn end(&mut self) -> &mut Self {
        self.out.byte(0x0B);
        self.depth = self.depth.saturating_sub(1);
        self
    }

    /// Branch out of `depth` enclosing structures (0 is the innermost).
    pub(crate) fn br(&mut self, depth: u32) -> &mut Self {
        self.out.byte(0x0C).u32(depth);
        self
    }

    /// Branch when the `i32` on top is non-zero.
    pub(crate) fn br_if(&mut self, depth: u32) -> &mut Self {
        self.out.byte(0x0D).u32(depth);
        self
    }

    /// Branch to `targets[i]` for the `i32` index `i` on top, or to `default` when it is out of range.
    ///
    /// The index is read as **unsigned**, so one `i32.sub` by the lowest key is the whole bounds check
    /// a `switch` needs: a key below the minimum wraps past 2³¹ and lands on the default with it.
    pub(crate) fn br_table(&mut self, targets: &[u32], default: u32) -> &mut Self {
        self.out.byte(0x0E);
        self.out
            .u32(u32::try_from(targets.len()).unwrap_or(u32::MAX));
        for &target in targets {
            self.out.u32(target);
        }
        self.out.u32(default);
        self
    }

    /// `ref.as_non_null` — trap when the reference on top is `null`, otherwise leave it.
    pub(crate) fn ref_as_non_null(&mut self) -> &mut Self {
        self.out.byte(0xD4);
        self
    }

    /// `throw` — raise `tag` with the value on the stack as its payload.
    pub(crate) fn throw(&mut self, tag: u32) -> &mut Self {
        self.out.byte(0x08).u32(tag);
        self
    }

    /// `try_table` with no result, whose `catches` are `(tag, label)` pairs.
    ///
    /// A catch's label is resolved in the context *enclosing* the `try_table`, not inside it — the
    /// `try_table`'s own label is not in scope for its handlers — so the depth is taken before this
    /// opens. Closed by [`end`](Self::end) like a block.
    pub(crate) fn try_table(&mut self, catches: &[(u32, u32)]) -> &mut Self {
        self.out.byte(0x1F).byte(0x40);
        self.out.count(catches.len());
        for &(tag, label) in catches {
            self.out.byte(0x00).u32(tag).u32(label);
        }
        self.depth += 1;
        self
    }

    /// `unreachable` — a trap. What a path Java cannot reach lowers to: it satisfies the validator's
    /// demand for a value on every path without inventing one.
    pub(crate) fn unreachable(&mut self) -> &mut Self {
        self.out.byte(0x00);
        self
    }

    pub(crate) fn return_(&mut self) -> &mut Self {
        self.out.byte(0x0F);
        self
    }

    pub(crate) fn call(&mut self, func: u32) -> &mut Self {
        self.out.byte(0x10).u32(func);
        self
    }

    pub(crate) fn drop(&mut self) -> &mut Self {
        self.out.byte(0x1A);
        self
    }

    // --- locals -------------------------------------------------------------

    pub(crate) fn local_get(&mut self, index: u32) -> &mut Self {
        self.out.byte(0x20).u32(index);
        self
    }

    pub(crate) fn local_set(&mut self, index: u32) -> &mut Self {
        self.out.byte(0x21).u32(index);
        self
    }

    pub(crate) fn local_tee(&mut self, index: u32) -> &mut Self {
        self.out.byte(0x22).u32(index);
        self
    }

    pub(crate) fn global_get(&mut self, index: u32) -> &mut Self {
        self.out.byte(0x23).u32(index);
        self
    }

    pub(crate) fn global_set(&mut self, index: u32) -> &mut Self {
        self.out.byte(0x24).u32(index);
        self
    }

    // --- constants ----------------------------------------------------------

    pub(crate) fn i32_const(&mut self, value: i32) -> &mut Self {
        self.out.byte(0x41).i32(value);
        self
    }

    pub(crate) fn i64_const(&mut self, value: i64) -> &mut Self {
        self.out.byte(0x42).i64(value);
        self
    }

    pub(crate) fn f32_const(&mut self, value: f32) -> &mut Self {
        self.out.byte(0x43).raw(&value.to_le_bytes());
        self
    }

    pub(crate) fn f64_const(&mut self, value: f64) -> &mut Self {
        self.out.byte(0x44).raw(&value.to_le_bytes());
        self
    }

    // --- arithmetic ---------------------------------------------------------

    /// Apply `op` to the two values on top, which are both of type `ty`.
    ///
    /// `None` when the pair has no such operation: `%` on a float is `f32.rem` in Java but has no
    /// wasm instruction at all, and the reference types have no arithmetic.
    pub(crate) fn numeric(&mut self, op: NumOp, ty: ValType) -> Option<&mut Self> {
        // Each family is a contiguous opcode block, but the blocks are not aligned with each
        // other, so the table is written out rather than computed from a base.
        let opcode = match (ty, op) {
            (ValType::I32, NumOp::Add) => 0x6A,
            (ValType::I32, NumOp::Sub) => 0x6B,
            (ValType::I32, NumOp::Mul) => 0x6C,
            (ValType::I32, NumOp::Div) => 0x6D,
            (ValType::I32, NumOp::Rem) => 0x6F,
            (ValType::I32, NumOp::Eq) => 0x46,
            (ValType::I32, NumOp::Ne) => 0x47,
            (ValType::I32, NumOp::Lt) => 0x48,
            (ValType::I32, NumOp::Gt) => 0x4A,
            (ValType::I32, NumOp::Le) => 0x4C,
            (ValType::I32, NumOp::Ge) => 0x4E,
            (ValType::I64, NumOp::Add) => 0x7C,
            (ValType::I64, NumOp::Sub) => 0x7D,
            (ValType::I64, NumOp::Mul) => 0x7E,
            (ValType::I64, NumOp::Div) => 0x7F,
            (ValType::I64, NumOp::Rem) => 0x81,
            (ValType::I64, NumOp::Eq) => 0x51,
            (ValType::I64, NumOp::Ne) => 0x52,
            (ValType::I64, NumOp::Lt) => 0x53,
            (ValType::I64, NumOp::Gt) => 0x55,
            (ValType::I64, NumOp::Le) => 0x57,
            (ValType::I64, NumOp::Ge) => 0x59,
            (ValType::F32, NumOp::Add) => 0x92,
            (ValType::F32, NumOp::Sub) => 0x93,
            (ValType::F32, NumOp::Mul) => 0x94,
            (ValType::F32, NumOp::Div) => 0x95,
            (ValType::F32, NumOp::Eq) => 0x5B,
            (ValType::F32, NumOp::Ne) => 0x5C,
            (ValType::F32, NumOp::Lt) => 0x5D,
            (ValType::F32, NumOp::Gt) => 0x5E,
            (ValType::F32, NumOp::Le) => 0x5F,
            (ValType::F32, NumOp::Ge) => 0x60,
            (ValType::F64, NumOp::Add) => 0xA0,
            (ValType::F64, NumOp::Sub) => 0xA1,
            (ValType::F64, NumOp::Mul) => 0xA2,
            (ValType::F64, NumOp::Div) => 0xA3,
            (ValType::F64, NumOp::Eq) => 0x61,
            (ValType::F64, NumOp::Ne) => 0x62,
            (ValType::F64, NumOp::Lt) => 0x63,
            (ValType::F64, NumOp::Gt) => 0x64,
            (ValType::F64, NumOp::Le) => 0x65,
            (ValType::F64, NumOp::Ge) => 0x66,
            // The bitwise and shift families exist only over the two integer types, which is also
            // where Java has them.
            (ValType::I32, NumOp::And) => 0x71,
            (ValType::I32, NumOp::Or) => 0x72,
            (ValType::I32, NumOp::Xor) => 0x73,
            (ValType::I32, NumOp::Shl) => 0x74,
            (ValType::I32, NumOp::Shr) => 0x75,
            (ValType::I32, NumOp::Ushr) => 0x76,
            (ValType::I64, NumOp::And) => 0x83,
            (ValType::I64, NumOp::Or) => 0x84,
            (ValType::I64, NumOp::Xor) => 0x85,
            (ValType::I64, NumOp::Shl) => 0x86,
            (ValType::I64, NumOp::Shr) => 0x87,
            (ValType::I64, NumOp::Ushr) => 0x88,
            _ => return None,
        };
        self.out.byte(opcode);
        Some(self)
    }

    /// `i32.eqz` — also how a `boolean` is negated, since it is an `i32` that is 0 or 1.
    pub(crate) fn i32_eqz(&mut self) -> &mut Self {
        self.out.byte(0x45);
        self
    }

    /// Negate the floating value on top. wasm has no integer negation at all — `0 - x` is the way, and
    /// the lowering emits that pair rather than pretending there is an opcode.
    pub(crate) fn neg(&mut self, ty: ValType) -> Option<&mut Self> {
        let opcode = match ty {
            ValType::F32 => 0x8C,
            ValType::F64 => 0x9A,
            _ => return None,
        };
        self.out.byte(opcode);
        Some(self)
    }

    /// Convert the value on top from `from` to `to`.
    ///
    /// Two steps whenever a narrowing to `byte` / `short` / `char` does not start from `int`, exactly as
    /// JLS §5.1.3 defines it — and the second step is *not* the same instruction in each case: `byte`
    /// and `short` sign-extend (`i32.extend8_s` / `i32.extend16_s`) while `char` masks, because it is
    /// the one unsigned integral type.
    ///
    /// **The float-to-integer conversions are the saturating ones.** wasm's `i32.trunc_f32_s` *traps*
    /// on a NaN or an out-of-range value; JLS §5.1.3 requires 0 for a NaN and the nearest
    /// representable value otherwise, which is what `i32.trunc_sat_f32_s` does. Using the trapping
    /// form would turn `(int) (0.0 / 0.0)` from a 0 into a crash.
    pub(crate) fn convert(&mut self, from: Numeric, to: Numeric) -> Option<&mut Self> {
        use Numeric::{Byte, Char, Double, Float, Int, Long, Short};
        if from.val() != to.val() {
            match (from, to) {
                (Byte | Short | Char | Int, Long) => self.out.byte(0xAC),
                (Byte | Short | Char | Int, Float) => self.out.byte(0xB2),
                (Byte | Short | Char | Int, Double) => self.out.byte(0xB7),
                (Long, Byte | Short | Char | Int) => self.out.byte(0xA7),
                (Long, Float) => self.out.byte(0xB4),
                (Long, Double) => self.out.byte(0xB9),
                (Float, Byte | Short | Char | Int) => self.saturating(0x00),
                (Float, Long) => self.saturating(0x04),
                (Float, Double) => self.out.byte(0xBB),
                (Double, Byte | Short | Char | Int) => self.saturating(0x02),
                (Double, Long) => self.saturating(0x06),
                (Double, Float) => self.out.byte(0xB6),
                _ => return None,
            };
        }
        // Step two, skipped where the source's range already fits the target's — `byte` to `short`
        // needs nothing, while `byte` to `char` needs the mask because a signed byte's negative half
        // has no place in an unsigned `char`.
        match to {
            Byte if from != Byte => {
                self.out.byte(0xC0);
            }
            Short if !matches!(from, Byte | Short) => {
                self.out.byte(0xC1);
            }
            Char if from != Char => {
                // No `extend16_u`; the mask is the conversion.
                self.out.byte(0x41).i32(0xFFFF);
                self.out.byte(0x71);
            }
            _ => {}
        }
        Some(self)
    }

    /// One of the `0xFC`-prefixed saturating truncations.
    fn saturating(&mut self, opcode: u8) -> &mut Bytes {
        self.out.byte(0xFC).u32(u32::from(opcode))
    }

    // --- garbage-collected heap ---------------------------------------------

    /// Allocate a struct of type `ty` with every field at its type's default. This is Java's
    /// `new`, and the host's collector owns the result from here on.
    pub(crate) fn struct_new_default(&mut self, ty: u32) -> &mut Self {
        self.gc(0x01).u32(ty);
        self
    }

    pub(crate) fn struct_get(&mut self, ty: u32, field: u32) -> &mut Self {
        self.gc(0x02).u32(ty).u32(field);
        self
    }

    pub(crate) fn struct_set(&mut self, ty: u32, field: u32) -> &mut Self {
        self.gc(0x05).u32(ty).u32(field);
        self
    }

    /// Allocate an array of type `ty` with `length` elements at their default — Java's
    /// `new T[n]`, whose elements are zero or `null` by definition.
    pub(crate) fn array_new_default(&mut self, ty: u32) -> &mut Self {
        self.gc(0x07).u32(ty);
        self
    }

    /// Allocate an array of type `ty` from the `count` values already on the stack, first element
    /// deepest — Java's `{1, 2, 3}`, where the elements are written rather than defaulted.
    pub(crate) fn array_new_fixed(&mut self, ty: u32, count: u32) -> &mut Self {
        self.gc(0x08).u32(ty).u32(count);
        self
    }

    pub(crate) fn array_get(&mut self, ty: u32) -> &mut Self {
        self.gc(0x0B).u32(ty);
        self
    }

    pub(crate) fn array_set(&mut self, ty: u32) -> &mut Self {
        self.gc(0x0E).u32(ty);
        self
    }

    pub(crate) fn array_len(&mut self) -> &mut Self {
        self.gc(0x0F);
        self
    }

    // --- references ---------------------------------------------------------

    /// `ref.null` of a concrete type — Java's `null`, which has no type of its own.
    pub(crate) fn ref_null(&mut self, heap: HeapType) -> &mut Self {
        self.out.byte(0xD0);
        heap.write_to(&mut self.out);
        self
    }

    /// `ref.is_null`, which is how `x == null` is asked without a second operand.
    pub(crate) fn ref_is_null(&mut self) -> &mut Self {
        self.out.byte(0xD1);
        self
    }

    /// `ref.eq` — reference identity, which is what Java's `==` means over two references.
    pub(crate) fn ref_eq(&mut self) -> &mut Self {
        self.out.byte(0xD3);
        self
    }

    /// `ref.test (ref null ht)` — Java's `instanceof`, except that `instanceof` is false for `null`
    /// and the nullable form of this is true, so the caller pairs it with a null check.
    pub(crate) fn ref_test(&mut self, heap: HeapType, nullable: bool) -> &mut Self {
        self.gc(if nullable { 0x15 } else { 0x14 });
        heap.write_to(&mut self.out);
        self
    }

    /// `ref.cast` — Java's checked cast. It traps rather than throwing a `ClassCastException`, which
    /// is the closest a host with no exception model gets.
    pub(crate) fn ref_cast(&mut self, heap: HeapType, nullable: bool) -> &mut Self {
        self.gc(if nullable { 0x17 } else { 0x16 });
        heap.write_to(&mut self.out);
        self
    }

    /// The `0xFB` prefix every garbage-collection instruction carries.
    fn gc(&mut self, opcode: u8) -> &mut Bytes {
        self.out.byte(0xFB).u32(u32::from(opcode))
    }
}
