//! The instruction stream: one method per operation a lowering wants to emit, recorded as
//! [`Instr`] values and encoded only when the module is finished.
//!
//! Named after what they do rather than after their opcodes, for the same reason the JVM assembler
//! is: the caller is a code generator reasoning about Java, not about byte values.
//!
//! Control flow here is *structured* — `block` / `loop` / `if` / `br` — which is why the wasm
//! backend lowers from the syntax tree rather than from the JVM bytecode the other backend emits.
//! Recovering `while` from a `goto` would mean a relooper; keeping the tree means the nesting is
//! already right.
//!
//! # Why the instructions are kept rather than encoded
//!
//! [`Insn`] used to append opcode bytes as each method was called, which made a body opaque the
//! instant it was built: there was nothing to read back, so the only way to ask what a lowering
//! emitted was to hand a whole module to an engine. The JVM assembler does not work that way — it
//! records [`Item`](crate::jvm::Assembler)s and materializes them in `finish` — and the asymmetry
//! showed up as a 4,900-line lowering with no test that could name a single instruction. Keeping
//! the stream costs one `Vec` per body and buys the same footing the other backend has.

use crate::facts::Numeric;
use alloc::vec::Vec;

use crate::wasm::encode::{Bytes, HeapType, ValType};

/// A binary numeric operation, resolved to an opcode by the type it applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumOp {
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

    /// The opcode for this operator applied to `ty`, or `None` when the pair has no such
    /// instruction: `%` on a float is `f32.rem` in Java but has no wasm instruction at all, and the
    /// reference types have no arithmetic.
    ///
    /// One table, read by both the builder that refuses an unrepresentable pair
    /// ([`Insn::numeric`]) and the encoder that writes a representable one
    /// ([`Instr::write`]) — so the two cannot disagree about which pairs exist.
    const fn opcode(self, ty: ValType) -> Option<u8> {
        // Each family is a contiguous opcode block, but the blocks are not aligned with each
        // other, so the table is written out rather than computed from a base.
        Some(match (ty, self) {
            (ValType::I32, Self::Add) => 0x6A,
            (ValType::I32, Self::Sub) => 0x6B,
            (ValType::I32, Self::Mul) => 0x6C,
            (ValType::I32, Self::Div) => 0x6D,
            (ValType::I32, Self::Rem) => 0x6F,
            (ValType::I32, Self::Eq) => 0x46,
            (ValType::I32, Self::Ne) => 0x47,
            (ValType::I32, Self::Lt) => 0x48,
            (ValType::I32, Self::Gt) => 0x4A,
            (ValType::I32, Self::Le) => 0x4C,
            (ValType::I32, Self::Ge) => 0x4E,
            (ValType::I64, Self::Add) => 0x7C,
            (ValType::I64, Self::Sub) => 0x7D,
            (ValType::I64, Self::Mul) => 0x7E,
            (ValType::I64, Self::Div) => 0x7F,
            (ValType::I64, Self::Rem) => 0x81,
            (ValType::I64, Self::Eq) => 0x51,
            (ValType::I64, Self::Ne) => 0x52,
            (ValType::I64, Self::Lt) => 0x53,
            (ValType::I64, Self::Gt) => 0x55,
            (ValType::I64, Self::Le) => 0x57,
            (ValType::I64, Self::Ge) => 0x59,
            (ValType::F32, Self::Add) => 0x92,
            (ValType::F32, Self::Sub) => 0x93,
            (ValType::F32, Self::Mul) => 0x94,
            (ValType::F32, Self::Div) => 0x95,
            (ValType::F32, Self::Eq) => 0x5B,
            (ValType::F32, Self::Ne) => 0x5C,
            (ValType::F32, Self::Lt) => 0x5D,
            (ValType::F32, Self::Gt) => 0x5E,
            (ValType::F32, Self::Le) => 0x5F,
            (ValType::F32, Self::Ge) => 0x60,
            (ValType::F64, Self::Add) => 0xA0,
            (ValType::F64, Self::Sub) => 0xA1,
            (ValType::F64, Self::Mul) => 0xA2,
            (ValType::F64, Self::Div) => 0xA3,
            (ValType::F64, Self::Eq) => 0x61,
            (ValType::F64, Self::Ne) => 0x62,
            (ValType::F64, Self::Lt) => 0x63,
            (ValType::F64, Self::Gt) => 0x64,
            (ValType::F64, Self::Le) => 0x65,
            (ValType::F64, Self::Ge) => 0x66,
            // The bitwise and shift families exist only over the two integer types, which is also
            // where Java has them.
            (ValType::I32, Self::And) => 0x71,
            (ValType::I32, Self::Or) => 0x72,
            (ValType::I32, Self::Xor) => 0x73,
            (ValType::I32, Self::Shl) => 0x74,
            (ValType::I32, Self::Shr) => 0x75,
            (ValType::I32, Self::Ushr) => 0x76,
            (ValType::I64, Self::And) => 0x83,
            (ValType::I64, Self::Or) => 0x84,
            (ValType::I64, Self::Xor) => 0x85,
            (ValType::I64, Self::Shl) => 0x86,
            (ValType::I64, Self::Shr) => 0x87,
            (ValType::I64, Self::Ushr) => 0x88,
            _ => return None,
        })
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

/// One WebAssembly instruction.
///
/// The unit a body is recorded in, so that what a lowering emitted can be read back before any of
/// it becomes bytes. Every variant is exactly one instruction: a Java construct that needs several
/// — a narrowing cast, which JLS §5.1.3 defines in two steps — pushes several, rather than hiding
/// the sequence behind one name. That is the property that lets a test assert what was emitted
/// without an engine to run it.
#[derive(Debug, Clone, PartialEq)]
pub enum Instr {
    // --- control ------------------------------------------------------------
    /// `block` with no result. Closed by [`End`](Self::End); `br 0` inside jumps *past* it.
    Block,
    /// `block` leaving one value. `br 0` out of it must carry that value, which is what makes a
    /// `switch` *expression* a block rather than a statement wrapped in one.
    BlockTyped(ValType),
    /// `loop` with no result. `br 0` inside jumps back to its *start*, which is the whole
    /// difference from a block and what makes a `while` two nested labels rather than one.
    Loop,
    /// `if` with no result, taking an `i32` condition.
    If,
    /// `if` leaving one value, which both arms must produce.
    ///
    /// This — not `select` — is how Java's `?:` and `&&` / `||` lower. `select` pops *both* value
    /// operands, so both arms would already have run: `c ? f() : g()` would call both, and a
    /// trapping arm would trap whether or not it was taken. §15.25 evaluates exactly one arm.
    IfTyped(ValType),
    Else,
    End,
    /// Branch out of the named number of enclosing structures (0 is the innermost).
    Br(u32),
    /// Branch when the `i32` on top is non-zero.
    BrIf(u32),
    /// Branch to `targets[i]` for the `i32` index `i` on top, or to the default when it is out of
    /// range.
    ///
    /// The index is read as **unsigned**, so one `i32.sub` by the lowest key is the whole bounds
    /// check a `switch` needs: a key below the minimum wraps past 2³¹ and lands on the default with
    /// it.
    BrTable(Vec<u32>, u32),
    /// `try_table` with no result, whose catches are `(tag, label)` pairs.
    ///
    /// A catch's label is resolved in the context *enclosing* the `try_table`, not inside it — the
    /// `try_table`'s own label is not in scope for its handlers. Closed by [`End`](Self::End) like
    /// a block.
    TryTable(Vec<(u32, u32)>),
    /// `throw` — raise the named tag with the value on the stack as its payload.
    Throw(u32),
    /// `ref.as_non_null` — trap when the reference on top is `null`, otherwise leave it.
    RefAsNonNull,
    /// `unreachable` — a trap. What a path Java cannot reach lowers to: it satisfies the
    /// validator's demand for a value on every path without inventing one.
    Unreachable,
    Return,
    Call(u32),
    Drop,

    // --- locals and globals -------------------------------------------------
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),

    // --- constants ----------------------------------------------------------
    I32Const(i32),
    I64Const(i64),
    F32Const(f32),
    F64Const(f64),

    // --- arithmetic ---------------------------------------------------------
    /// Apply the operator to the two values on top, which are both of the named type.
    ///
    /// A pair [`NumOp::opcode`] has no instruction for encodes nothing; [`Insn::numeric`] refuses
    /// one at the point it would be recorded, so a lowering cannot produce it.
    Numeric(NumOp, ValType),
    /// `i32.eqz` — also how a `boolean` is negated, since it is an `i32` that is 0 or 1.
    I32Eqz,
    /// `f32.neg`. wasm has no *integer* negation at all — `0 - x` is the way, and the lowering
    /// emits that pair rather than pretending there is an opcode.
    F32Neg,
    /// `f64.neg`.
    F64Neg,

    // --- conversions --------------------------------------------------------
    //
    // Spelled out one instruction per variant rather than folded into a `Convert(from, to)`, so
    // that a rendered body shows *which* conversion ran. The saturating truncations below are the
    // whole reason that matters.
    /// `i64.extend_i32_s`
    I64ExtendI32S,
    /// `i32.wrap_i64`
    I32WrapI64,
    /// `f32.convert_i32_s`
    F32ConvertI32S,
    /// `f64.convert_i32_s`
    F64ConvertI32S,
    /// `f32.convert_i64_s`
    F32ConvertI64S,
    /// `f64.convert_i64_s`
    F64ConvertI64S,
    /// `f32.demote_f64`
    F32DemoteF64,
    /// `f64.promote_f32`
    F64PromoteF32,
    /// `i32.trunc_sat_f32_s`. **Saturating, not trapping.** wasm's `i32.trunc_f32_s` traps on a NaN
    /// or an out-of-range value; JLS §5.1.3 requires 0 for a NaN and the nearest representable
    /// value otherwise. The trapping form would turn `(int) (0.0 / 0.0)` from a 0 into a crash.
    I32TruncSatF32S,
    /// `i32.trunc_sat_f64_s`. Saturating, for the reason [`I32TruncSatF32S`](Self::I32TruncSatF32S)
    /// gives.
    I32TruncSatF64S,
    /// `i64.trunc_sat_f32_s`. Saturating, for the reason [`I32TruncSatF32S`](Self::I32TruncSatF32S)
    /// gives.
    I64TruncSatF32S,
    /// `i64.trunc_sat_f64_s`. Saturating, for the reason [`I32TruncSatF32S`](Self::I32TruncSatF32S)
    /// gives.
    I64TruncSatF64S,
    /// `i32.extend8_s` — the second step of a narrowing to `byte`.
    I32Extend8S,
    /// `i32.extend16_s` — the second step of a narrowing to `short`.
    I32Extend16S,

    // --- garbage-collected heap ---------------------------------------------
    /// Allocate a struct with every field at its type's default. This is Java's `new`, and the
    /// host's collector owns the result from here on.
    StructNewDefault(u32),
    StructGet(u32, u32),
    StructSet(u32, u32),
    /// Allocate an array with its elements at their default — Java's `new T[n]`, whose elements are
    /// zero or `null` by definition.
    ArrayNewDefault(u32),
    /// Allocate an array from the values already on the stack, first element deepest — Java's
    /// `{1, 2, 3}`, where the elements are written rather than defaulted.
    ArrayNewFixed(u32, u32),
    ArrayGet(u32),
    ArraySet(u32),
    ArrayLen,

    // --- references ---------------------------------------------------------
    /// `ref.null` — Java's `null`, which has no type of its own.
    RefNull(HeapType),
    /// `ref.is_null`, which is how `x == null` is asked without a second operand.
    RefIsNull,
    /// `ref.eq` — reference identity, which is what Java's `==` means over two references.
    RefEq,
    /// `ref.test` — Java's `instanceof`, except that `instanceof` is false for `null` and the
    /// nullable form of this is true, so the caller pairs it with a null check. The flag is that
    /// nullability.
    RefTest(HeapType, bool),
    /// `ref.cast` — Java's checked cast. It traps rather than throwing a `ClassCastException`,
    /// which is the closest a host with no exception model gets. The flag is nullability.
    RefCast(HeapType, bool),
}

