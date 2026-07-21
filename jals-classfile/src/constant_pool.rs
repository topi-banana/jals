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

use crate::bytes::{Input, Reader, Writer};
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
    pub(crate) async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        let count = r.u16().await?;
        let mut entries = Vec::with_capacity(count as usize);
        entries.push(ConstantSlot::Sentinel);
        let mut i = 1u16;
        while i < count {
            let entry = ConstantPoolEntry::read(r).await?;
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
        Ok(Self { entries })
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

    /// The first unassigned pool index: valid entry indices are `1..self.next_index()`.
    ///
    /// This equals the `constant_pool_count` the pool would serialise with right now.
    pub const fn next_index(&self) -> u16 {
        self.entries.len() as u16
    }

    /// Append `entry` to the pool and return its new 1-based index, or `None` when the pool has no
    /// room (`constant_pool_count` is a `u16`, and `Long`/`Double` each need two slots).
    ///
    /// Appending never moves existing entries, so every index already handed out stays valid. A
    /// remapper-style transform can therefore grow the pool while repointing existing references.
    pub fn add(&mut self, entry: ConstantPoolEntry) -> Option<u16> {
        Self::check_utf8_length(&entry)?;
        let wide = matches!(
            entry,
            ConstantPoolEntry::Long(_) | ConstantPoolEntry::Double(_)
        );
        let needed = if wide { 2 } else { 1 };
        if self.entries.len() + needed > 0xFFFF {
            return None;
        }
        let index = u16::try_from(self.entries.len()).ok()?;
        self.entries.push(ConstantSlot::Entry(entry));
        if wide {
            self.entries.push(ConstantSlot::Gap);
        }
        Some(index)
    }

    /// Overwrite the entry at `index` and return the previous one, keeping every other index
    /// stable. Returns `None` — leaving the pool untouched — when `index` does not denote an
    /// existing entry, or when either side is a `Long`/`Double`: the two-slot layout must never
    /// change, or every higher index would shift.
    pub fn replace(&mut self, index: u16, entry: ConstantPoolEntry) -> Option<ConstantPoolEntry> {
        if matches!(
            entry,
            ConstantPoolEntry::Long(_) | ConstantPoolEntry::Double(_)
        ) {
            return None;
        }
        Self::check_utf8_length(&entry)?;
        let slot = self.entries.get_mut(index as usize)?;
        let ConstantSlot::Entry(previous) = slot else {
            return None;
        };
        // Overwriting a `Long`/`Double` would orphan the `Gap` slot that follows it. `write` skips
        // gaps but still declares `entries.len()` entries, so the file would claim one more entry
        // than it contains and a reader would run off into the fields table.
        if matches!(
            previous,
            ConstantPoolEntry::Long(_) | ConstantPoolEntry::Double(_)
        ) {
            return None;
        }
        Some(core::mem::replace(previous, entry))
    }

    /// `Utf8` carries a `u16` byte length, so anything longer cannot be written.
    ///
    /// Without this the length silently wraps on write and the class file is corrupt in a way no
    /// caller can see. Deobfuscation makes it reachable: expanding one-character names to real
    /// ones can push a large generic `Signature` past the limit.
    fn check_utf8_length(entry: &ConstantPoolEntry) -> Option<()> {
        match entry {
            ConstantPoolEntry::Utf8(bytes) if bytes.len() > usize::from(u16::MAX) => None,
            _ => Some(()),
        }
    }

    /// The decoded text of a `Utf8` entry at `index`, or `None` if it is not a `Utf8`.
    pub fn utf8(&self, index: u16) -> Option<Cow<'_, str>> {
        match self.get(index) {
            Some(ConstantPoolEntry::Utf8(bytes)) => Some(Self::decode_modified_utf8(bytes)),
            _ => None,
        }
    }

    /// Encode a Rust string as JVM *modified* UTF-8 (JVMS §4.4.7), the inverse of
    /// [`Self::decode_modified_utf8`].
    ///
    /// Modified UTF-8 differs from standard UTF-8 in two places, and a class file written with
    /// standard UTF-8 is rejected by the JVM wherever they differ: NUL is `0xC0 0x80` rather than
    /// `0x00`, and a supplementary character is a surrogate *pair*, each surrogate encoded as its
    /// own three-byte sequence, rather than one four-byte sequence.
    #[must_use]
    pub fn encode_modified_utf8(text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len());
        for character in text.chars() {
            let code = character as u32;
            match code {
                // NUL must not appear as a zero byte: it would terminate the string for readers
                // that treat the encoding as C-style.
                0 => out.extend_from_slice(&[0xC0, 0x80]),
                0x01..=0x7F => out.push(code as u8),
                0x80..=0x07FF => {
                    out.push(0xC0 | (code >> 6) as u8);
                    out.push(0x80 | (code & 0x3F) as u8);
                }
                0x0800..=0xFFFF => {
                    out.push(0xE0 | (code >> 12) as u8);
                    out.push(0x80 | ((code >> 6) & 0x3F) as u8);
                    out.push(0x80 | (code & 0x3F) as u8);
                }
                _ => {
                    let value = code - 0x1_0000;
                    let high = 0xD800 | (value >> 10);
                    let low = 0xDC00 | (value & 0x3FF);
                    for surrogate in [high, low] {
                        out.push(0xE0 | (surrogate >> 12) as u8);
                        out.push(0x80 | ((surrogate >> 6) & 0x3F) as u8);
                        out.push(0x80 | (surrogate & 0x3F) as u8);
                    }
                }
            }
        }
        out
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
                    + (((b1 & 0x0F) << 16)
                        | ((b2 & 0x3F) << 10)
                        | ((b4 & 0x0F) << 6)
                        | (b5 & 0x3F));
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

    /// The internal-form name (`a/b/C`) a `Class` entry at `index` points to.
    pub fn class_name(&self, index: u16) -> Option<Cow<'_, str>> {
        match self.get(index) {
            Some(ConstantPoolEntry::Class { name_index }) => self.utf8(*name_index),
            _ => None,
        }
    }
}

