//! JVM bytecode instructions (JVMS §6.5) and the `Code` array codec.
//!
//! [`decode_code`] turns a raw `code` byte array into a `Vec<Instruction>`; [`encode_code`] turns it
//! back. The two are exact inverses for valid bytecode: branch offsets are stored verbatim (never
//! recomputed), and the variable-length forms — `tableswitch` / `lookupswitch` 4-byte alignment
//! padding and the `wide` prefix — are reproduced byte-for-byte. An unknown opcode makes
//! `decode_code` fail, which degrades the whole `Code` attribute to `Unknown` (still byte-exact).

use serde::{Deserialize, Serialize};

use crate::bytes::{Reader, Writer};
use crate::error::{ClassfileError, Result};

/// A single decoded bytecode instruction. Operands carry constant-pool indices, local-variable slot
/// numbers, immediate constants, or branch offsets (relative to the instruction, as stored).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    fn read(r: &mut Reader<'_>) -> Result<Instruction> {
        let opcode = r.u8()?;
        Ok(match opcode {
            0x00 => Instruction::Nop,
            0x01 => Instruction::AconstNull,
            0x02 => Instruction::IconstM1,
            0x03 => Instruction::Iconst0,
            0x04 => Instruction::Iconst1,
            0x05 => Instruction::Iconst2,
            0x06 => Instruction::Iconst3,
            0x07 => Instruction::Iconst4,
            0x08 => Instruction::Iconst5,
            0x09 => Instruction::Lconst0,
            0x0a => Instruction::Lconst1,
            0x0b => Instruction::Fconst0,
            0x0c => Instruction::Fconst1,
            0x0d => Instruction::Fconst2,
            0x0e => Instruction::Dconst0,
            0x0f => Instruction::Dconst1,
            0x10 => Instruction::Bipush(r.u8()? as i8),
            0x11 => Instruction::Sipush(r.u16()? as i16),
            0x12 => Instruction::Ldc(r.u8()?),
            0x13 => Instruction::LdcW(r.u16()?),
            0x14 => Instruction::Ldc2W(r.u16()?),
            0x15 => Instruction::Iload(r.u8()?),
            0x16 => Instruction::Lload(r.u8()?),
            0x17 => Instruction::Fload(r.u8()?),
            0x18 => Instruction::Dload(r.u8()?),
            0x19 => Instruction::Aload(r.u8()?),
            0x1a => Instruction::Iload0,
            0x1b => Instruction::Iload1,
            0x1c => Instruction::Iload2,
            0x1d => Instruction::Iload3,
            0x1e => Instruction::Lload0,
            0x1f => Instruction::Lload1,
            0x20 => Instruction::Lload2,
            0x21 => Instruction::Lload3,
            0x22 => Instruction::Fload0,
            0x23 => Instruction::Fload1,
            0x24 => Instruction::Fload2,
            0x25 => Instruction::Fload3,
            0x26 => Instruction::Dload0,
            0x27 => Instruction::Dload1,
            0x28 => Instruction::Dload2,
            0x29 => Instruction::Dload3,
            0x2a => Instruction::Aload0,
            0x2b => Instruction::Aload1,
            0x2c => Instruction::Aload2,
            0x2d => Instruction::Aload3,
            0x2e => Instruction::Iaload,
            0x2f => Instruction::Laload,
            0x30 => Instruction::Faload,
            0x31 => Instruction::Daload,
            0x32 => Instruction::Aaload,
            0x33 => Instruction::Baload,
            0x34 => Instruction::Caload,
            0x35 => Instruction::Saload,
            0x36 => Instruction::Istore(r.u8()?),
            0x37 => Instruction::Lstore(r.u8()?),
            0x38 => Instruction::Fstore(r.u8()?),
            0x39 => Instruction::Dstore(r.u8()?),
            0x3a => Instruction::Astore(r.u8()?),
            0x3b => Instruction::Istore0,
            0x3c => Instruction::Istore1,
            0x3d => Instruction::Istore2,
            0x3e => Instruction::Istore3,
            0x3f => Instruction::Lstore0,
            0x40 => Instruction::Lstore1,
            0x41 => Instruction::Lstore2,
            0x42 => Instruction::Lstore3,
            0x43 => Instruction::Fstore0,
            0x44 => Instruction::Fstore1,
            0x45 => Instruction::Fstore2,
            0x46 => Instruction::Fstore3,
            0x47 => Instruction::Dstore0,
            0x48 => Instruction::Dstore1,
            0x49 => Instruction::Dstore2,
            0x4a => Instruction::Dstore3,
            0x4b => Instruction::Astore0,
            0x4c => Instruction::Astore1,
            0x4d => Instruction::Astore2,
            0x4e => Instruction::Astore3,
            0x4f => Instruction::Iastore,
            0x50 => Instruction::Lastore,
            0x51 => Instruction::Fastore,
            0x52 => Instruction::Dastore,
            0x53 => Instruction::Aastore,
            0x54 => Instruction::Bastore,
            0x55 => Instruction::Castore,
            0x56 => Instruction::Sastore,
            0x57 => Instruction::Pop,
            0x58 => Instruction::Pop2,
            0x59 => Instruction::Dup,
            0x5a => Instruction::DupX1,
            0x5b => Instruction::DupX2,
            0x5c => Instruction::Dup2,
            0x5d => Instruction::Dup2X1,
            0x5e => Instruction::Dup2X2,
            0x5f => Instruction::Swap,
            0x60 => Instruction::Iadd,
            0x61 => Instruction::Ladd,
            0x62 => Instruction::Fadd,
            0x63 => Instruction::Dadd,
            0x64 => Instruction::Isub,
            0x65 => Instruction::Lsub,
            0x66 => Instruction::Fsub,
            0x67 => Instruction::Dsub,
            0x68 => Instruction::Imul,
            0x69 => Instruction::Lmul,
            0x6a => Instruction::Fmul,
            0x6b => Instruction::Dmul,
            0x6c => Instruction::Idiv,
            0x6d => Instruction::Ldiv,
            0x6e => Instruction::Fdiv,
            0x6f => Instruction::Ddiv,
            0x70 => Instruction::Irem,
            0x71 => Instruction::Lrem,
            0x72 => Instruction::Frem,
            0x73 => Instruction::Drem,
            0x74 => Instruction::Ineg,
            0x75 => Instruction::Lneg,
            0x76 => Instruction::Fneg,
            0x77 => Instruction::Dneg,
            0x78 => Instruction::Ishl,
            0x79 => Instruction::Lshl,
            0x7a => Instruction::Ishr,
            0x7b => Instruction::Lshr,
            0x7c => Instruction::Iushr,
            0x7d => Instruction::Lushr,
            0x7e => Instruction::Iand,
            0x7f => Instruction::Land,
            0x80 => Instruction::Ior,
            0x81 => Instruction::Lor,
            0x82 => Instruction::Ixor,
            0x83 => Instruction::Lxor,
            0x84 => Instruction::Iinc {
                index: r.u8()?,
                value: r.u8()? as i8,
            },
            0x85 => Instruction::I2l,
            0x86 => Instruction::I2f,
            0x87 => Instruction::I2d,
            0x88 => Instruction::L2i,
            0x89 => Instruction::L2f,
            0x8a => Instruction::L2d,
            0x8b => Instruction::F2i,
            0x8c => Instruction::F2l,
            0x8d => Instruction::F2d,
            0x8e => Instruction::D2i,
            0x8f => Instruction::D2l,
            0x90 => Instruction::D2f,
            0x91 => Instruction::I2b,
            0x92 => Instruction::I2c,
            0x93 => Instruction::I2s,
            0x94 => Instruction::Lcmp,
            0x95 => Instruction::Fcmpl,
            0x96 => Instruction::Fcmpg,
            0x97 => Instruction::Dcmpl,
            0x98 => Instruction::Dcmpg,
            0x99 => Instruction::Ifeq(r.u16()? as i16),
            0x9a => Instruction::Ifne(r.u16()? as i16),
            0x9b => Instruction::Iflt(r.u16()? as i16),
            0x9c => Instruction::Ifge(r.u16()? as i16),
            0x9d => Instruction::Ifgt(r.u16()? as i16),
            0x9e => Instruction::Ifle(r.u16()? as i16),
            0x9f => Instruction::IfIcmpeq(r.u16()? as i16),
            0xa0 => Instruction::IfIcmpne(r.u16()? as i16),
            0xa1 => Instruction::IfIcmplt(r.u16()? as i16),
            0xa2 => Instruction::IfIcmpge(r.u16()? as i16),
            0xa3 => Instruction::IfIcmpgt(r.u16()? as i16),
            0xa4 => Instruction::IfIcmple(r.u16()? as i16),
            0xa5 => Instruction::IfAcmpeq(r.u16()? as i16),
            0xa6 => Instruction::IfAcmpne(r.u16()? as i16),
            0xa7 => Instruction::Goto(r.u16()? as i16),
            0xa8 => Instruction::Jsr(r.u16()? as i16),
            0xa9 => Instruction::Ret(r.u8()?),
            0xaa => Instruction::read_table_switch(r)?,
            0xab => Instruction::read_lookup_switch(r)?,
            0xac => Instruction::Ireturn,
            0xad => Instruction::Lreturn,
            0xae => Instruction::Freturn,
            0xaf => Instruction::Dreturn,
            0xb0 => Instruction::Areturn,
            0xb1 => Instruction::Return,
            0xb2 => Instruction::GetStatic(r.u16()?),
            0xb3 => Instruction::PutStatic(r.u16()?),
            0xb4 => Instruction::GetField(r.u16()?),
            0xb5 => Instruction::PutField(r.u16()?),
            0xb6 => Instruction::InvokeVirtual(r.u16()?),
            0xb7 => Instruction::InvokeSpecial(r.u16()?),
            0xb8 => Instruction::InvokeStatic(r.u16()?),
            0xb9 => {
                let index = r.u16()?;
                let count = r.u8()?;
                let _zero = r.u8()?;
                Instruction::InvokeInterface { index, count }
            }
            0xba => {
                let index = r.u16()?;
                let _zero = r.u16()?;
                Instruction::InvokeDynamic { index }
            }
            0xbb => Instruction::New(r.u16()?),
            0xbc => Instruction::NewArray(r.u8()?),
            0xbd => Instruction::ANewArray(r.u16()?),
            0xbe => Instruction::ArrayLength,
            0xbf => Instruction::Athrow,
            0xc0 => Instruction::CheckCast(r.u16()?),
            0xc1 => Instruction::InstanceOf(r.u16()?),
            0xc2 => Instruction::MonitorEnter,
            0xc3 => Instruction::MonitorExit,
            0xc4 => Instruction::Wide(WideInstruction::read(r)?),
            0xc5 => Instruction::MultiANewArray {
                index: r.u16()?,
                dimensions: r.u8()?,
            },
            0xc6 => Instruction::IfNull(r.u16()? as i16),
            0xc7 => Instruction::IfNonNull(r.u16()? as i16),
            0xc8 => Instruction::GotoW(r.u32()? as i32),
            0xc9 => Instruction::JsrW(r.u32()? as i32),
            other => return Err(ClassfileError::InvalidOpcode(other)),
        })
    }

    fn read_table_switch(r: &mut Reader<'_>) -> Result<Instruction> {
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
        Ok(Instruction::TableSwitch {
            default,
            low,
            high,
            offsets,
        })
    }

    fn read_lookup_switch(r: &mut Reader<'_>) -> Result<Instruction> {
        skip_switch_padding(r)?;
        let default = r.u32()? as i32;
        let npairs = r.u32()?;
        let mut pairs = Vec::with_capacity(npairs as usize);
        for _ in 0..npairs {
            let key = r.u32()? as i32;
            let offset = r.u32()? as i32;
            pairs.push((key, offset));
        }
        Ok(Instruction::LookupSwitch { default, pairs })
    }

    fn write(&self, w: &mut Writer) {
        match self {
            Instruction::Nop => w.u8(0x00),
            Instruction::AconstNull => w.u8(0x01),
            Instruction::IconstM1 => w.u8(0x02),
            Instruction::Iconst0 => w.u8(0x03),
            Instruction::Iconst1 => w.u8(0x04),
            Instruction::Iconst2 => w.u8(0x05),
            Instruction::Iconst3 => w.u8(0x06),
            Instruction::Iconst4 => w.u8(0x07),
            Instruction::Iconst5 => w.u8(0x08),
            Instruction::Lconst0 => w.u8(0x09),
            Instruction::Lconst1 => w.u8(0x0a),
            Instruction::Fconst0 => w.u8(0x0b),
            Instruction::Fconst1 => w.u8(0x0c),
            Instruction::Fconst2 => w.u8(0x0d),
            Instruction::Dconst0 => w.u8(0x0e),
            Instruction::Dconst1 => w.u8(0x0f),
            Instruction::Bipush(v) => {
                w.u8(0x10);
                w.u8(*v as u8);
            }
            Instruction::Sipush(v) => {
                w.u8(0x11);
                w.u16(*v as u16);
            }
            Instruction::Ldc(v) => {
                w.u8(0x12);
                w.u8(*v);
            }
            Instruction::LdcW(v) => {
                w.u8(0x13);
                w.u16(*v);
            }
            Instruction::Ldc2W(v) => {
                w.u8(0x14);
                w.u16(*v);
            }
            Instruction::Iload(v) => {
                w.u8(0x15);
                w.u8(*v);
            }
            Instruction::Lload(v) => {
                w.u8(0x16);
                w.u8(*v);
            }
            Instruction::Fload(v) => {
                w.u8(0x17);
                w.u8(*v);
            }
            Instruction::Dload(v) => {
                w.u8(0x18);
                w.u8(*v);
            }
            Instruction::Aload(v) => {
                w.u8(0x19);
                w.u8(*v);
            }
            Instruction::Iload0 => w.u8(0x1a),
            Instruction::Iload1 => w.u8(0x1b),
            Instruction::Iload2 => w.u8(0x1c),
            Instruction::Iload3 => w.u8(0x1d),
            Instruction::Lload0 => w.u8(0x1e),
            Instruction::Lload1 => w.u8(0x1f),
            Instruction::Lload2 => w.u8(0x20),
            Instruction::Lload3 => w.u8(0x21),
            Instruction::Fload0 => w.u8(0x22),
            Instruction::Fload1 => w.u8(0x23),
            Instruction::Fload2 => w.u8(0x24),
            Instruction::Fload3 => w.u8(0x25),
            Instruction::Dload0 => w.u8(0x26),
            Instruction::Dload1 => w.u8(0x27),
            Instruction::Dload2 => w.u8(0x28),
            Instruction::Dload3 => w.u8(0x29),
            Instruction::Aload0 => w.u8(0x2a),
            Instruction::Aload1 => w.u8(0x2b),
            Instruction::Aload2 => w.u8(0x2c),
            Instruction::Aload3 => w.u8(0x2d),
            Instruction::Iaload => w.u8(0x2e),
            Instruction::Laload => w.u8(0x2f),
            Instruction::Faload => w.u8(0x30),
            Instruction::Daload => w.u8(0x31),
            Instruction::Aaload => w.u8(0x32),
            Instruction::Baload => w.u8(0x33),
            Instruction::Caload => w.u8(0x34),
            Instruction::Saload => w.u8(0x35),
            Instruction::Istore(v) => {
                w.u8(0x36);
                w.u8(*v);
            }
            Instruction::Lstore(v) => {
                w.u8(0x37);
                w.u8(*v);
            }
            Instruction::Fstore(v) => {
                w.u8(0x38);
                w.u8(*v);
            }
            Instruction::Dstore(v) => {
                w.u8(0x39);
                w.u8(*v);
            }
            Instruction::Astore(v) => {
                w.u8(0x3a);
                w.u8(*v);
            }
            Instruction::Istore0 => w.u8(0x3b),
            Instruction::Istore1 => w.u8(0x3c),
            Instruction::Istore2 => w.u8(0x3d),
            Instruction::Istore3 => w.u8(0x3e),
            Instruction::Lstore0 => w.u8(0x3f),
            Instruction::Lstore1 => w.u8(0x40),
            Instruction::Lstore2 => w.u8(0x41),
            Instruction::Lstore3 => w.u8(0x42),
            Instruction::Fstore0 => w.u8(0x43),
            Instruction::Fstore1 => w.u8(0x44),
            Instruction::Fstore2 => w.u8(0x45),
            Instruction::Fstore3 => w.u8(0x46),
            Instruction::Dstore0 => w.u8(0x47),
            Instruction::Dstore1 => w.u8(0x48),
            Instruction::Dstore2 => w.u8(0x49),
            Instruction::Dstore3 => w.u8(0x4a),
            Instruction::Astore0 => w.u8(0x4b),
            Instruction::Astore1 => w.u8(0x4c),
            Instruction::Astore2 => w.u8(0x4d),
            Instruction::Astore3 => w.u8(0x4e),
            Instruction::Iastore => w.u8(0x4f),
            Instruction::Lastore => w.u8(0x50),
            Instruction::Fastore => w.u8(0x51),
            Instruction::Dastore => w.u8(0x52),
            Instruction::Aastore => w.u8(0x53),
            Instruction::Bastore => w.u8(0x54),
            Instruction::Castore => w.u8(0x55),
            Instruction::Sastore => w.u8(0x56),
            Instruction::Pop => w.u8(0x57),
            Instruction::Pop2 => w.u8(0x58),
            Instruction::Dup => w.u8(0x59),
            Instruction::DupX1 => w.u8(0x5a),
            Instruction::DupX2 => w.u8(0x5b),
            Instruction::Dup2 => w.u8(0x5c),
            Instruction::Dup2X1 => w.u8(0x5d),
            Instruction::Dup2X2 => w.u8(0x5e),
            Instruction::Swap => w.u8(0x5f),
            Instruction::Iadd => w.u8(0x60),
            Instruction::Ladd => w.u8(0x61),
            Instruction::Fadd => w.u8(0x62),
            Instruction::Dadd => w.u8(0x63),
            Instruction::Isub => w.u8(0x64),
            Instruction::Lsub => w.u8(0x65),
            Instruction::Fsub => w.u8(0x66),
            Instruction::Dsub => w.u8(0x67),
            Instruction::Imul => w.u8(0x68),
            Instruction::Lmul => w.u8(0x69),
            Instruction::Fmul => w.u8(0x6a),
            Instruction::Dmul => w.u8(0x6b),
            Instruction::Idiv => w.u8(0x6c),
            Instruction::Ldiv => w.u8(0x6d),
            Instruction::Fdiv => w.u8(0x6e),
            Instruction::Ddiv => w.u8(0x6f),
            Instruction::Irem => w.u8(0x70),
            Instruction::Lrem => w.u8(0x71),
            Instruction::Frem => w.u8(0x72),
            Instruction::Drem => w.u8(0x73),
            Instruction::Ineg => w.u8(0x74),
            Instruction::Lneg => w.u8(0x75),
            Instruction::Fneg => w.u8(0x76),
            Instruction::Dneg => w.u8(0x77),
            Instruction::Ishl => w.u8(0x78),
            Instruction::Lshl => w.u8(0x79),
            Instruction::Ishr => w.u8(0x7a),
            Instruction::Lshr => w.u8(0x7b),
            Instruction::Iushr => w.u8(0x7c),
            Instruction::Lushr => w.u8(0x7d),
            Instruction::Iand => w.u8(0x7e),
            Instruction::Land => w.u8(0x7f),
            Instruction::Ior => w.u8(0x80),
            Instruction::Lor => w.u8(0x81),
            Instruction::Ixor => w.u8(0x82),
            Instruction::Lxor => w.u8(0x83),
            Instruction::Iinc { index, value } => {
                w.u8(0x84);
                w.u8(*index);
                w.u8(*value as u8);
            }
            Instruction::I2l => w.u8(0x85),
            Instruction::I2f => w.u8(0x86),
            Instruction::I2d => w.u8(0x87),
            Instruction::L2i => w.u8(0x88),
            Instruction::L2f => w.u8(0x89),
            Instruction::L2d => w.u8(0x8a),
            Instruction::F2i => w.u8(0x8b),
            Instruction::F2l => w.u8(0x8c),
            Instruction::F2d => w.u8(0x8d),
            Instruction::D2i => w.u8(0x8e),
            Instruction::D2l => w.u8(0x8f),
            Instruction::D2f => w.u8(0x90),
            Instruction::I2b => w.u8(0x91),
            Instruction::I2c => w.u8(0x92),
            Instruction::I2s => w.u8(0x93),
            Instruction::Lcmp => w.u8(0x94),
            Instruction::Fcmpl => w.u8(0x95),
            Instruction::Fcmpg => w.u8(0x96),
            Instruction::Dcmpl => w.u8(0x97),
            Instruction::Dcmpg => w.u8(0x98),
            Instruction::Ifeq(v) => write_branch(w, 0x99, *v),
            Instruction::Ifne(v) => write_branch(w, 0x9a, *v),
            Instruction::Iflt(v) => write_branch(w, 0x9b, *v),
            Instruction::Ifge(v) => write_branch(w, 0x9c, *v),
            Instruction::Ifgt(v) => write_branch(w, 0x9d, *v),
            Instruction::Ifle(v) => write_branch(w, 0x9e, *v),
            Instruction::IfIcmpeq(v) => write_branch(w, 0x9f, *v),
            Instruction::IfIcmpne(v) => write_branch(w, 0xa0, *v),
            Instruction::IfIcmplt(v) => write_branch(w, 0xa1, *v),
            Instruction::IfIcmpge(v) => write_branch(w, 0xa2, *v),
            Instruction::IfIcmpgt(v) => write_branch(w, 0xa3, *v),
            Instruction::IfIcmple(v) => write_branch(w, 0xa4, *v),
            Instruction::IfAcmpeq(v) => write_branch(w, 0xa5, *v),
            Instruction::IfAcmpne(v) => write_branch(w, 0xa6, *v),
            Instruction::Goto(v) => write_branch(w, 0xa7, *v),
            Instruction::Jsr(v) => write_branch(w, 0xa8, *v),
            Instruction::Ret(v) => {
                w.u8(0xa9);
                w.u8(*v);
            }
            Instruction::TableSwitch {
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
            Instruction::LookupSwitch { default, pairs } => {
                w.u8(0xab);
                write_switch_padding(w);
                w.u32(*default as u32);
                w.u32(pairs.len() as u32);
                for (key, off) in pairs {
                    w.u32(*key as u32);
                    w.u32(*off as u32);
                }
            }
            Instruction::Ireturn => w.u8(0xac),
            Instruction::Lreturn => w.u8(0xad),
            Instruction::Freturn => w.u8(0xae),
            Instruction::Dreturn => w.u8(0xaf),
            Instruction::Areturn => w.u8(0xb0),
            Instruction::Return => w.u8(0xb1),
            Instruction::GetStatic(v) => {
                w.u8(0xb2);
                w.u16(*v);
            }
            Instruction::PutStatic(v) => {
                w.u8(0xb3);
                w.u16(*v);
            }
            Instruction::GetField(v) => {
                w.u8(0xb4);
                w.u16(*v);
            }
            Instruction::PutField(v) => {
                w.u8(0xb5);
                w.u16(*v);
            }
            Instruction::InvokeVirtual(v) => {
                w.u8(0xb6);
                w.u16(*v);
            }
            Instruction::InvokeSpecial(v) => {
                w.u8(0xb7);
                w.u16(*v);
            }
            Instruction::InvokeStatic(v) => {
                w.u8(0xb8);
                w.u16(*v);
            }
            Instruction::InvokeInterface { index, count } => {
                w.u8(0xb9);
                w.u16(*index);
                w.u8(*count);
                w.u8(0);
            }
            Instruction::InvokeDynamic { index } => {
                w.u8(0xba);
                w.u16(*index);
                w.u16(0);
            }
            Instruction::New(v) => {
                w.u8(0xbb);
                w.u16(*v);
            }
            Instruction::NewArray(v) => {
                w.u8(0xbc);
                w.u8(*v);
            }
            Instruction::ANewArray(v) => {
                w.u8(0xbd);
                w.u16(*v);
            }
            Instruction::ArrayLength => w.u8(0xbe),
            Instruction::Athrow => w.u8(0xbf),
            Instruction::CheckCast(v) => {
                w.u8(0xc0);
                w.u16(*v);
            }
            Instruction::InstanceOf(v) => {
                w.u8(0xc1);
                w.u16(*v);
            }
            Instruction::MonitorEnter => w.u8(0xc2),
            Instruction::MonitorExit => w.u8(0xc3),
            Instruction::Wide(wide) => {
                w.u8(0xc4);
                wide.write(w);
            }
            Instruction::MultiANewArray { index, dimensions } => {
                w.u8(0xc5);
                w.u16(*index);
                w.u8(*dimensions);
            }
            Instruction::IfNull(v) => write_branch(w, 0xc6, *v),
            Instruction::IfNonNull(v) => write_branch(w, 0xc7, *v),
            Instruction::GotoW(v) => {
                w.u8(0xc8);
                w.u32(*v as u32);
            }
            Instruction::JsrW(v) => {
                w.u8(0xc9);
                w.u32(*v as u32);
            }
        }
    }
}