impl Instr {
    /// The `0xFB` prefix every garbage-collection instruction carries.
    fn gc(out: &mut Bytes, opcode: u8) -> &mut Bytes {
        out.byte(0xFB).u32(u32::from(opcode))
    }

    /// One of the `0xFC`-prefixed saturating truncations.
    fn saturating(out: &mut Bytes, opcode: u8) -> &mut Bytes {
        out.byte(0xFC).u32(u32::from(opcode))
    }

    /// The single conversion instruction that moves a value between two *different* wasm value
    /// types, or `None` for a pair that needs none.
    ///
    /// Only the first of JLS §5.1.3's two steps; the second narrows within `i32` and is chosen by
    /// the target alone, which is why [`Insn::convert`] appends it separately.
    const fn conversion(from: Numeric, to: Numeric) -> Option<Self> {
        use Numeric::{Byte, Char, Double, Float, Int, Long, Short};
        Some(match (from, to) {
            (Byte | Short | Char | Int, Long) => Self::I64ExtendI32S,
            (Byte | Short | Char | Int, Float) => Self::F32ConvertI32S,
            (Byte | Short | Char | Int, Double) => Self::F64ConvertI32S,
            (Long, Byte | Short | Char | Int) => Self::I32WrapI64,
            (Long, Float) => Self::F32ConvertI64S,
            (Long, Double) => Self::F64ConvertI64S,
            (Float, Byte | Short | Char | Int) => Self::I32TruncSatF32S,
            (Float, Long) => Self::I64TruncSatF32S,
            (Float, Double) => Self::F64PromoteF32,
            (Double, Byte | Short | Char | Int) => Self::I32TruncSatF64S,
            (Double, Long) => Self::I64TruncSatF64S,
            (Double, Float) => Self::F32DemoteF64,
            _ => return None,
        })
    }

