//! JVM bytecode instructions (JVMS §6.5) and the `Code` array codec.
//!
//! [`decode_code`] turns a raw `code` byte array into a `Vec<Instruction>`; [`encode_code`] turns it
//! back. The two are exact inverses for valid bytecode: branch offsets are stored verbatim (never
//! recomputed), and the variable-length forms — `tableswitch` / `lookupswitch` 4-byte alignment
//! padding and the `wide` prefix — are reproduced byte-for-byte. An unknown opcode makes
//! `decode_code` fail, which degrades the whole `Code` attribute to `Unknown` (still byte-exact).

use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::bytes::{Reader, Writer};
use crate::error::{ClassfileError, Result};

/// A single decoded bytecode instruction. Operands carry constant-pool indices, local-variable slot
/// numbers, immediate constants, or branch offsets (relative to the instruction, as stored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub enum Instruction {
    Nop,
    AconstNull,
    IconstM1,
    Iconst0,
    Iconst1,
    Iconst2,
    Iconst3,
    Iconst4,
    Iconst5,
    Lconst0,
    Lconst1,
    Fconst0,
    Fconst1,
    Fconst2,
    Dconst0,
    Dconst1,
    /// `bipush`: push a sign-extended byte.
    Bipush(i8),
    /// `sipush`: push a sign-extended short.
    Sipush(i16),
    /// `ldc`: push a constant by 1-byte pool index.
    Ldc(u8),
    /// `ldc_w`: push a constant by 2-byte pool index.
    LdcW(u16),
    /// `ldc2_w`: push a long/double constant by 2-byte pool index.
    Ldc2W(u16),
    /// `iload` (local slot).
    Iload(u8),
    /// `lload` (local slot).
    Lload(u8),
    /// `fload` (local slot).
    Fload(u8),
    /// `dload` (local slot).
    Dload(u8),
    /// `aload` (local slot).
    Aload(u8),
    Iload0,
    Iload1,
    Iload2,
    Iload3,
    Lload0,
    Lload1,
    Lload2,
    Lload3,
    Fload0,
    Fload1,
    Fload2,
    Fload3,
    Dload0,
    Dload1,
    Dload2,
    Dload3,
    Aload0,
    Aload1,
    Aload2,
    Aload3,
    Iaload,
    Laload,
    Faload,
    Daload,
    Aaload,
    Baload,
    Caload,
    Saload,
    /// `istore` (local slot).
    Istore(u8),
    /// `lstore` (local slot).
    Lstore(u8),
    /// `fstore` (local slot).
    Fstore(u8),
    /// `dstore` (local slot).
    Dstore(u8),
    /// `astore` (local slot).
    Astore(u8),
    Istore0,
    Istore1,
    Istore2,
    Istore3,
    Lstore0,
    Lstore1,
    Lstore2,
    Lstore3,
    Fstore0,
    Fstore1,
    Fstore2,
    Fstore3,
    Dstore0,
    Dstore1,
    Dstore2,
    Dstore3,
    Astore0,
    Astore1,
    Astore2,
    Astore3,
    Iastore,
    Lastore,
    Fastore,
    Dastore,
    Aastore,
    Bastore,
    Castore,
    Sastore,
    Pop,
    Pop2,
    Dup,
    DupX1,
    DupX2,
    Dup2,
    Dup2X1,
    Dup2X2,
    Swap,
    Iadd,
    Ladd,
    Fadd,
    Dadd,
    Isub,
    Lsub,
    Fsub,
    Dsub,
    Imul,
    Lmul,
    Fmul,
    Dmul,
    Idiv,
    Ldiv,
    Fdiv,
    Ddiv,
    Irem,
    Lrem,
    Frem,
    Drem,
    Ineg,
    Lneg,
    Fneg,
    Dneg,
    Ishl,
    Lshl,
    Ishr,
    Lshr,
    Iushr,
    Lushr,
    Iand,
    Land,
    Ior,
    Lor,
    Ixor,
    Lxor,
    /// `iinc`: increment local `index` by `value`.
    Iinc {
        /// Local-variable slot.
        index: u8,
        /// Signed increment.
        value: i8,
    },
    I2l,
    I2f,
    I2d,
    L2i,
    L2f,
    L2d,
    F2i,
    F2l,
    F2d,
    D2i,
    D2l,
    D2f,
    I2b,
    I2c,
    I2s,
    Lcmp,
    Fcmpl,
    Fcmpg,
    Dcmpl,
    Dcmpg,
    /// `ifeq` (branch offset).
    Ifeq(i16),
    /// `ifne` (branch offset).
    Ifne(i16),
    /// `iflt` (branch offset).
    Iflt(i16),
    /// `ifge` (branch offset).
    Ifge(i16),
    /// `ifgt` (branch offset).
    Ifgt(i16),
    /// `ifle` (branch offset).
    Ifle(i16),
    /// `if_icmpeq` (branch offset).
    IfIcmpeq(i16),
    /// `if_icmpne` (branch offset).
    IfIcmpne(i16),
    /// `if_icmplt` (branch offset).
    IfIcmplt(i16),
    /// `if_icmpge` (branch offset).
    IfIcmpge(i16),
    /// `if_icmpgt` (branch offset).
    IfIcmpgt(i16),
    /// `if_icmple` (branch offset).
    IfIcmple(i16),
    /// `if_acmpeq` (branch offset).
    IfAcmpeq(i16),
    /// `if_acmpne` (branch offset).
    IfAcmpne(i16),
    /// `goto` (branch offset).
    Goto(i16),
    /// `jsr` (branch offset).
    Jsr(i16),
    /// `ret` (local slot).
    Ret(u8),
    /// `tableswitch`: a dense jump table over `low..=high`.
    TableSwitch {
        /// Default branch offset.
        default: i32,
        /// Lowest key.
        low: i32,
        /// Highest key.
        high: i32,
        /// One branch offset per key in `low..=high`.
        offsets: Vec<i32>,
    },
    /// `lookupswitch`: a sparse `(key, offset)` jump table.
    LookupSwitch {
        /// Default branch offset.
        default: i32,
        /// The `(match key, branch offset)` pairs, in ascending key order.
        pairs: Vec<(i32, i32)>,
    },
    Ireturn,
    Lreturn,
    Freturn,
    Dreturn,
    Areturn,
    Return,
    /// `getstatic` (field-ref pool index).
    GetStatic(u16),
    /// `putstatic` (field-ref pool index).
    PutStatic(u16),
    /// `getfield` (field-ref pool index).
    GetField(u16),
    /// `putfield` (field-ref pool index).
    PutField(u16),
    /// `invokevirtual` (method-ref pool index).
    InvokeVirtual(u16),
    /// `invokespecial` (method-ref pool index).
    InvokeSpecial(u16),
    /// `invokestatic` (method-ref pool index).
    InvokeStatic(u16),
    /// `invokeinterface` (interface-method-ref pool index plus a `count`).
    InvokeInterface {
        /// Interface-method-ref pool index.
        index: u16,
        /// Argument-slot count (the historically-redundant `count` operand).
        count: u8,
    },
    /// `invokedynamic` (invoke-dynamic pool index).
    InvokeDynamic {
        /// Invoke-dynamic pool index.
        index: u16,
    },
    /// `new` (class pool index).
    New(u16),
    /// `newarray` (primitive array type code).
    NewArray(u8),
    /// `anewarray` (class pool index).
    ANewArray(u16),
    ArrayLength,
    Athrow,
    /// `checkcast` (class pool index).
    CheckCast(u16),
    /// `instanceof` (class pool index).
    InstanceOf(u16),
    MonitorEnter,
    MonitorExit,
    /// `wide`: the wide-operand form of a load/store/ret/iinc.
    Wide(WideInstruction),
    /// `multianewarray` (class pool index plus a dimension count).
    MultiANewArray {
        /// Array class pool index.
        index: u16,
        /// Number of dimensions to allocate.
        dimensions: u8,
    },
    /// `ifnull` (branch offset).
    IfNull(i16),
    /// `ifnonnull` (branch offset).
    IfNonNull(i16),
    /// `goto_w` (wide branch offset).
    GotoW(i32),
    /// `jsr_w` (wide branch offset).
    JsrW(i32),
}