impl ConstantPoolEntry {
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        let tag = r.u8().await?;
        Ok(match tag {
            1 => {
                let len = r.u16().await? as usize;
                Self::Utf8(r.bytes(len).await?)
            }
            3 => Self::Integer(r.u32().await? as i32),
            4 => Self::Float(f32::from_bits(r.u32().await?)),
            5 => Self::Long(r.u64().await? as i64),
            6 => Self::Double(f64::from_bits(r.u64().await?)),
            7 => Self::Class {
                name_index: r.u16().await?,
            },
            8 => Self::String {
                string_index: r.u16().await?,
            },
            9 => Self::FieldRef {
                class_index: r.u16().await?,
                name_and_type_index: r.u16().await?,
            },
            10 => Self::MethodRef {
                class_index: r.u16().await?,
                name_and_type_index: r.u16().await?,
            },
            11 => Self::InterfaceMethodRef {
                class_index: r.u16().await?,
                name_and_type_index: r.u16().await?,
            },
            12 => Self::NameAndType {
                name_index: r.u16().await?,
                descriptor_index: r.u16().await?,
            },
            15 => Self::MethodHandle {
                reference_kind: r.u8().await?,
                reference_index: r.u16().await?,
            },
            16 => Self::MethodType {
                descriptor_index: r.u16().await?,
            },
            17 => Self::Dynamic {
                bootstrap_method_attr_index: r.u16().await?,
                name_and_type_index: r.u16().await?,
            },
            18 => Self::InvokeDynamic {
                bootstrap_method_attr_index: r.u16().await?,
                name_and_type_index: r.u16().await?,
            },
            19 => Self::Module {
                name_index: r.u16().await?,
            },
            20 => Self::Package {
                name_index: r.u16().await?,
            },
            other => return Err(ClassfileError::InvalidConstantTag(other)),
        })
    }

    fn write(&self, w: &mut Writer) {
        match self {
            Self::Utf8(bytes) => {
                w.u8(1);
                w.u16(bytes.len() as u16);
                w.bytes(bytes);
            }
            Self::Integer(v) => {
                w.u8(3);
                w.u32(v.cast_unsigned());
            }
            Self::Float(v) => {
                w.u8(4);
                w.u32(v.to_bits());
            }
            Self::Long(v) => {
                w.u8(5);
                w.u64(*v as u64);
            }
            Self::Double(v) => {
                w.u8(6);
                w.u64(v.to_bits());
            }
            Self::Class { name_index } => {
                w.u8(7);
                w.u16(*name_index);
            }
            Self::String { string_index } => {
                w.u8(8);
                w.u16(*string_index);
            }
            Self::FieldRef {
                class_index,
                name_and_type_index,
            } => {
                w.u8(9);
                w.u16(*class_index);
                w.u16(*name_and_type_index);
            }
            Self::MethodRef {
                class_index,
                name_and_type_index,
            } => {
                w.u8(10);
                w.u16(*class_index);
                w.u16(*name_and_type_index);
            }
            Self::InterfaceMethodRef {
                class_index,
                name_and_type_index,
            } => {
                w.u8(11);
                w.u16(*class_index);
                w.u16(*name_and_type_index);
            }
            Self::NameAndType {
                name_index,
                descriptor_index,
            } => {
                w.u8(12);
                w.u16(*name_index);
                w.u16(*descriptor_index);
            }
            Self::MethodHandle {
                reference_kind,
                reference_index,
            } => {
                w.u8(15);
                w.u8(*reference_kind);
                w.u16(*reference_index);
            }
            Self::MethodType { descriptor_index } => {
                w.u8(16);
                w.u16(*descriptor_index);
            }
            Self::Dynamic {
                bootstrap_method_attr_index,
                name_and_type_index,
            } => {
                w.u8(17);
                w.u16(*bootstrap_method_attr_index);
                w.u16(*name_and_type_index);
            }
            Self::InvokeDynamic {
                bootstrap_method_attr_index,
                name_and_type_index,
            } => {
                w.u8(18);
                w.u16(*bootstrap_method_attr_index);
                w.u16(*name_and_type_index);
            }
            Self::Module { name_index } => {
                w.u8(19);
                w.u16(*name_index);
            }
            Self::Package { name_index } => {
                w.u8(20);
                w.u16(*name_index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    /// A pool with only its index-0 sentinel, matching what `read` produces for an empty pool.
    fn empty_pool() -> ConstantPool {
        ConstantPool {
            entries: vec![ConstantSlot::Sentinel],
        }
    }

    /// Modified UTF-8 is not standard UTF-8: NUL is two bytes and a supplementary character is a
    /// surrogate *pair* of three-byte sequences. Writing standard UTF-8 produces a class file the
    /// JVM rejects, so the encoder must round-trip through the decoder.
    #[test]
    fn modified_utf8_round_trips() {
        for text in [
            "",
            "plain/Ascii",
            "nul\0inside",
            "café",
            "\u{1F600} emoji",
            "\u{10FFFF}",
        ] {
            let encoded = ConstantPool::encode_modified_utf8(text);
            assert_eq!(
                ConstantPool::decode_modified_utf8(&encoded),
                text,
                "round trip failed for {text:?}"
            );
        }

        // The two places modified UTF-8 diverges from `str::as_bytes`.
        assert_eq!(ConstantPool::encode_modified_utf8("\0"), vec![0xC0, 0x80]);
        assert_eq!(ConstantPool::encode_modified_utf8("\u{1F600}").len(), 6);
    }

    /// `Utf8` carries a `u16` length, so an over-long entry cannot be written. Accepting it would
    /// truncate the length on write and silently corrupt the class file.
    #[test]
    fn rejects_an_over_long_utf8() {
        let mut pool = empty_pool();
        let ok = ConstantPoolEntry::Utf8(vec![b'a'; usize::from(u16::MAX)]);
        let too_long = ConstantPoolEntry::Utf8(vec![b'a'; usize::from(u16::MAX) + 1]);

        let index = pool.add(ok).expect("a maximum-length Utf8 fits");
        assert_eq!(pool.add(too_long.clone()), None);
        assert_eq!(pool.replace(index, too_long), None);
    }

    /// Overwriting a `Long`/`Double` would orphan the `Gap` slot that follows it, leaving the
    /// written pool one entry short of the count it declares.
    #[test]
    fn replace_refuses_to_overwrite_a_wide_entry() {
        let mut pool = empty_pool();
        let wide = pool
            .add(ConstantPoolEntry::Long(1))
            .expect("a Long fits in an empty pool");
        assert_eq!(
            pool.replace(wide, ConstantPoolEntry::Utf8(b"x".to_vec())),
            None
        );
        assert!(matches!(pool.get(wide), Some(ConstantPoolEntry::Long(1))));
    }
}
