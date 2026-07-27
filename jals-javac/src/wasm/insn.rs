//! The instruction encoder: one method per operation a lowering wants to emit.
//!
//! Named after what they do rather than after their opcodes, for the same reason the JVM assembler
//! is: the caller is a code generator reasoning about Java, not about byte values.
//!
//! Control flow here is *structured* — `block` / `loop` / `if` / `br` — which is why the wasm
//! backend lowers from the syntax tree rather than from the JVM bytecode the other backend emits.
//! Recovering `while` from a `goto` would mean a relooper; keeping the tree means the nesting is
//! already right.

use alloc::vec::Vec;

use crate::wasm::encode::{Bytes, ValType};

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
}

/// Builds one function body.
#[derive(Debug, Default)]
pub(crate) struct Insn {
    out: Bytes,
}

impl Insn {
    pub(crate) const fn new() -> Self {
        Self { out: Bytes::new() }
    }

    pub(crate) fn into_body(self) -> Vec<u8> {
        self.out.into_vec()
    }

    // --- control ------------------------------------------------------------

    /// `block` with no result. Closed by [`end`](Self::end); `br 0` inside jumps *past* it.
    pub(crate) fn block(&mut self) -> &mut Self {
        self.out.byte(0x02).byte(0x40);
        self
    }

    /// `loop` with no result. `br 0` inside jumps back to its *start*, which is the whole
    /// difference between the two and what makes a `while` two nested labels rather than one.
    pub(crate) fn loop_(&mut self) -> &mut Self {
        self.out.byte(0x03).byte(0x40);
        self
    }

    /// `if` with no result, taking an `i32` condition.
    pub(crate) fn if_(&mut self) -> &mut Self {
        self.out.byte(0x04).byte(0x40);
        self
    }

    pub(crate) fn else_(&mut self) -> &mut Self {
        self.out.byte(0x05);
        self
    }

    pub(crate) fn end(&mut self) -> &mut Self {
        self.out.byte(0x0B);
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

    /// The `0xFB` prefix every garbage-collection instruction carries.
    fn gc(&mut self, opcode: u8) -> &mut Bytes {
        self.out.byte(0xFB).u32(u32::from(opcode))
    }
}
