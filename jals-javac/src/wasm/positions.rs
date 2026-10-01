//! The `jals.positions` custom section: where each statement was written.
//!
//! A module compiled with [`WasmOptions::positions`](super::WasmOptions) stores the index of the
//! statement it is executing in an exported global before that statement's own code runs, and the
//! table here says what each index means. That is what lets a *run* report a line the way a
//! compile reports one: an engine that stops on a trap reads the global back —
//! [`POSITION_GLOBAL`] — and finds the file and byte range the statement was written at.
//!
//! The table is not free — a pair of instructions per statement and one exported global per
//! module — so it is a compile's choice, and a script compile is what asks for it.
//!
//! # The format
//!
//! A count, then that many records of `(file, start, end)`, each a LEB128 `u32` as wasm spells
//! every length. No magic and no version: writer and reader are one release of one crate, and a
//! section this reader cannot make sense of is treated as absent rather than as an error — see
//! [`Positions::of_module`].

use alloc::vec::Vec;
use core::ops::Range;

use jals_hir::FileId;

use super::encode::Bytes;

/// The custom section's name, as it is written into the module.
pub const CUSTOM_SECTION: &str = "jals.positions";

/// The global an instrumented module exports and every statement stores its index into.
///
/// Mutable and initialised to `-1`, which is "no statement has run yet": a failure while the
/// module is being instantiated reads as no position, not as position zero.
pub const POSITION_GLOBAL: &str = "$jals$position";

/// Where one statement was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// The source file, numbered the way the compile numbered its inputs.
    pub file: u32,
    /// The statement's first byte in that file.
    pub start: u32,
    /// One past its last byte.
    pub end: u32,
}

/// The statement positions a module carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Positions {
    positions: Vec<Position>,
}

impl Positions {
    /// The table a module carries, or `None` when it carries none.
    ///
    /// A minimal section walk, the same one the ABI reader does: every section but a custom one is
    /// skipped by its length, not decoded, so this stays a reader of the container the table
    /// travels in.
    ///
    /// A malformed or truncated table reads as none. Positions are a diagnostic aid, and a module
    /// refused because its own table is scrap would have turned the aid into a new failure mode.
    pub fn of_module(module: &[u8]) -> Option<Self> {
        if module.len() < 8 || &module[..8] != b"\0asm\x01\0\0\0" {
            return None;
        }
        let mut reader = Reader::new(&module[8..]);
        while !reader.is_empty() {
            let id = reader.byte()?;
            let size = usize::try_from(reader.u32()?).ok()?;
            let payload = reader.take(size)?;
            if id != 0 {
                continue;
            }
            // A custom section's payload starts with its name; the rest is the contents.
            let mut payload = Reader::new(payload);
            let name = payload.name()?;
            if name == CUSTOM_SECTION {
                return Self::read(payload.rest());
            }
        }
        None
    }

    /// Where the statement with this index was written.
    ///
    /// `None` for the `-1` the global starts at and for any index the table does not hold: a
    /// reader that guessed at either would point at the wrong statement, which is worse than
    /// pointing at none.
    pub fn get(&self, index: i32) -> Option<(FileId, Range<usize>)> {
        let position = self.positions.get(usize::try_from(index).ok()?)?;
        Some((
            FileId(position.file),
            usize::try_from(position.start).ok()?..usize::try_from(position.end).ok()?,
        ))
    }

    /// The records a section payload holds.
    fn read(bytes: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(bytes);
        let count = usize::try_from(reader.u32()?).ok()?;
        let mut positions = Vec::new();
        for _ in 0..count {
            positions.push(Position {
                file: reader.u32()?,
                start: reader.u32()?,
                end: reader.u32()?,
            });
        }
        // Trailing bytes would mean a writer and a reader that disagree about the shape, and this
        // one would be guessing at where the records end.
        if !reader.is_empty() {
            return None;
        }
        Some(Self { positions })
    }

    /// The section contents for a table, index 0 first.
    ///
    /// The write side of [`read`](Self::read), and the reason it is associated with the table type
    /// rather than free: the two are one format and belong beside each other.
    pub(crate) fn encode(positions: &[Position]) -> Vec<u8> {
        let mut out = Bytes::new();
        out.count(positions.len());
        for position in positions {
            out.u32(position.file).u32(position.start).u32(position.end);
        }
        out.into_vec()
    }
}

/// A cursor over section bytes.
///
/// The same shape as the ABI reader's, kept apart because the two sections share only the
/// encoding: what one reads as a type list the other reads as a table of triples.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    const fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    /// What is left, for handing the contents of a nested structure on.
    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at..]
    }

    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    /// LEB128, as wasm spells every length.
    ///
    /// The last of five bytes carries three more data bits than a `u32` holds; accepting them would
    /// read `FF FF FF FF 7F` as `u32::MAX`, so a byte meaning that is no value at all.
    fn u32(&mut self) -> Option<u32> {
        let mut value = 0u32;
        for shift in 0..5 {
            let byte = self.byte()?;
            let payload = u32::from(byte & 0x7F);
            if shift == 4 && payload & 0x70 != 0 {
                return None;
            }
            value |= payload << (shift * 7);
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    /// A length-prefixed UTF-8 string, which is how a custom section spells its name.
    fn name(&mut self) -> Option<&'a str> {
        let len = usize::try_from(self.u32()?).ok()?;
        core::str::from_utf8(self.take(len)?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wasm::Module;

    /// One statement per line, which is enough to tell the records apart.
    fn positions() -> Vec<Position> {
        alloc::vec![
            Position {
                file: 0,
                start: 20,
                end: 33,
            },
            Position {
                file: 1,
                start: 0,
                end: 11,
            },
        ]
    }

    #[test]
    fn a_table_round_trips_through_a_module() {
        let positions = positions();
        let mut module = Module::new();
        module.add_custom_section(CUSTOM_SECTION.to_owned(), Positions::encode(&positions));
        let bytes = module.finish().expect("encodes");

        let read = Positions::of_module(&bytes).expect("the section reads back");
        assert_eq!(read.get(0), Some((FileId(0), 20..33)));
        assert_eq!(read.get(1), Some((FileId(1), 0..11)));
    }

    #[test]
    fn a_module_without_the_section_has_no_positions() {
        let module = Module::new().finish().expect("encodes");
        assert_eq!(Positions::of_module(&module), None);
    }

    #[test]
    fn the_global_starts_at_no_statement_and_ends_at_the_table() {
        let mut module = Module::new();
        module.add_custom_section(CUSTOM_SECTION.to_owned(), Positions::encode(&positions()));
        let bytes = module.finish().expect("encodes");
        let read = Positions::of_module(&bytes).expect("the section reads back");

        // `-1` is what an unwritten global holds; `2` is one past the last record.
        assert_eq!(read.get(-1), None);
        assert_eq!(read.get(2), None);
    }

    #[test]
    fn a_section_that_stops_mid_record_reads_as_absent() {
        let mut section = Positions::encode(&positions());
        section.truncate(section.len() - 1);
        let mut module = Module::new();
        module.add_custom_section(CUSTOM_SECTION.to_owned(), section);
        let bytes = module.finish().expect("encodes");
        assert_eq!(Positions::of_module(&bytes), None);
    }
}