    /// Append this instruction's encoding to `out`.
    pub(crate) fn write(&self, out: &mut Bytes) {
        match self {
            // --- control ---
            Self::Block => {
                out.byte(0x02).byte(0x40);
            }
            Self::BlockTyped(ty) => {
                out.byte(0x02);
                ty.write(out);
            }
            Self::Loop => {
                out.byte(0x03).byte(0x40);
            }
            Self::If => {
                out.byte(0x04).byte(0x40);
            }
            Self::IfTyped(ty) => {
                out.byte(0x04);
                ty.write(out);
            }
            Self::Else => {
                out.byte(0x05);
            }
            Self::End => {
                out.byte(0x0B);
            }
            Self::Br(depth) => {
                out.byte(0x0C).u32(*depth);
            }
            Self::BrIf(depth) => {
                out.byte(0x0D).u32(*depth);
            }
            Self::BrTable(targets, default) => {
                out.byte(0x0E);
                out.u32(u32::try_from(targets.len()).unwrap_or(u32::MAX));
                for &target in targets {
                    out.u32(target);
                }
                out.u32(*default);
            }
            Self::TryTable(catches) => {
                out.byte(0x1F).byte(0x40);
                out.count(catches.len());
                for &(tag, label) in catches {
                    out.byte(0x00).u32(tag).u32(label);
                }
            }
            Self::Throw(tag) => {
                out.byte(0x08).u32(*tag);
            }
            Self::RefAsNonNull => {
                out.byte(0xD4);
            }
            Self::Unreachable => {
                out.byte(0x00);
            }
            Self::Return => {
                out.byte(0x0F);
            }
            Self::Call(func) => {
                out.byte(0x10).u32(*func);
            }
            Self::Drop => {
                out.byte(0x1A);
            }

            // --- locals and globals ---
            Self::LocalGet(index) => {
                out.byte(0x20).u32(*index);
            }
            Self::LocalSet(index) => {
                out.byte(0x21).u32(*index);
            }
            Self::LocalTee(index) => {
                out.byte(0x22).u32(*index);
            }
            Self::GlobalGet(index) => {
                out.byte(0x23).u32(*index);
            }
            Self::GlobalSet(index) => {
                out.byte(0x24).u32(*index);
            }

            // --- constants ---
            Self::I32Const(value) => {
                out.byte(0x41).i32(*value);
            }
            Self::I64Const(value) => {
                out.byte(0x42).i64(*value);
            }
            Self::F32Const(value) => {
                out.byte(0x43).raw(&value.to_le_bytes());
            }
            Self::F64Const(value) => {
                out.byte(0x44).raw(&value.to_le_bytes());
            }

            // --- arithmetic ---
            Self::Numeric(op, ty) => {
                if let Some(opcode) = op.opcode(*ty) {
                    out.byte(opcode);
                }
            }
            Self::I32Eqz => {
                out.byte(0x45);
            }
            Self::F32Neg => {
                out.byte(0x8C);
            }
            Self::F64Neg => {
                out.byte(0x9A);
            }

            // --- conversions ---
            Self::I64ExtendI32S => {
                out.byte(0xAC);
            }
            Self::I32WrapI64 => {
                out.byte(0xA7);
            }
            Self::F32ConvertI32S => {
                out.byte(0xB2);
            }
            Self::F64ConvertI32S => {
                out.byte(0xB7);
            }
            Self::F32ConvertI64S => {
                out.byte(0xB4);
            }
            Self::F64ConvertI64S => {
                out.byte(0xB9);
            }
            Self::F32DemoteF64 => {
                out.byte(0xB6);
            }
            Self::F64PromoteF32 => {
                out.byte(0xBB);
            }
            Self::I32TruncSatF32S => {
                Self::saturating(out, 0x00);
            }
            Self::I32TruncSatF64S => {
                Self::saturating(out, 0x02);
            }
            Self::I64TruncSatF32S => {
                Self::saturating(out, 0x04);
            }
            Self::I64TruncSatF64S => {
                Self::saturating(out, 0x06);
            }
            Self::I32Extend8S => {
                out.byte(0xC0);
            }
            Self::I32Extend16S => {
                out.byte(0xC1);
            }

            // --- garbage-collected heap ---
            Self::StructNewDefault(ty) => {
                Self::gc(out, 0x01).u32(*ty);
            }
            Self::StructGet(ty, field) => {
                Self::gc(out, 0x02).u32(*ty).u32(*field);
            }
            Self::StructSet(ty, field) => {
                Self::gc(out, 0x05).u32(*ty).u32(*field);
            }
            Self::ArrayNewDefault(ty) => {
                Self::gc(out, 0x07).u32(*ty);
            }
            Self::ArrayNewFixed(ty, count) => {
                Self::gc(out, 0x08).u32(*ty).u32(*count);
            }
            Self::ArrayGet(ty) => {
                Self::gc(out, 0x0B).u32(*ty);
            }
            Self::ArraySet(ty) => {
                Self::gc(out, 0x0E).u32(*ty);
            }
            Self::ArrayLen => {
                Self::gc(out, 0x0F);
            }

            // --- references ---
            Self::RefNull(heap) => {
                out.byte(0xD0);
                heap.write_to(out);
            }
            Self::RefIsNull => {
                out.byte(0xD1);
            }
            Self::RefEq => {
                out.byte(0xD3);
            }
            Self::RefTest(heap, nullable) => {
                Self::gc(out, if *nullable { 0x15 } else { 0x14 });
                heap.write_to(out);
            }
            Self::RefCast(heap, nullable) => {
                Self::gc(out, if *nullable { 0x17 } else { 0x16 });
                heap.write_to(out);
            }
        }
    }
}

