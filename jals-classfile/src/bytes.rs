//! A minimal big-endian byte cursor ([`Reader`]) and sink ([`Writer`]).
//!
//! The sole primitive codec layer. No external byte crate, so the crate stays `wasm32`-pure. Every
//! read is bounds-checked and returns [`Result`] rather than panicking.

use alloc::vec::Vec;

use crate::error::{ClassfileError, Result};

/// A big-endian cursor over an input buffer.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    /// The current absolute byte offset. Used by `Code` to compute the alignment padding of
    /// `tableswitch` / `lookupswitch`, which is relative to the start of the code array.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// How many bytes are left unread.
    pub(crate) fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Borrow and consume the next `n` bytes, or fail if fewer remain.
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        match self.pos.checked_add(n).filter(|&end| end <= self.buf.len()) {
            Some(end) => {
                let s = &self.buf[self.pos..end];
                self.pos = end;
                Ok(s)
            }
            None => Err(ClassfileError::UnexpectedEof {
                offset: self.pos,
                needed: n,
            }),
        }
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        let s = self.take(8)?;
        Ok(u64::from_be_bytes([
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ]))
    }

    /// Borrow and consume the next `n` bytes verbatim.
    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }
}

/// A reserved 4-byte slot in a [`Writer`], to be filled in later with the byte length of whatever
/// was written after it. See [`Writer::reserve_u32_len`] / [`Writer::patch_u32_len`].
#[must_use]
pub(crate) struct LenPatch(usize);

/// A big-endian byte sink.
#[derive(Default)]
pub(crate) struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Writer::default()
    }

    pub(crate) fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub(crate) fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// The number of bytes written so far. Used as the current code-array offset when emitting
    /// alignment padding for `tableswitch` / `lookupswitch`.
    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }

    /// Reserve a 4-byte big-endian length field, to be back-patched once its body is written. The
    /// mechanism that makes `attribute_length` / `code_length` derived rather than stored.
    pub(crate) fn reserve_u32_len(&mut self) -> LenPatch {
        let at = self.buf.len();
        self.buf.extend_from_slice(&[0; 4]);
        LenPatch(at)
    }

    /// Fill a previously [reserved](Writer::reserve_u32_len) slot with the number of bytes written
    /// after it.
    pub(crate) fn patch_u32_len(&mut self, patch: LenPatch) {
        let len = (self.buf.len() - patch.0 - 4) as u32;
        self.buf[patch.0..patch.0 + 4].copy_from_slice(&len.to_be_bytes());
    }

    pub(crate) fn into_vec(self) -> Vec<u8> {
        self.buf
    }
}
