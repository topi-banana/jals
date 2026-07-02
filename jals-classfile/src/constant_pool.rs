//! The constant pool (JVMS §4.4): every `cp_info` tag, plus the 1-based / two-slot indexing quirk.
//!
//! `Long` and `Double` entries each occupy *two* pool slots; the slot after them is unusable. To let
//! any raw `u16` index from the file index the pool directly, the pool is stored 1-based with a
//! leading [`Sentinel`](ConstantSlot::Sentinel) at index 0 and a [`Gap`](ConstantSlot::Gap)
//! immediately after every `Long`/`Double`. Consumers never do offset arithmetic — they index
//! straight in.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::bytes::{Reader, Writer};
use crate::error::{ClassfileError, Result};

/// A class file's constant pool. Indices are 1-based, matching the on-disk encoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstantPool {
    entries: Vec<ConstantSlot>,
}

/// One slot of the pool. `Long`/`Double` entries are followed by a [`Gap`](ConstantSlot::Gap) so
/// indices stay aligned with the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum ConstantSlot {
    /// Index 0, which the JVM never uses.
    Sentinel,
    /// A real entry.
    Entry(ConstantPoolEntry),
    /// The unusable second slot of a preceding `Long`/`Double`.
    Gap,
}

/// A single constant-pool entry (JVMS Table 4.4-A). Reference entries store raw `u16` pool indices
/// rather than resolved pointers, so the pool serialises verbatim and round-trips byte-for-byte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConstantPoolEntry {
    /// Modified-UTF8 string data (tag 1), kept as raw bytes for exact round-trip. Decode with
    /// [`ConstantPool::utf8`].
    Utf8(Vec<u8>),
    /// `int` constant (tag 3).
    Integer(i32),
    /// `float` constant (tag 4).
    Float(f32),
    /// `long` constant (tag 5); occupies two slots.
    Long(i64),
    /// `double` constant (tag 6); occupies two slots.
    Double(f64),
    /// A class or interface reference (tag 7): index of its internal-form name `Utf8`.
    Class {
        /// `Utf8` index of the internal binary name (`a/b/C`).
        name_index: u16,
    },
    /// A string literal (tag 8).
    String {
        /// `Utf8` index of the literal's text.
        string_index: u16,
    },
    /// A field reference (tag 9).
    FieldRef {
        /// `Class` index of the owner.
        class_index: u16,
        /// `NameAndType` index.
        name_and_type_index: u16,
    },
    /// A method reference (tag 10).
    MethodRef {
        /// `Class` index of the owner.
        class_index: u16,
        /// `NameAndType` index.
        name_and_type_index: u16,
    },
    /// An interface-method reference (tag 11).
    InterfaceMethodRef {
        /// `Class` index of the owner interface.
        class_index: u16,
        /// `NameAndType` index.
        name_and_type_index: u16,
    },
    /// A name-and-descriptor pair (tag 12).
    NameAndType {
        /// `Utf8` index of the simple name.
        name_index: u16,
        /// `Utf8` index of the descriptor.
        descriptor_index: u16,
    },
    /// A method handle (tag 15).
    MethodHandle {
        /// The kind of reference (JVMS Table 5.4.3.5-A).
        reference_kind: u8,
        /// Index of the referenced entry.
        reference_index: u16,
    },
    /// A method type (tag 16).
    MethodType {
        /// `Utf8` index of the method descriptor.
        descriptor_index: u16,
    },
    /// A dynamically-computed constant (tag 17).
    Dynamic {
        /// Index into the `BootstrapMethods` attribute.
        bootstrap_method_attr_index: u16,
        /// `NameAndType` index.
        name_and_type_index: u16,
    },
    /// An `invokedynamic` call site (tag 18).
    InvokeDynamic {
        /// Index into the `BootstrapMethods` attribute.
        bootstrap_method_attr_index: u16,
        /// `NameAndType` index.
        name_and_type_index: u16,
    },
    /// A module (tag 19).
    Module {
        /// `Utf8` index of the module name.
        name_index: u16,
    },
    /// A package (tag 20).
    Package {
        /// `Utf8` index of the package name (internal form).
        name_index: u16,
    },
}