/// Builds one function body.
#[derive(Debug, Default)]
pub struct Insn {
    code: Vec<Instr>,
    /// How many structured control instructions are currently open.
    ///
    /// A `br` names a *relative* depth, so the target of a `continue` shifts every time an `if` opens
    /// between the loop header and the branch. Nothing in the source says how deep that is; only the
    /// emitter knows, so it counts. A lowering records this at the point it opens a loop and takes the
    /// difference at the branch.
    depth: u32,
}

impl Insn {
    pub const fn new() -> Self {
        Self {
            code: Vec::new(),
            depth: 0,
        }
    }

    /// How many structured control instructions are open here.
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// The instructions recorded so far, *without* the terminating `end`.
    pub fn code(&self) -> &[Instr] {
        &self.code
    }

    /// The finished body, *without* the terminating `end` a function's code entry adds.
    pub fn into_body(self) -> Vec<Instr> {
        self.code
    }

    /// Record one instruction.
    fn push(&mut self, instruction: Instr) -> &mut Self {
        self.code.push(instruction);
        self
    }

    // --- control ------------------------------------------------------------

    /// `block` with no result. Closed by [`end`](Self::end); `br 0` inside jumps *past* it.
    pub fn block(&mut self) -> &mut Self {
        self.depth += 1;
        self.push(Instr::Block)
    }

