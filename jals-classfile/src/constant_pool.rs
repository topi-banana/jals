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
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self> {
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

    /// The decoded text of a `Utf8` entry at `index`, or `None` if it is not a `Utf8`.
    pub fn utf8(&self, index: u16) -> Option<Cow<'_, str>> {
        match self.get(index) {
            Some(ConstantPoolEntry::Utf8(bytes)) => Some(Self::decode_modified_utf8(bytes)),
            _ => None,
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
                // The masked fields occupy disjoint bit ranges, so summing them assembles the code
                // point exactly as OR-ing would (written as `+` so each term is independent).
                let b1 = u32::from(bytes[i + 1]);
                let b2 = u32::from(bytes[i + 2]);
                let b4 = u32::from(bytes[i + 4]);
                let b5 = u32::from(bytes[i + 5]);
                let c = 0x10000
                    + ((b1 & 0x0F) << 16)
                    + ((b2 & 0x3F) << 10)
                    + ((b4 & 0x0F) << 6)
                    + (b5 & 0x3F);
                out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                i += 6;
            } else if b0 & 0xE0 == 0xC0 && i + 1 < bytes.len() {
                let b1 = u32::from(bytes[i + 1]);
                // Disjoint bit ranges — `+` assembles the code point just as `|` would.
                let c = ((u32::from(b0) & 0x1F) << 6) + (b1 & 0x3F);
                out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                i += 2;
            } else if b0 & 0xF0 == 0xE0 && i + 2 < bytes.len() {
                let b1 = u32::from(bytes[i + 1]);
                let b2 = u32::from(bytes[i + 2]);
                // Disjoint bit ranges — `+` assembles the code point just as `|` would.
                let c = ((u32::from(b0) & 0x0F) << 12) + ((b1 & 0x3F) << 6) + (b2 & 0x3F);
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
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        let tag = r.u8()?;
        Ok(match tag {
            1 => {
                let len = r.u16()? as usize;
                Self::Utf8(r.bytes(len)?.to_vec())
            }
            3 => Self::Integer(r.u32()? as i32),
            4 => Self::Float(f32::from_bits(r.u32()?)),
            5 => Self::Long(r.u64()? as i64),
            6 => Self::Double(f64::from_bits(r.u64()?)),
            7 => Self::Class {
                name_index: r.u16()?,
            },
            8 => Self::String {
                string_index: r.u16()?,
            },
            9 => Self::FieldRef {
                class_index: r.u16()?,
                name_and_type_index: r.u16()?,
            },
            10 => Self::MethodRef {
                class_index: r.u16()?,
                name_and_type_index: r.u16()?,
            },
            11 => Self::InterfaceMethodRef {
                class_index: r.u16()?,
                name_and_type_index: r.u16()?,
            },
            12 => Self::NameAndType {
                name_index: r.u16()?,
                descriptor_index: r.u16()?,
            },
            15 => Self::MethodHandle {
                reference_kind: r.u8()?,
                reference_index: r.u16()?,
            },
            16 => Self::MethodType {
                descriptor_index: r.u16()?,
            },
            17 => Self::Dynamic {
                bootstrap_method_attr_index: r.u16()?,
                name_and_type_index: r.u16()?,
            },
            18 => Self::InvokeDynamic {
                bootstrap_method_attr_index: r.u16()?,
                name_and_type_index: r.u16()?,
            },
            19 => Self::Module {
                name_index: r.u16()?,
            },
            20 => Self::Package {
                name_index: r.u16()?,
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
    use super::*;
    use crate::bytes::Reader;

    fn decode(bytes: &[u8]) -> String {
        ConstantPool::decode_modified_utf8(bytes).into_owned()
    }

    /// An independently-written reference encoder for JVM *modified* UTF-8 (JVMS §4.4.7): NUL as
    /// `0xC0 0x80`, the BMP two-/three-byte forms, and supplementary code points as a surrogate pair
    /// of 3-byte sequences. Kept deliberately separate from the decoder so a single-operator mutation
    /// in the decoder makes the two disagree.
    fn encode_modified_utf8(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for ch in s.chars() {
            let c = ch as u32;
            if c == 0 {
                out.extend_from_slice(&[0xC0, 0x80]);
            } else if c < 0x80 {
                out.push(c as u8);
            } else if c < 0x800 {
                out.push(0xC0 | (c >> 6) as u8);
                out.push(0x80 | (c & 0x3F) as u8);
            } else if c < 0x1_0000 {
                out.push(0xE0 | (c >> 12) as u8);
                out.push(0x80 | ((c >> 6) & 0x3F) as u8);
                out.push(0x80 | (c & 0x3F) as u8);
            } else {
                let v = c - 0x1_0000;
                for surrogate in [0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF)] {
                    out.push(0xE0 | (surrogate >> 12) as u8);
                    out.push(0x80 | ((surrogate >> 6) & 0x3F) as u8);
                    out.push(0x80 | (surrogate & 0x3F) as u8);
                }
            }
        }
        out
    }

    fn is_surrogate(c: u32) -> bool {
        (0xD800..=0xDFFF).contains(&c)
    }

    #[test]
    fn ascii_is_borrowed_verbatim() {
        assert_eq!(decode(b"java/lang/Object"), "java/lang/Object");
        assert!(matches!(
            ConstantPool::decode_modified_utf8(b"plain"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn nul_uses_the_two_byte_form() {
        assert_eq!(decode(&[0xC0, 0x80]), "\0");
    }

    #[test]
    fn decode_round_trips_every_two_byte_code_point() {
        // Exhaustive over the whole two-byte range — every mask/shift/add in the two-byte decoder is
        // exercised against a known value.
        for c in 0x80u32..0x800 {
            let ch = char::from_u32(c).unwrap();
            let s = ch.to_string();
            assert_eq!(decode(&encode_modified_utf8(&s)), s, "U+{c:04X}");
        }
    }

    #[test]
    fn decode_round_trips_three_byte_and_supplementary_samples() {
        let three_byte = (0x800u32..0x1_0000)
            .step_by(97)
            .filter(|&c| !is_surrogate(c));
        let supplementary = (0x1_0000u32..0x11_0000).step_by(331);
        // Boundary values that the strides would otherwise miss.
        let boundaries = [0x7Fu32, 0x80, 0x7FF, 0x800, 0xFFFF, 0x1_0000, 0x10_FFFF];
        for c in three_byte.chain(supplementary).chain(boundaries) {
            if is_surrogate(c) {
                continue;
            }
            let ch = char::from_u32(c).unwrap();
            let s = ch.to_string();
            assert_eq!(decode(&encode_modified_utf8(&s)), s, "U+{c:04X}");
        }
    }

    #[test]
    fn decode_round_trips_with_leading_ascii_so_the_loop_index_is_nonzero() {
        // A non-zero loop index makes every `i + n` byte offset in the decoder observable (an `i * n`
        // or `i - n` mutation reads the wrong byte / panics). Several prefix lengths cover each form.
        let samples = [
            '\u{E9}',
            '\u{20AC}',
            '\u{1_0000}',
            '\u{1F600}',
            '\u{10_FFFF}',
        ];
        for prefix in ["", "A", "AB", "ABC", "ABCD"] {
            for ch in samples {
                let s = format!("{prefix}{ch}");
                assert_eq!(
                    decode(&encode_modified_utf8(&s)),
                    s,
                    "{prefix:?} + U+{:04X}",
                    ch as u32
                );
            }
        }
    }

    #[test]
    fn truncated_multibyte_sequences_never_read_past_the_end() {
        // Each is a lead byte whose continuation bytes are missing; the length guards must stop the
        // decoder before it indexes out of bounds (a `<` → `<=` mutation would panic here).
        assert_eq!(decode(&[0xC3]), "\u{FFFD}");
        assert_eq!(decode(&[0xE2, 0x82]), "\u{FFFD}\u{FFFD}");
        assert_eq!(
            decode(&[0xED, 0xA0, 0xBD, 0xED, 0xB8]),
            "\u{FFFD}\u{FFFD}\u{FFFD}"
        );
    }

    #[test]
    fn malformed_supplementary_conditions_fall_back_without_reading_past_the_end() {
        // A `0xED` lead whose surrogate-pair shape is broken must NOT be decoded as supplementary.
        // These pin the exact per-sub-condition behaviour of the six-byte guard: relaxing any `&&`
        // to `||` either changes the output or reads out of bounds.
        assert_eq!(decode(&[0xED]), "\u{FFFD}");
        assert_eq!(decode(&[0xED, 0x00]), "\u{FFFD}\0");
        assert_eq!(decode(&[0xED, 0xA0, 0x00]), "\u{FFFD}");
        // A valid six-byte surrogate shape that does NOT start with 0xED: only if the leading
        // `b0 == 0xED` guard is (wrongly) OR-ed in does this decode as a supplementary char.
        assert_eq!(
            decode(&[0xEE, 0xA0, 0xBD, 0xED, 0xB0, 0x80]),
            "\u{E83D}\u{FFFD}"
        );
    }

    #[test]
    fn a_stray_continuation_byte_is_replaced_not_pushed() {
        // 0x80 is never a valid lead byte: the `b0 < 0x80` fast path must not claim it (a `<` → `<=`
        // mutation would push U+0080 instead of the replacement character).
        assert_eq!(decode(&[0xC3, 0xA9, 0x80]), "\u{E9}\u{FFFD}");
    }

    #[test]
    fn read_keeps_long_entries_two_slots_wide() {
        // Two back-to-back `long`s: index 1 and index 3 hold the values, 2 and 4 are the unusable
        // gap slots. The `i += 2` slot accounting is what keeps the second long at index 3 — a `-=`
        // underflows and a `*=` mis-counts into an out-of-bytes read.
        let mut bytes = vec![0x00, 0x05]; // constant_pool_count = 5
        bytes.extend_from_slice(&[0x05, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]); // Long #1
        bytes.extend_from_slice(&[0x05, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18]); // Long #2
        let pool = ConstantPool::read(&mut Reader::new(&bytes)).expect("read pool");

        assert_eq!(
            pool.get(1),
            Some(&ConstantPoolEntry::Long(0x0102_0304_0506_0708))
        );
        assert_eq!(pool.get(2), None, "the slot after a long is a gap");
        assert_eq!(
            pool.get(3),
            Some(&ConstantPoolEntry::Long(0x1112_1314_1516_1718))
        );
        assert_eq!(pool.get(4), None, "the slot after a long is a gap");
    }

    #[test]
    fn class_name_follows_the_name_index_to_its_utf8() {
        let mut bytes = vec![0x00, 0x03]; // constant_pool_count = 3
        bytes.extend_from_slice(&[0x07, 0x00, 0x02]); // #1 Class -> #2
        bytes.extend_from_slice(&[0x01, 0x00, 0x03, b'A', b'/', b'B']); // #2 Utf8 "A/B"
        let pool = ConstantPool::read(&mut Reader::new(&bytes)).expect("read pool");

        assert_eq!(pool.class_name(1).as_deref(), Some("A/B"));
        assert_eq!(pool.utf8(2).as_deref(), Some("A/B"));
        // A non-Class index has no class name.
        assert_eq!(pool.class_name(2), None);
        assert_eq!(pool.class_name(0), None);
    }
}
