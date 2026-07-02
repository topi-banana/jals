//! The `StackMapTable` attribute's frames (JVMS §4.7.4).
//!
//! Several frame kinds encode their `offset_delta` (or a local count) *in the `frame_type` byte*
//! itself. Each kind is a distinct [`StackMapFrame`] variant, so the exact `frame_type` byte is
//! reconstructed on write and the attribute round-trips byte-for-byte.

use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::bytes::{Reader, Writer};
use crate::error::{ClassfileError, Result};

/// One `stack_map_frame` (JVMS §4.7.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StackMapFrame {
    /// `same_frame` (frame_type 0–63): same locals, empty stack; the type *is* the delta.
    Same {
        /// Bytecode offset delta (0–63).
        offset_delta: u16,
    },
    /// `same_locals_1_stack_item_frame` (64–127): same locals, one stack item; delta is `type − 64`.
    SameLocals1StackItem {
        /// Bytecode offset delta (0–63).
        offset_delta: u16,
        /// The single stack item.
        stack: VerificationType,
    },
    /// `same_locals_1_stack_item_frame_extended` (247).
    SameLocals1StackItemExtended {
        /// Bytecode offset delta.
        offset_delta: u16,
        /// The single stack item.
        stack: VerificationType,
    },
    /// `chop_frame` (248–250): the last `count` (1–3) locals are removed.
    Chop {
        /// Number of locals chopped (1–3); `frame_type` is `251 − count`.
        count: u8,
        /// Bytecode offset delta.
        offset_delta: u16,
    },
    /// `same_frame_extended` (251).
    SameFrameExtended {
        /// Bytecode offset delta.
        offset_delta: u16,
    },
    /// `append_frame` (252–254): `locals.len()` (1–3) locals are appended.
    Append {
        /// Bytecode offset delta.
        offset_delta: u16,
        /// The appended locals (1–3); `frame_type` is `251 + locals.len()`.
        locals: Vec<VerificationType>,
    },
    /// `full_frame` (255): the complete local and stack maps.
    Full {
        /// Bytecode offset delta.
        offset_delta: u16,
        /// The full local-variable map.
        locals: Vec<VerificationType>,
        /// The full operand-stack map.
        stack: Vec<VerificationType>,
    },
}

/// A `verification_type_info` (JVMS §4.7.4): the abstract type of a local or stack slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VerificationType {
    /// `ITEM_Top` (0).
    Top,
    /// `ITEM_Integer` (1).
    Integer,
    /// `ITEM_Float` (2).
    Float,
    /// `ITEM_Double` (3).
    Double,
    /// `ITEM_Long` (4).
    Long,
    /// `ITEM_Null` (5).
    Null,
    /// `ITEM_UninitializedThis` (6).
    UninitializedThis,
    /// `ITEM_Object` (7): an initialised reference of a known class.
    Object {
        /// `Class` constant-pool index.
        cpool_index: u16,
    },
    /// `ITEM_Uninitialized` (8): a reference created by a `new` not yet run through its constructor.
    Uninitialized {
        /// Bytecode offset of the `new` instruction.
        offset: u16,
    },
}

impl StackMapFrame {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<StackMapFrame> {
        let frame_type = r.u8()?;
        Ok(match frame_type {
            0..=63 => StackMapFrame::Same {
                offset_delta: u16::from(frame_type),
            },
            64..=127 => StackMapFrame::SameLocals1StackItem {
                offset_delta: u16::from(frame_type - 64),
                stack: VerificationType::read(r)?,
            },
            247 => StackMapFrame::SameLocals1StackItemExtended {
                offset_delta: r.u16()?,
                stack: VerificationType::read(r)?,
            },
            248..=250 => StackMapFrame::Chop {
                count: 251 - frame_type,
                offset_delta: r.u16()?,
            },
            251 => StackMapFrame::SameFrameExtended {
                offset_delta: r.u16()?,
            },
            252..=254 => {
                let count = frame_type - 251;
                let offset_delta = r.u16()?;
                let mut locals = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    locals.push(VerificationType::read(r)?);
                }
                StackMapFrame::Append {
                    offset_delta,
                    locals,
                }
            }
            255 => {
                let offset_delta = r.u16()?;
                let locals = read_verification_list(r)?;
                let stack = read_verification_list(r)?;
                StackMapFrame::Full {
                    offset_delta,
                    locals,
                    stack,
                }
            }
            _ => return Err(ClassfileError::Malformed("stack_map_frame type")),
        })
    }

    pub(crate) fn write(&self, w: &mut Writer) {
        match self {
            StackMapFrame::Same { offset_delta } => w.u8(*offset_delta as u8),
            StackMapFrame::SameLocals1StackItem {
                offset_delta,
                stack,
            } => {
                w.u8(64 + *offset_delta as u8);
                stack.write(w);
            }
            StackMapFrame::SameLocals1StackItemExtended {
                offset_delta,
                stack,
            } => {
                w.u8(247);
                w.u16(*offset_delta);
                stack.write(w);
            }
            StackMapFrame::Chop {
                count,
                offset_delta,
            } => {
                w.u8(251 - *count);
                w.u16(*offset_delta);
            }
            StackMapFrame::SameFrameExtended { offset_delta } => {
                w.u8(251);
                w.u16(*offset_delta);
            }
            StackMapFrame::Append {
                offset_delta,
                locals,
            } => {
                w.u8(251 + locals.len() as u8);
                w.u16(*offset_delta);
                for l in locals {
                    l.write(w);
                }
            }
            StackMapFrame::Full {
                offset_delta,
                locals,
                stack,
            } => {
                w.u8(255);
                w.u16(*offset_delta);
                write_verification_list(locals, w);
                write_verification_list(stack, w);
            }
        }
    }
}

impl VerificationType {
    fn read(r: &mut Reader<'_>) -> Result<VerificationType> {
        let tag = r.u8()?;
        Ok(match tag {
            0 => VerificationType::Top,
            1 => VerificationType::Integer,
            2 => VerificationType::Float,
            3 => VerificationType::Double,
            4 => VerificationType::Long,
            5 => VerificationType::Null,
            6 => VerificationType::UninitializedThis,
            7 => VerificationType::Object {
                cpool_index: r.u16()?,
            },
            8 => VerificationType::Uninitialized { offset: r.u16()? },
            _ => return Err(ClassfileError::Malformed("verification_type_info tag")),
        })
    }

    fn write(&self, w: &mut Writer) {
        match self {
            VerificationType::Top => w.u8(0),
            VerificationType::Integer => w.u8(1),
            VerificationType::Float => w.u8(2),
            VerificationType::Double => w.u8(3),
            VerificationType::Long => w.u8(4),
            VerificationType::Null => w.u8(5),
            VerificationType::UninitializedThis => w.u8(6),
            VerificationType::Object { cpool_index } => {
                w.u8(7);
                w.u16(*cpool_index);
            }
            VerificationType::Uninitialized { offset } => {
                w.u8(8);
                w.u16(*offset);
            }
        }
    }
}

fn read_verification_list(r: &mut Reader<'_>) -> Result<Vec<VerificationType>> {
    let count = r.u16()?;
    let mut v = Vec::with_capacity(count as usize);
    for _ in 0..count {
        v.push(VerificationType::read(r)?);
    }
    Ok(v)
}

fn write_verification_list(items: &[VerificationType], w: &mut Writer) {
    w.u16(items.len() as u16);
    for item in items {
        item.write(w);
    }
}