    /// `block` leaving one value of type `ty`. `br 0` out of it must carry that value, which is what
    /// makes a `switch` *expression* a block rather than a statement wrapped in one.
    pub fn block_typed(&mut self, ty: ValType) -> &mut Self {
        self.depth += 1;
        self.push(Instr::BlockTyped(ty))
    }

    /// `loop` with no result. `br 0` inside jumps back to its *start*, which is the whole
    /// difference between the two and what makes a `while` two nested labels rather than one.
    pub fn loop_(&mut self) -> &mut Self {
        self.depth += 1;
        self.push(Instr::Loop)
    }

    /// `if` with no result, taking an `i32` condition.
    pub fn if_(&mut self) -> &mut Self {
        self.depth += 1;
        self.push(Instr::If)
    }

    /// `if` leaving one value of type `ty`, which both arms must produce.
    ///
    /// This — not `select` — is how Java's `?:` and `&&` / `||` lower. `select` pops *both* value
    /// operands, so both arms would already have run: `c ? f() : g()` would call both, and a trapping
    /// arm would trap whether or not it was taken. §15.25 evaluates exactly one arm.
    pub fn if_typed(&mut self, ty: ValType) -> &mut Self {
        self.depth += 1;
        self.push(Instr::IfTyped(ty))
    }