/// The instruction following a `wide` prefix (JVMS §6.5 `wide`): a load/store/ret/iinc with a
/// 2-byte local index (and, for `iinc`, a 2-byte increment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub enum WideInstruction {
    Iload(u16),
    Lload(u16),
    Fload(u16),
    Dload(u16),
    Aload(u16),
    Istore(u16),
    Lstore(u16),
    Fstore(u16),
    Dstore(u16),
    Astore(u16),
    Ret(u16),
    /// `wide iinc`.
    Iinc {
        /// Local-variable slot.
        index: u16,
        /// Signed increment.
        value: i16,
    },
}

/// Decode a `Code` attribute's `code` array into instructions.
pub(crate) fn decode_code(bytes: &[u8]) -> Result<Vec<Instruction>> {
    let mut r = Reader::new(bytes);
    let mut out = Vec::new();
    while r.remaining() > 0 {
        out.push(Instruction::read(&mut r)?);
    }
    Ok(out)
}

/// Encode instructions back into a `code` array, reproducing switch alignment padding exactly.
pub(crate) fn encode_code(instructions: &[Instruction]) -> Vec<u8> {
    let mut w = Writer::new();
    for ins in instructions {
        ins.write(&mut w);
    }
    w.into_vec()
}

impl Instruction {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        let opcode = r.u8()?;
        Ok(match opcode {
            0x00 => Self::Nop,
            0x01 => Self::AconstNull,
            0x02 => Self::IconstM1,
            0x03 => Self::Iconst0,
            0x04 => Self::Iconst1,
            0x05 => Self::Iconst2,
            0x06 => Self::Iconst3,
            0x07 => Self::Iconst4,
            0x08 => Self::Iconst5,
            0x09 => Self::Lconst0,
            0x0a => Self::Lconst1,
            0x0b => Self::Fconst0,
            0x0c => Self::Fconst1,
            0x0d => Self::Fconst2,
            0x0e => Self::Dconst0,
            0x0f => Self::Dconst1,
            0x10 => Self::Bipush(r.u8()? as i8),
            0x11 => Self::Sipush(r.u16()? as i16),
            0x12 => Self::Ldc(r.u8()?),
            0x13 => Self::LdcW(r.u16()?),
            0x14 => Self::Ldc2W(r.u16()?),
            0x15 => Self::Iload(r.u8()?),
            0x16 => Self::Lload(r.u8()?),
            0x17 => Self::Fload(r.u8()?),
            0x18 => Self::Dload(r.u8()?),
            0x19 => Self::Aload(r.u8()?),
            0x1a => Self::Iload0,
            0x1b => Self::Iload1,
            0x1c => Self::Iload2,
            0x1d => Self::Iload3,
            0x1e => Self::Lload0,
            0x1f => Self::Lload1,
            0x20 => Self::Lload2,
            0x21 => Self::Lload3,
            0x22 => Self::Fload0,
            0x23 => Self::Fload1,
            0x24 => Self::Fload2,
            0x25 => Self::Fload3,
            0x26 => Self::Dload0,
            0x27 => Self::Dload1,
            0x28 => Self::Dload2,
            0x29 => Self::Dload3,
            0x2a => Self::Aload0,
            0x2b => Self::Aload1,
            0x2c => Self::Aload2,
            0x2d => Self::Aload3,
            0x2e => Self::Iaload,
            0x2f => Self::Laload,
            0x30 => Self::Faload,
            0x31 => Self::Daload,
            0x32 => Self::Aaload,
            0x33 => Self::Baload,
            0x34 => Self::Caload,
            0x35 => Self::Saload,
            0x36 => Self::Istore(r.u8()?),
            0x37 => Self::Lstore(r.u8()?),
            0x38 => Self::Fstore(r.u8()?),
            0x39 => Self::Dstore(r.u8()?),
            0x3a => Self::Astore(r.u8()?),
            0x3b => Self::Istore0,
            0x3c => Self::Istore1,
            0x3d => Self::Istore2,
            0x3e => Self::Istore3,
            0x3f => Self::Lstore0,
            0x40 => Self::Lstore1,
            0x41 => Self::Lstore2,
            0x42 => Self::Lstore3,
            0x43 => Self::Fstore0,
            0x44 => Self::Fstore1,
            0x45 => Self::Fstore2,
            0x46 => Self::Fstore3,
            0x47 => Self::Dstore0,
            0x48 => Self::Dstore1,
            0x49 => Self::Dstore2,
            0x4a => Self::Dstore3,
            0x4b => Self::Astore0,
            0x4c => Self::Astore1,
            0x4d => Self::Astore2,
            0x4e => Self::Astore3,
            0x4f => Self::Iastore,
            0x50 => Self::Lastore,
            0x51 => Self::Fastore,
            0x52 => Self::Dastore,
            0x53 => Self::Aastore,
            0x54 => Self::Bastore,
            0x55 => Self::Castore,
            0x56 => Self::Sastore,
            0x57 => Self::Pop,
            0x58 => Self::Pop2,
            0x59 => Self::Dup,
            0x5a => Self::DupX1,
            0x5b => Self::DupX2,
            0x5c => Self::Dup2,
            0x5d => Self::Dup2X1,
            0x5e => Self::Dup2X2,
            0x5f => Self::Swap,
            0x60 => Self::Iadd,
            0x61 => Self::Ladd,
            0x62 => Self::Fadd,
            0x63 => Self::Dadd,
            0x64 => Self::Isub,
            0x65 => Self::Lsub,
            0x66 => Self::Fsub,
            0x67 => Self::Dsub,
            0x68 => Self::Imul,
            0x69 => Self::Lmul,
            0x6a => Self::Fmul,
            0x6b => Self::Dmul,
            0x6c => Self::Idiv,
            0x6d => Self::Ldiv,
            0x6e => Self::Fdiv,
            0x6f => Self::Ddiv,
            0x70 => Self::Irem,
            0x71 => Self::Lrem,
            0x72 => Self::Frem,
            0x73 => Self::Drem,
            0x74 => Self::Ineg,
            0x75 => Self::Lneg,
            0x76 => Self::Fneg,
            0x77 => Self::Dneg,
            0x78 => Self::Ishl,
            0x79 => Self::Lshl,
            0x7a => Self::Ishr,
            0x7b => Self::Lshr,
            0x7c => Self::Iushr,
            0x7d => Self::Lushr,
            0x7e => Self::Iand,
            0x7f => Self::Land,
            0x80 => Self::Ior,
            0x81 => Self::Lor,
            0x82 => Self::Ixor,
            0x83 => Self::Lxor,
            0x84 => Self::Iinc {
                index: r.u8()?,
                value: r.u8()? as i8,
            },
            0x85 => Self::I2l,
            0x86 => Self::I2f,
            0x87 => Self::I2d,
            0x88 => Self::L2i,
            0x89 => Self::L2f,
            0x8a => Self::L2d,
            0x8b => Self::F2i,
            0x8c => Self::F2l,
            0x8d => Self::F2d,
            0x8e => Self::D2i,
            0x8f => Self::D2l,
            0x90 => Self::D2f,
            0x91 => Self::I2b,
            0x92 => Self::I2c,
            0x93 => Self::I2s,
            0x94 => Self::Lcmp,
            0x95 => Self::Fcmpl,
            0x96 => Self::Fcmpg,
            0x97 => Self::Dcmpl,
            0x98 => Self::Dcmpg,
            0x99 => Self::Ifeq(r.u16()? as i16),
            0x9a => Self::Ifne(r.u16()? as i16),
            0x9b => Self::Iflt(r.u16()? as i16),
            0x9c => Self::Ifge(r.u16()? as i16),
            0x9d => Self::Ifgt(r.u16()? as i16),
            0x9e => Self::Ifle(r.u16()? as i16),
            0x9f => Self::IfIcmpeq(r.u16()? as i16),
            0xa0 => Self::IfIcmpne(r.u16()? as i16),
            0xa1 => Self::IfIcmplt(r.u16()? as i16),
            0xa2 => Self::IfIcmpge(r.u16()? as i16),
            0xa3 => Self::IfIcmpgt(r.u16()? as i16),
            0xa4 => Self::IfIcmple(r.u16()? as i16),
            0xa5 => Self::IfAcmpeq(r.u16()? as i16),
            0xa6 => Self::IfAcmpne(r.u16()? as i16),
            0xa7 => Self::Goto(r.u16()? as i16),
            0xa8 => Self::Jsr(r.u16()? as i16),
            0xa9 => Self::Ret(r.u8()?),
            0xaa => Self::read_table_switch(r)?,
            0xab => Self::read_lookup_switch(r)?,
            0xac => Self::Ireturn,
            0xad => Self::Lreturn,
            0xae => Self::Freturn,
            0xaf => Self::Dreturn,
            0xb0 => Self::Areturn,
            0xb1 => Self::Return,
            0xb2 => Self::GetStatic(r.u16()?),
            0xb3 => Self::PutStatic(r.u16()?),
            0xb4 => Self::GetField(r.u16()?),
            0xb5 => Self::PutField(r.u16()?),
            0xb6 => Self::InvokeVirtual(r.u16()?),
            0xb7 => Self::InvokeSpecial(r.u16()?),
            0xb8 => Self::InvokeStatic(r.u16()?),
            0xb9 => {
                let index = r.u16()?;
                let count = r.u8()?;
                let _zero = r.u8()?;
                Self::InvokeInterface { index, count }
            }
            0xba => {
                let index = r.u16()?;
                let _zero = r.u16()?;
                Self::InvokeDynamic { index }
            }
            0xbb => Self::New(r.u16()?),
            0xbc => Self::NewArray(r.u8()?),
            0xbd => Self::ANewArray(r.u16()?),
            0xbe => Self::ArrayLength,
            0xbf => Self::Athrow,
            0xc0 => Self::CheckCast(r.u16()?),
            0xc1 => Self::InstanceOf(r.u16()?),
            0xc2 => Self::MonitorEnter,
            0xc3 => Self::MonitorExit,
            0xc4 => Self::Wide(WideInstruction::read(r)?),
            0xc5 => Self::MultiANewArray {
                index: r.u16()?,
                dimensions: r.u8()?,
            },
            0xc6 => Self::IfNull(r.u16()? as i16),
            0xc7 => Self::IfNonNull(r.u16()? as i16),
            0xc8 => Self::GotoW(r.u32()? as i32),
            0xc9 => Self::JsrW(r.u32()? as i32),
            other => return Err(ClassfileError::InvalidOpcode(other)),
        })
    }

    fn read_table_switch(r: &mut Reader<'_>) -> Result<Self> {
        skip_switch_padding(r)?;
        let default = r.u32()? as i32;
        let low = r.u32()? as i32;
        let high = r.u32()? as i32;
        let count = i64::from(high) - i64::from(low) + 1;
        if !(0..=i64::from(u32::MAX)).contains(&count) {
            return Err(ClassfileError::Malformed("tableswitch bounds"));
        }
        let mut offsets = Vec::with_capacity(count as usize);
        for _ in 0..count {
            offsets.push(r.u32()? as i32);
        }
        Ok(Self::TableSwitch {
            default,
            low,
            high,
            offsets,
        })
    }

    fn read_lookup_switch(r: &mut Reader<'_>) -> Result<Self> {
        skip_switch_padding(r)?;
        let default = r.u32()? as i32;
        let npairs = r.u32()?;
        let mut pairs = Vec::with_capacity(npairs as usize);
        for _ in 0..npairs {
            let key = r.u32()? as i32;
            let offset = r.u32()? as i32;
            pairs.push((key, offset));
        }
        Ok(Self::LookupSwitch { default, pairs })
    }

    fn write(&self, w: &mut Writer) {
        match self {
            Self::Nop => w.u8(0x00),
            Self::AconstNull => w.u8(0x01),
            Self::IconstM1 => w.u8(0x02),
            Self::Iconst0 => w.u8(0x03),
            Self::Iconst1 => w.u8(0x04),
            Self::Iconst2 => w.u8(0x05),
            Self::Iconst3 => w.u8(0x06),
            Self::Iconst4 => w.u8(0x07),
            Self::Iconst5 => w.u8(0x08),
            Self::Lconst0 => w.u8(0x09),
            Self::Lconst1 => w.u8(0x0a),
            Self::Fconst0 => w.u8(0x0b),
            Self::Fconst1 => w.u8(0x0c),
            Self::Fconst2 => w.u8(0x0d),
            Self::Dconst0 => w.u8(0x0e),
            Self::Dconst1 => w.u8(0x0f),
            Self::Bipush(v) => {
                w.u8(0x10);
                w.u8(v.cast_unsigned());
            }
            Self::Sipush(v) => {
                w.u8(0x11);
                w.u16(v.cast_unsigned());
            }
            Self::Ldc(v) => {
                w.u8(0x12);
                w.u8(*v);
            }
            Self::LdcW(v) => {
                w.u8(0x13);
                w.u16(*v);
            }
            Self::Ldc2W(v) => {
                w.u8(0x14);
                w.u16(*v);
            }
            Self::Iload(v) => {
                w.u8(0x15);
                w.u8(*v);
            }
            Self::Lload(v) => {
                w.u8(0x16);
                w.u8(*v);
            }
            Self::Fload(v) => {
                w.u8(0x17);
                w.u8(*v);
            }
            Self::Dload(v) => {
                w.u8(0x18);
                w.u8(*v);
            }
            Self::Aload(v) => {
                w.u8(0x19);
                w.u8(*v);
            }
            Self::Iload0 => w.u8(0x1a),
            Self::Iload1 => w.u8(0x1b),
            Self::Iload2 => w.u8(0x1c),
            Self::Iload3 => w.u8(0x1d),
            Self::Lload0 => w.u8(0x1e),
            Self::Lload1 => w.u8(0x1f),
            Self::Lload2 => w.u8(0x20),
            Self::Lload3 => w.u8(0x21),
            Self::Fload0 => w.u8(0x22),
            Self::Fload1 => w.u8(0x23),
            Self::Fload2 => w.u8(0x24),
            Self::Fload3 => w.u8(0x25),
            Self::Dload0 => w.u8(0x26),
            Self::Dload1 => w.u8(0x27),
            Self::Dload2 => w.u8(0x28),
            Self::Dload3 => w.u8(0x29),
            Self::Aload0 => w.u8(0x2a),
            Self::Aload1 => w.u8(0x2b),
            Self::Aload2 => w.u8(0x2c),
            Self::Aload3 => w.u8(0x2d),
            Self::Iaload => w.u8(0x2e),
            Self::Laload => w.u8(0x2f),
            Self::Faload => w.u8(0x30),
            Self::Daload => w.u8(0x31),
            Self::Aaload => w.u8(0x32),
            Self::Baload => w.u8(0x33),
            Self::Caload => w.u8(0x34),
            Self::Saload => w.u8(0x35),
            Self::Istore(v) => {
                w.u8(0x36);
                w.u8(*v);
            }
            Self::Lstore(v) => {
                w.u8(0x37);
                w.u8(*v);
            }
            Self::Fstore(v) => {
                w.u8(0x38);
                w.u8(*v);
            }
            Self::Dstore(v) => {
                w.u8(0x39);
                w.u8(*v);
            }
            Self::Astore(v) => {
                w.u8(0x3a);
                w.u8(*v);
            }
            Self::Istore0 => w.u8(0x3b),
            Self::Istore1 => w.u8(0x3c),
            Self::Istore2 => w.u8(0x3d),
            Self::Istore3 => w.u8(0x3e),
            Self::Lstore0 => w.u8(0x3f),
            Self::Lstore1 => w.u8(0x40),
            Self::Lstore2 => w.u8(0x41),
            Self::Lstore3 => w.u8(0x42),
            Self::Fstore0 => w.u8(0x43),
            Self::Fstore1 => w.u8(0x44),
            Self::Fstore2 => w.u8(0x45),
            Self::Fstore3 => w.u8(0x46),
            Self::Dstore0 => w.u8(0x47),
            Self::Dstore1 => w.u8(0x48),
            Self::Dstore2 => w.u8(0x49),
            Self::Dstore3 => w.u8(0x4a),
            Self::Astore0 => w.u8(0x4b),
            Self::Astore1 => w.u8(0x4c),
            Self::Astore2 => w.u8(0x4d),
            Self::Astore3 => w.u8(0x4e),
            Self::Iastore => w.u8(0x4f),
            Self::Lastore => w.u8(0x50),
            Self::Fastore => w.u8(0x51),
            Self::Dastore => w.u8(0x52),
            Self::Aastore => w.u8(0x53),
            Self::Bastore => w.u8(0x54),
            Self::Castore => w.u8(0x55),
            Self::Sastore => w.u8(0x56),
            Self::Pop => w.u8(0x57),
            Self::Pop2 => w.u8(0x58),
            Self::Dup => w.u8(0x59),
            Self::DupX1 => w.u8(0x5a),
            Self::DupX2 => w.u8(0x5b),
            Self::Dup2 => w.u8(0x5c),
            Self::Dup2X1 => w.u8(0x5d),
            Self::Dup2X2 => w.u8(0x5e),
            Self::Swap => w.u8(0x5f),
            Self::Iadd => w.u8(0x60),
            Self::Ladd => w.u8(0x61),
            Self::Fadd => w.u8(0x62),
            Self::Dadd => w.u8(0x63),
            Self::Isub => w.u8(0x64),
            Self::Lsub => w.u8(0x65),
            Self::Fsub => w.u8(0x66),
            Self::Dsub => w.u8(0x67),
            Self::Imul => w.u8(0x68),
            Self::Lmul => w.u8(0x69),
            Self::Fmul => w.u8(0x6a),
            Self::Dmul => w.u8(0x6b),
            Self::Idiv => w.u8(0x6c),
            Self::Ldiv => w.u8(0x6d),
            Self::Fdiv => w.u8(0x6e),
            Self::Ddiv => w.u8(0x6f),
            Self::Irem => w.u8(0x70),
            Self::Lrem => w.u8(0x71),
            Self::Frem => w.u8(0x72),
            Self::Drem => w.u8(0x73),
            Self::Ineg => w.u8(0x74),
            Self::Lneg => w.u8(0x75),
            Self::Fneg => w.u8(0x76),
            Self::Dneg => w.u8(0x77),
            Self::Ishl => w.u8(0x78),
            Self::Lshl => w.u8(0x79),
            Self::Ishr => w.u8(0x7a),
            Self::Lshr => w.u8(0x7b),
            Self::Iushr => w.u8(0x7c),
            Self::Lushr => w.u8(0x7d),
            Self::Iand => w.u8(0x7e),
            Self::Land => w.u8(0x7f),
            Self::Ior => w.u8(0x80),
            Self::Lor => w.u8(0x81),
            Self::Ixor => w.u8(0x82),
            Self::Lxor => w.u8(0x83),
            Self::Iinc { index, value } => {
                w.u8(0x84);
                w.u8(*index);
                w.u8(*value as u8);
            }
            Self::I2l => w.u8(0x85),
            Self::I2f => w.u8(0x86),
            Self::I2d => w.u8(0x87),
            Self::L2i => w.u8(0x88),
            Self::L2f => w.u8(0x89),
            Self::L2d => w.u8(0x8a),
            Self::F2i => w.u8(0x8b),
            Self::F2l => w.u8(0x8c),
            Self::F2d => w.u8(0x8d),
            Self::D2i => w.u8(0x8e),
            Self::D2l => w.u8(0x8f),
            Self::D2f => w.u8(0x90),
            Self::I2b => w.u8(0x91),
            Self::I2c => w.u8(0x92),
            Self::I2s => w.u8(0x93),
            Self::Lcmp => w.u8(0x94),
            Self::Fcmpl => w.u8(0x95),
            Self::Fcmpg => w.u8(0x96),
            Self::Dcmpl => w.u8(0x97),
            Self::Dcmpg => w.u8(0x98),
            Self::Ifeq(v) => write_branch(w, 0x99, *v),
            Self::Ifne(v) => write_branch(w, 0x9a, *v),
            Self::Iflt(v) => write_branch(w, 0x9b, *v),
            Self::Ifge(v) => write_branch(w, 0x9c, *v),
            Self::Ifgt(v) => write_branch(w, 0x9d, *v),
            Self::Ifle(v) => write_branch(w, 0x9e, *v),
            Self::IfIcmpeq(v) => write_branch(w, 0x9f, *v),
            Self::IfIcmpne(v) => write_branch(w, 0xa0, *v),
            Self::IfIcmplt(v) => write_branch(w, 0xa1, *v),
            Self::IfIcmpge(v) => write_branch(w, 0xa2, *v),
            Self::IfIcmpgt(v) => write_branch(w, 0xa3, *v),
            Self::IfIcmple(v) => write_branch(w, 0xa4, *v),
            Self::IfAcmpeq(v) => write_branch(w, 0xa5, *v),
            Self::IfAcmpne(v) => write_branch(w, 0xa6, *v),
            Self::Goto(v) => write_branch(w, 0xa7, *v),
            Self::Jsr(v) => write_branch(w, 0xa8, *v),
            Self::Ret(v) => {
                w.u8(0xa9);
                w.u8(*v);
            }
            Self::TableSwitch {
                default,
                low,
                high,
                offsets,
            } => {
                w.u8(0xaa);
                write_switch_padding(w);
                w.u32(*default as u32);
                w.u32(*low as u32);
                w.u32(*high as u32);
                for off in offsets {
                    w.u32(*off as u32);
                }
            }
            Self::LookupSwitch { default, pairs } => {
                w.u8(0xab);
                write_switch_padding(w);
                w.u32(*default as u32);
                w.u32(pairs.len() as u32);
                for (key, off) in pairs {
                    w.u32(*key as u32);
                    w.u32(*off as u32);
                }
            }
            Self::Ireturn => w.u8(0xac),
            Self::Lreturn => w.u8(0xad),
            Self::Freturn => w.u8(0xae),
            Self::Dreturn => w.u8(0xaf),
            Self::Areturn => w.u8(0xb0),
            Self::Return => w.u8(0xb1),
            Self::GetStatic(v) => {
                w.u8(0xb2);
                w.u16(*v);
            }
            Self::PutStatic(v) => {
                w.u8(0xb3);
                w.u16(*v);
            }
            Self::GetField(v) => {
                w.u8(0xb4);
                w.u16(*v);
            }
            Self::PutField(v) => {
                w.u8(0xb5);
                w.u16(*v);
            }
            Self::InvokeVirtual(v) => {
                w.u8(0xb6);
                w.u16(*v);
            }
            Self::InvokeSpecial(v) => {
                w.u8(0xb7);
                w.u16(*v);
            }
            Self::InvokeStatic(v) => {
                w.u8(0xb8);
                w.u16(*v);
            }
            Self::InvokeInterface { index, count } => {
                w.u8(0xb9);
                w.u16(*index);
                w.u8(*count);
                w.u8(0);
            }
            Self::InvokeDynamic { index } => {
                w.u8(0xba);
                w.u16(*index);
                w.u16(0);
            }
            Self::New(v) => {
                w.u8(0xbb);
                w.u16(*v);
            }
            Self::NewArray(v) => {
                w.u8(0xbc);
                w.u8(*v);
            }
            Self::ANewArray(v) => {
                w.u8(0xbd);
                w.u16(*v);
            }
            Self::ArrayLength => w.u8(0xbe),
            Self::Athrow => w.u8(0xbf),
            Self::CheckCast(v) => {
                w.u8(0xc0);
                w.u16(*v);
            }
            Self::InstanceOf(v) => {
                w.u8(0xc1);
                w.u16(*v);
            }
            Self::MonitorEnter => w.u8(0xc2),
            Self::MonitorExit => w.u8(0xc3),
            Self::Wide(wide) => {
                w.u8(0xc4);
                wide.write(w);
            }
            Self::MultiANewArray { index, dimensions } => {
                w.u8(0xc5);
                w.u16(*index);
                w.u8(*dimensions);
            }
            Self::IfNull(v) => write_branch(w, 0xc6, *v),
            Self::IfNonNull(v) => write_branch(w, 0xc7, *v),
            Self::GotoW(v) => {
                w.u8(0xc8);
                w.u32(*v as u32);
            }
            Self::JsrW(v) => {
                w.u8(0xc9);
                w.u32(*v as u32);
            }
        }
    }

    /// The number of bytes this instruction occupies when written at code offset `pc` — exactly what
    /// [`write`](Self::write) emits. `pc` matters only for `tableswitch` / `lookupswitch`, whose
    /// alignment padding is relative to the instruction's position. Summing `encoded_len` across a
    /// `code` array reconstructs each instruction's byte offset, so branch offsets (stored relative to
    /// their instruction) can be resolved to targets.
    ///
    /// Measured from [`write`](Self::write) itself — the single source of truth for the encoding, so the
    /// two can never drift: a scratch encode is primed to `pc`'s 4-byte alignment (all a switch's
    /// padding depends on), and the length is how far past that priming `write` advances.
    pub fn encoded_len(&self, pc: usize) -> usize {
        let align = pc % 4;
        let mut w = Writer::new();
        for _ in 0..align {
            w.u8(0);
        }
        self.write(&mut w);
        w.len() - align
    }
}