impl ConstantPool {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<ConstantPool> {
        let count = r.u16()?;
        let mut entries = Vec::with_capacity(count as usize);
        entries.push(ConstantSlot::Sentinel);
        let mut i = 1u16;
        while i < count {
            let entry = ConstantPoolEntry::read(r)?;
            let wide = matches!(
                entry,
                ConstantPoolEntry::Long(_) | ConstantPoolEntry::Double(_)
            );
            entries.push(ConstantSlot::Entry(entry));
            if wide {
                entries.push(ConstantSlot::Gap);
                i += 2;
            } else {
                i += 1;
            }
        }
        Ok(ConstantPool { entries })
    }

    pub(crate) fn write(&self, w: &mut Writer) {
        w.u16(self.entries.len() as u16);
        for slot in &self.entries {
            if let ConstantSlot::Entry(e) = slot {
                e.write(w);
            }
        }
    }

    /// The entry at a 1-based pool `index`, or `None` for index 0, a `Gap` slot, or out of range.
    pub fn get(&self, index: u16) -> Option<&ConstantPoolEntry> {
        match self.entries.get(index as usize) {
            Some(ConstantSlot::Entry(e)) => Some(e),
            _ => None,
        }
    }

    /// The decoded text of a `Utf8` entry at `index`, or `None` if it is not a `Utf8`.
    pub fn utf8(&self, index: u16) -> Option<Cow<'_, str>> {
        match self.get(index) {
            Some(ConstantPoolEntry::Utf8(bytes)) => Some(decode_modified_utf8(bytes)),
            _ => None,
        }
    }

    /// The internal-form name (`a/b/C`) a `Class` entry at `index` points to.
    pub fn class_name(&self, index: u16) -> Option<Cow<'_, str>> {
        match self.get(index) {
            Some(ConstantPoolEntry::Class { name_index }) => self.utf8(*name_index),
            _ => None,
        }
    }
}

impl ConstantPoolEntry {
    fn read(r: &mut Reader<'_>) -> Result<ConstantPoolEntry> {
        let tag = r.u8()?;
        Ok(match tag {
            1 => {
                let len = r.u16()? as usize;
                ConstantPoolEntry::Utf8(r.bytes(len)?.to_vec())
            }
            3 => ConstantPoolEntry::Integer(r.u32()? as i32),
            4 => ConstantPoolEntry::Float(f32::from_bits(r.u32()?)),
            5 => ConstantPoolEntry::Long(r.u64()? as i64),
            6 => ConstantPoolEntry::Double(f64::from_bits(r.u64()?)),
            7 => ConstantPoolEntry::Class {
                name_index: r.u16()?,
            },
            8 => ConstantPoolEntry::String {
                string_index: r.u16()?,
            },
            9 => ConstantPoolEntry::FieldRef {
                class_index: r.u16()?,
                name_and_type_index: r.u16()?,
            },
            10 => ConstantPoolEntry::MethodRef {
                class_index: r.u16()?,
                name_and_type_index: r.u16()?,
            },
            11 => ConstantPoolEntry::InterfaceMethodRef {
                class_index: r.u16()?,
                name_and_type_index: r.u16()?,
            },
            12 => ConstantPoolEntry::NameAndType {
                name_index: r.u16()?,
                descriptor_index: r.u16()?,
            },
            15 => ConstantPoolEntry::MethodHandle {
                reference_kind: r.u8()?,
                reference_index: r.u16()?,
            },
            16 => ConstantPoolEntry::MethodType {
                descriptor_index: r.u16()?,
            },
            17 => ConstantPoolEntry::Dynamic {
                bootstrap_method_attr_index: r.u16()?,
                name_and_type_index: r.u16()?,
            },
            18 => ConstantPoolEntry::InvokeDynamic {
                bootstrap_method_attr_index: r.u16()?,
                name_and_type_index: r.u16()?,
            },
            19 => ConstantPoolEntry::Module {
                name_index: r.u16()?,
            },
            20 => ConstantPoolEntry::Package {
                name_index: r.u16()?,
            },
            other => return Err(ClassfileError::InvalidConstantTag(other)),
        })
    }