    pub fn else_(&mut self) -> &mut Self {
        self.push(Instr::Else)
    }

    pub fn end(&mut self) -> &mut Self {
        self.depth = self.depth.saturating_sub(1);
        self.push(Instr::End)
    }

    /// Branch out of `depth` enclosing structures (0 is the innermost).
    pub fn br(&mut self, depth: u32) -> &mut Self {
        self.push(Instr::Br(depth))
    }

    /// Branch when the `i32` on top is non-zero.
    pub fn br_if(&mut self, depth: u32) -> &mut Self {
        self.push(Instr::BrIf(depth))
    }

    /// Branch to `targets[i]` for the `i32` index `i` on top, or to `default` when it is out of range.
    ///
    /// The index is read as **unsigned**, so one `i32.sub` by the lowest key is the whole bounds check
    /// a `switch` needs: a key below the minimum wraps past 2³¹ and lands on the default with it.
    pub fn br_table(&mut self, targets: &[u32], default: u32) -> &mut Self {
        self.push(Instr::BrTable(targets.to_vec(), default))
    }

    /// `ref.as_non_null` — trap when the reference on top is `null`, otherwise leave it.
    pub fn ref_as_non_null(&mut self) -> &mut Self {
        self.push(Instr::RefAsNonNull)
    }

    /// `throw` — raise `tag` with the value on the stack as its payload.
    pub fn throw(&mut self, tag: u32) -> &mut Self {
        self.push(Instr::Throw(tag))
    }