impl WideInstruction {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        let opcode = r.u8()?;
        Ok(match opcode {
            0x15 => Self::Iload(r.u16()?),
            0x16 => Self::Lload(r.u16()?),
            0x17 => Self::Fload(r.u16()?),
            0x18 => Self::Dload(r.u16()?),
            0x19 => Self::Aload(r.u16()?),
            0x36 => Self::Istore(r.u16()?),
            0x37 => Self::Lstore(r.u16()?),
            0x38 => Self::Fstore(r.u16()?),
            0x39 => Self::Dstore(r.u16()?),
            0x3a => Self::Astore(r.u16()?),
            0xa9 => Self::Ret(r.u16()?),
            0x84 => Self::Iinc {
                index: r.u16()?,
                value: r.u16()? as i16,
            },
            other => return Err(ClassfileError::InvalidOpcode(other)),
        })
    }

    fn write(&self, w: &mut Writer) {
        match self {
            Self::Iload(v) => {
                w.u8(0x15);
                w.u16(*v);
            }
            Self::Lload(v) => {
                w.u8(0x16);
                w.u16(*v);
            }
            Self::Fload(v) => {
                w.u8(0x17);
                w.u16(*v);
            }
            Self::Dload(v) => {
                w.u8(0x18);
                w.u16(*v);
            }
            Self::Aload(v) => {
                w.u8(0x19);
                w.u16(*v);
            }
            Self::Istore(v) => {
                w.u8(0x36);
                w.u16(*v);
            }
            Self::Lstore(v) => {
                w.u8(0x37);
                w.u16(*v);
            }
            Self::Fstore(v) => {
                w.u8(0x38);
                w.u16(*v);
            }
            Self::Dstore(v) => {
                w.u8(0x39);
                w.u16(*v);
            }
            Self::Astore(v) => {
                w.u8(0x3a);
                w.u16(*v);
            }
            Self::Ret(v) => {
                w.u8(0xa9);
                w.u16(*v);
            }
            Self::Iinc { index, value } => {
                w.u8(0x84);
                w.u16(*index);
                w.u16(*value as u16);
            }
        }
    }
}