    fn write(&self, w: &mut Writer) {
        match self {
            ConstantPoolEntry::Utf8(bytes) => {
                w.u8(1);
                w.u16(bytes.len() as u16);
                w.bytes(bytes);
            }
            ConstantPoolEntry::Integer(v) => {
                w.u8(3);
                w.u32(*v as u32);
            }
            ConstantPoolEntry::Float(v) => {
                w.u8(4);
                w.u32(v.to_bits());
            }
            ConstantPoolEntry::Long(v) => {
                w.u8(5);
                w.u64(*v as u64);
            }
            ConstantPoolEntry::Double(v) => {
                w.u8(6);
                w.u64(v.to_bits());
            }
            ConstantPoolEntry::Class { name_index } => {
                w.u8(7);
                w.u16(*name_index);
            }
            ConstantPoolEntry::String { string_index } => {
                w.u8(8);
                w.u16(*string_index);
            }
            ConstantPoolEntry::FieldRef {
                class_index,
                name_and_type_index,
            } => {
                w.u8(9);
                w.u16(*class_index);
                w.u16(*name_and_type_index);
            }
            ConstantPoolEntry::MethodRef {
                class_index,
                name_and_type_index,
            } => {
                w.u8(10);
                w.u16(*class_index);
                w.u16(*name_and_type_index);
            }
            ConstantPoolEntry::InterfaceMethodRef {
                class_index,
                name_and_type_index,
            } => {
                w.u8(11);
                w.u16(*class_index);
                w.u16(*name_and_type_index);
            }
            ConstantPoolEntry::NameAndType {
                name_index,
                descriptor_index,
            } => {
                w.u8(12);
                w.u16(*name_index);
                w.u16(*descriptor_index);
            }
            ConstantPoolEntry::MethodHandle {
                reference_kind,
                reference_index,
            } => {
                w.u8(15);
                w.u8(*reference_kind);
                w.u16(*reference_index);
            }
            ConstantPoolEntry::MethodType { descriptor_index } => {
                w.u8(16);
                w.u16(*descriptor_index);
            }
            ConstantPoolEntry::Dynamic {
                bootstrap_method_attr_index,
                name_and_type_index,
            } => {
                w.u8(17);
                w.u16(*bootstrap_method_attr_index);
                w.u16(*name_and_type_index);
            }
            ConstantPoolEntry::InvokeDynamic {
                bootstrap_method_attr_index,
                name_and_type_index,
            } => {
                w.u8(18);
                w.u16(*bootstrap_method_attr_index);
                w.u16(*name_and_type_index);
            }
            ConstantPoolEntry::Module { name_index } => {
                w.u8(19);
                w.u16(*name_index);
            }
            ConstantPoolEntry::Package { name_index } => {
                w.u8(20);
                w.u16(*name_index);
            }
        }
    }
}

/// Decode JVM *modified* UTF-8 (JVMS §4.4.7) into a Rust string. ASCII is borrowed verbatim;
/// anything else (two-/three-byte forms, the six-byte supplementary form, NUL as `0xC0 0x80`) is
/// decoded into an owned `String`, with malformed sequences replaced by U+FFFD.
fn decode_modified_utf8(bytes: &[u8]) -> Cow<'_, str> {
    if bytes.is_ascii() {
        // ASCII is identical in modified UTF-8 and is valid Rust `str`.
        return Cow::Borrowed(core::str::from_utf8(bytes).unwrap_or_default());
    }
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        if b0 < 0x80 {
            out.push(b0 as char);
            i += 1;
        } else if b0 == 0xED
            && i + 5 < bytes.len()
            && bytes[i + 1] & 0xF0 == 0xA0
            && bytes[i + 3] == 0xED
            && bytes[i + 4] & 0xF0 == 0xB0
        {
            // Six-byte supplementary form: a surrogate pair, each surrogate as a 3-byte sequence.
            let b1 = u32::from(bytes[i + 1]);
            let b2 = u32::from(bytes[i + 2]);
            let b4 = u32::from(bytes[i + 4]);
            let b5 = u32::from(bytes[i + 5]);
            let c = 0x10000
                + (((b1 & 0x0F) << 16) | ((b2 & 0x3F) << 10) | ((b4 & 0x0F) << 6) | (b5 & 0x3F));
            out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
            i += 6;
        } else if b0 & 0xE0 == 0xC0 && i + 1 < bytes.len() {
            let b1 = u32::from(bytes[i + 1]);
            let c = ((u32::from(b0) & 0x1F) << 6) | (b1 & 0x3F);
            out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
            i += 2;
        } else if b0 & 0xF0 == 0xE0 && i + 2 < bytes.len() {
            let b1 = u32::from(bytes[i + 1]);
            let b2 = u32::from(bytes[i + 2]);
            let c = ((u32::from(b0) & 0x0F) << 12) | ((b1 & 0x3F) << 6) | (b2 & 0x3F);
            out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
            i += 3;
        } else {
            out.push('\u{FFFD}');
            i += 1;
        }
    }
    Cow::Owned(out)
}