    /// `try_table` with no result, whose `catches` are `(tag, label)` pairs.
    ///
    /// A catch's label is resolved in the context *enclosing* the `try_table`, not inside it — the
    /// `try_table`'s own label is not in scope for its handlers — so the depth is taken before this
    /// opens. Closed by [`end`](Self::end) like a block.
    pub fn try_table(&mut self, catches: &[(u32, u32)]) -> &mut Self {
        self.depth += 1;
        self.push(Instr::TryTable(catches.to_vec()))
    }

    /// `unreachable` — a trap. What a path Java cannot reach lowers to: it satisfies the validator's
    /// demand for a value on every path without inventing one.
    pub fn unreachable(&mut self) -> &mut Self {
        self.push(Instr::Unreachable)
    }

    pub fn return_(&mut self) -> &mut Self {
        self.push(Instr::Return)
    }

    pub fn call(&mut self, func: u32) -> &mut Self {
        self.push(Instr::Call(func))
    }

    pub fn drop(&mut self) -> &mut Self {
        self.push(Instr::Drop)
    }

    // --- locals -------------------------------------------------------------

    pub fn local_get(&mut self, index: u32) -> &mut Self {
        self.push(Instr::LocalGet(index))
    }

    pub fn local_set(&mut self, index: u32) -> &mut Self {
        self.push(Instr::LocalSet(index))
    }

    pub fn local_tee(&mut self, index: u32) -> &mut Self {
        self.push(Instr::LocalTee(index))
    }

    pub fn global_get(&mut self, index: u32) -> &mut Self {
        self.push(Instr::GlobalGet(index))
    }

    pub fn global_set(&mut self, index: u32) -> &mut Self {
        self.push(Instr::GlobalSet(index))
    }

    // --- constants ----------------------------------------------------------

    pub fn i32_const(&mut self, value: i32) -> &mut Self {
        self.push(Instr::I32Const(value))
    }

    pub fn i64_const(&mut self, value: i64) -> &mut Self {
        self.push(Instr::I64Const(value))
    }

    pub fn f32_const(&mut self, value: f32) -> &mut Self {
        self.push(Instr::F32Const(value))
    }

    pub fn f64_const(&mut self, value: f64) -> &mut Self {
        self.push(Instr::F64Const(value))
    }

    // --- arithmetic ---------------------------------------------------------

    /// Apply `op` to the two values on top, which are both of type `ty`.
    ///
    /// `None` when the pair has no such operation: `%` on a float is `f32.rem` in Java but has no
    /// wasm instruction at all, and the reference types have no arithmetic. Refusing here is what
    /// keeps an unrepresentable pair out of the recorded stream.
    pub fn numeric(&mut self, op: NumOp, ty: ValType) -> Option<&mut Self> {
        op.opcode(ty)?;
        Some(self.push(Instr::Numeric(op, ty)))
    }

    /// `i32.eqz` — also how a `boolean` is negated, since it is an `i32` that is 0 or 1.
    pub fn i32_eqz(&mut self) -> &mut Self {
        self.push(Instr::I32Eqz)
    }

    /// Negate the floating value on top. wasm has no integer negation at all — `0 - x` is the way, and
    /// the lowering emits that pair rather than pretending there is an opcode.
    pub fn neg(&mut self, ty: ValType) -> Option<&mut Self> {
        let instruction = match ty {
            ValType::F32 => Instr::F32Neg,
            ValType::F64 => Instr::F64Neg,
            _ => return None,
        };
        Some(self.push(instruction))
    }