/// Branch instructions all share the `opcode` + `i16 offset` shape.
fn write_branch(w: &mut Writer, opcode: u8, offset: i16) {
    w.u8(opcode);
    w.u16(offset as u16);
}

/// Skip the 0–3 alignment-padding bytes after a `tableswitch` / `lookupswitch` opcode. The reader's
/// position is the code-array offset, so the padded position lands on a 4-byte boundary.
fn skip_switch_padding(r: &mut Reader<'_>) -> Result<()> {
    let pad = (4 - (r.pos() % 4)) % 4;
    r.bytes(pad)?;
    Ok(())
}

/// Emit the matching 0–3 padding bytes when writing a switch.
fn write_switch_padding(w: &mut Writer) {
    let pad = (4 - (w.len() % 4)) % 4;
    for _ in 0..pad {
        w.u8(0);
    }
}

#[cfg(test)]
mod tests {
    use super::{Instruction, WideInstruction, encode_code};
    use crate::bytes::Reader;

    /// `encoded_len(pc)` must report exactly the bytes `write`/`read` use at that offset — including
    /// the position-dependent `switch` padding and the `wide` forms.
    #[test]
    fn encoded_len_matches_the_written_encoding() {
        let code = vec![
            Instruction::Iconst0,
            Instruction::Bipush(7),
            Instruction::Sipush(300),
            Instruction::Iload(4),
            Instruction::Goto(3),
            Instruction::Iinc {
                index: 2,
                value: -1,
            },
            Instruction::InvokeInterface { index: 5, count: 2 },
            Instruction::InvokeDynamic { index: 6 },
            Instruction::MultiANewArray {
                index: 7,
                dimensions: 3,
            },
            Instruction::GotoW(9),
            Instruction::Wide(WideInstruction::Iload(300)),
            Instruction::Wide(WideInstruction::Iinc {
                index: 300,
                value: -5,
            }),
            // Placed at varying offsets so the 0–3 alignment padding is exercised.
            Instruction::TableSwitch {
                default: 10,
                low: 0,
                high: 2,
                offsets: vec![1, 2, 3],
            },
            Instruction::LookupSwitch {
                default: 4,
                pairs: vec![(1, 2), (3, 4)],
            },
            Instruction::Return,
        ];
        let bytes = encode_code(&code);
        let mut r = Reader::new(&bytes);
        let mut pc = 0usize;
        for ins in &code {
            assert_eq!(r.pos(), pc, "reader drifted before {ins:?}");
            let decoded = Instruction::read(&mut r).expect("decode");
            assert_eq!(&decoded, ins, "round-trip mismatch");
            pc += ins.encoded_len(pc);
            assert_eq!(r.pos(), pc, "encoded_len wrong for {ins:?}");
        }
        assert_eq!(pc, bytes.len());
    }
}