impl WideInstruction {
    fn read(r: &mut Reader<'_>) -> Result<WideInstruction> {
        let opcode = r.u8()?;
        Ok(match opcode {
            0x15 => WideInstruction::Iload(r.u16()?),
            0x16 => WideInstruction::Lload(r.u16()?),
            0x17 => WideInstruction::Fload(r.u16()?),
            0x18 => WideInstruction::Dload(r.u16()?),
            0x19 => WideInstruction::Aload(r.u16()?),
            0x36 => WideInstruction::Istore(r.u16()?),
            0x37 => WideInstruction::Lstore(r.u16()?),
            0x38 => WideInstruction::Fstore(r.u16()?),
            0x39 => WideInstruction::Dstore(r.u16()?),
            0x3a => WideInstruction::Astore(r.u16()?),
            0xa9 => WideInstruction::Ret(r.u16()?),
            0x84 => WideInstruction::Iinc {
                index: r.u16()?,
                value: r.u16()? as i16,
            },
            other => return Err(ClassfileError::InvalidOpcode(other)),
        })
    }

    fn write(&self, w: &mut Writer) {
        match self {
            WideInstruction::Iload(v) => {
                w.u8(0x15);
                w.u16(*v);
            }
            WideInstruction::Lload(v) => {
                w.u8(0x16);
                w.u16(*v);
            }
            WideInstruction::Fload(v) => {
                w.u8(0x17);
                w.u16(*v);
            }
            WideInstruction::Dload(v) => {
                w.u8(0x18);
                w.u16(*v);
            }
            WideInstruction::Aload(v) => {
                w.u8(0x19);
                w.u16(*v);
            }
            WideInstruction::Istore(v) => {
                w.u8(0x36);
                w.u16(*v);
            }
            WideInstruction::Lstore(v) => {
                w.u8(0x37);
                w.u16(*v);
            }
            WideInstruction::Fstore(v) => {
                w.u8(0x38);
                w.u16(*v);
            }
            WideInstruction::Dstore(v) => {
                w.u8(0x39);
                w.u16(*v);
            }
            WideInstruction::Astore(v) => {
                w.u8(0x3a);
                w.u16(*v);
            }
            WideInstruction::Ret(v) => {
                w.u8(0xa9);
                w.u16(*v);
            }
            WideInstruction::Iinc { index, value } => {
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