    /// Convert the value on top from `from` to `to`.
    ///
    /// Two steps whenever a narrowing to `byte` / `short` / `char` does not start from `int`, exactly as
    /// JLS §5.1.3 defines it — and the second step is *not* the same instruction in each case: `byte`
    /// and `short` sign-extend (`i32.extend8_s` / `i32.extend16_s`) while `char` masks, because it is
    /// the one unsigned integral type. Each step is recorded separately, so a reader of the stream
    /// sees the sequence rather than a name standing for it.
    ///
    /// **The float-to-integer conversions are the saturating ones.** wasm's `i32.trunc_f32_s` *traps*
    /// on a NaN or an out-of-range value; JLS §5.1.3 requires 0 for a NaN and the nearest
    /// representable value otherwise, which is what `i32.trunc_sat_f32_s` does. Using the trapping
    /// form would turn `(int) (0.0 / 0.0)` from a 0 into a crash.
    pub fn convert(&mut self, from: Numeric, to: Numeric) -> Option<&mut Self> {
        use Numeric::{Byte, Char, Short};
        if from.val() != to.val() {
            self.push(Instr::conversion(from, to)?);
        }
        // Step two, skipped where the source's range already fits the target's — `byte` to `short`
        // needs nothing, while `byte` to `char` needs the mask because a signed byte's negative half
        // has no place in an unsigned `char`.
        match to {
            Byte if from != Byte => {
                self.push(Instr::I32Extend8S);
            }
            Short if !matches!(from, Byte | Short) => {
                self.push(Instr::I32Extend16S);
            }
            Char if from != Char => {
                // No `extend16_u`; the mask is the conversion.
                self.push(Instr::I32Const(0xFFFF));
                self.push(Instr::Numeric(NumOp::And, ValType::I32));
            }
            _ => {}
        }
        Some(self)
    }

    // --- garbage-collected heap ---------------------------------------------

    /// Allocate a struct of type `ty` with every field at its type's default. This is Java's
    /// `new`, and the host's collector owns the result from here on.
    pub fn struct_new_default(&mut self, ty: u32) -> &mut Self {
        self.push(Instr::StructNewDefault(ty))
    }

    pub fn struct_get(&mut self, ty: u32, field: u32) -> &mut Self {
        self.push(Instr::StructGet(ty, field))
    }

    pub fn struct_set(&mut self, ty: u32, field: u32) -> &mut Self {
        self.push(Instr::StructSet(ty, field))
    }

    /// Allocate an array of type `ty` with `length` elements at their default — Java's
    /// `new T[n]`, whose elements are zero or `null` by definition.
    pub fn array_new_default(&mut self, ty: u32) -> &mut Self {
        self.push(Instr::ArrayNewDefault(ty))
    }

    /// Allocate an array of type `ty` from the `count` values already on the stack, first element
    /// deepest — Java's `{1, 2, 3}`, where the elements are written rather than defaulted.
    pub fn array_new_fixed(&mut self, ty: u32, count: u32) -> &mut Self {
        self.push(Instr::ArrayNewFixed(ty, count))
    }

    pub fn array_get(&mut self, ty: u32) -> &mut Self {
        self.push(Instr::ArrayGet(ty))
    }

    pub fn array_set(&mut self, ty: u32) -> &mut Self {
        self.push(Instr::ArraySet(ty))
    }

    pub fn array_len(&mut self) -> &mut Self {
        self.push(Instr::ArrayLen)
    }

    // --- references ---------------------------------------------------------

    /// `ref.null` of a concrete type — Java's `null`, which has no type of its own.
    pub fn ref_null(&mut self, heap: HeapType) -> &mut Self {
        self.push(Instr::RefNull(heap))
    }

    /// `ref.is_null`, which is how `x == null` is asked without a second operand.
    pub fn ref_is_null(&mut self) -> &mut Self {
        self.push(Instr::RefIsNull)
    }

    /// `ref.eq` — reference identity, which is what Java's `==` means over two references.
    pub fn ref_eq(&mut self) -> &mut Self {
        self.push(Instr::RefEq)
    }

    /// `ref.test (ref null ht)` — Java's `instanceof`, except that `instanceof` is false for `null`
    /// and the nullable form of this is true, so the caller pairs it with a null check.
    pub fn ref_test(&mut self, heap: HeapType, nullable: bool) -> &mut Self {
        self.push(Instr::RefTest(heap, nullable))
    }

    /// `ref.cast` — Java's checked cast. It traps rather than throwing a `ClassCastException`, which
    /// is the closest a host with no exception model gets.
    pub fn ref_cast(&mut self, heap: HeapType, nullable: bool) -> &mut Self {
        self.push(Instr::RefCast(heap, nullable))
    }
}
