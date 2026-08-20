//! Reading a method's `LineNumberTable` — the one piece of *advisory* debug metadata the
//! structurer consults.
//!
//! Nothing here can be checked against the bytecode, so it never decides a structure on its own. It
//! only breaks a tie the CFG leaves open: a source `for` and the equivalent `while` compile to
//! byte-identical code, and the line table is the only thing that tells them apart. Every reader is
//! total and answers `None` when the table is absent, empty, or does not cover the offset, and the
//! caller then keeps its structural default.

use jals_classfile::{AttributeBody, CodeAttribute, LineNumberEntry};

pub(crate) use api::{line_at, table};

/// Namespace for the `LineNumberTable` readers.
mod api {
    use super::{AttributeBody, CodeAttribute, LineNumberEntry};

    /// The method `Code`'s `LineNumberTable`, or `None`. Present unless the class was compiled with
    /// `-g:none`: `javac` emits it by default (it is what a stack trace's line numbers come from),
    /// so it survives in published jars that carry no `LocalVariableTable`.
    pub(crate) fn table(code: &CodeAttribute) -> Option<&[LineNumberEntry]> {
        code.attributes.iter().find_map(|a| match &a.body {
            AttributeBody::LineNumberTable(table) => Some(table.as_slice()),
            _ => None,
        })
    }

    /// The source line of the instruction at byte offset `pc`: the entry with the greatest
    /// `start_pc <= pc`, since an entry's line runs until the next one begins. §4.7.12 does not
    /// require the table to be sorted, so this is a linear max-scan rather than a binary search.
    pub(crate) fn line_at(table: &[LineNumberEntry], pc: usize) -> Option<u16> {
        table
            .iter()
            .filter(|entry| usize::from(entry.start_pc) <= pc)
            .max_by_key(|entry| entry.start_pc)
            .map(|entry| entry.line_number)
    }
}

#[cfg(test)]
mod tests {
    use super::api;
    use jals_classfile::LineNumberEntry;

    fn entry(start_pc: u16, line_number: u16) -> LineNumberEntry {
        LineNumberEntry {
            start_pc,
            line_number,
        }
    }

    #[test]
    fn a_line_covers_offsets_up_to_the_next_entry() {
        // A `for`'s table, as `javac` emits it: the update jumps back to the header's line.
        let table = [entry(0, 3), entry(7, 4), entry(11, 3), entry(17, 6)];
        assert_eq!(api::line_at(&table, 0), Some(3));
        // pc 2 has no entry of its own — it is still covered by the one at pc 0.
        assert_eq!(api::line_at(&table, 2), Some(3));
        assert_eq!(api::line_at(&table, 7), Some(4));
        assert_eq!(api::line_at(&table, 11), Some(3));
        assert_eq!(api::line_at(&table, 20), Some(6));
    }

    #[test]
    fn an_offset_before_the_first_entry_has_no_line() {
        assert_eq!(api::line_at(&[entry(4, 9)], 0), None);
        assert_eq!(api::line_at(&[], 0), None);
    }

    #[test]
    fn an_unsorted_table_still_resolves() {
        // §4.7.12 does not require ascending `start_pc`, so the scan must not assume it.
        let table = [entry(11, 3), entry(0, 3), entry(7, 4)];
        assert_eq!(api::line_at(&table, 9), Some(4));
        assert_eq!(api::line_at(&table, 11), Some(3));
    }
}
