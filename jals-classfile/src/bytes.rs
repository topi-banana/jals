//! A minimal big-endian streaming reader ([`Reader`]) and sink ([`Writer`]).
//!
//! The sole primitive codec layer. The reader pulls from any portable byte source
//! ([`jals_storage::io::Read`]), so the crate stays `wasm32`-pure while hosts stream class files
//! straight from buffered files or archive members. Every read is checked against the stream and
//! returns [`Result`] rather than panicking.

use alloc::vec::Vec;

use jals_storage::io::IoError;
pub(crate) use jals_storage::io::Read as Input;

use crate::error::{ClassfileError, Result};

/// A big-endian reader over a portable byte source, tracking the absolute offset consumed.
pub(crate) struct Reader<R> {
    src: R,
    pos: usize,
}

/// Cap on a single up-front buffer allocation. Declared byte lengths are attacker-controlled
/// `u32`s and a stream cannot pre-check them, so capacity grows chunkwise as bytes actually
/// arrive and a huge length over a short input cannot force a huge allocation. (The old slice
/// cursor got this for free from its bounds check.)
const ALLOCATION_CHUNK: usize = 64 * 1024;

impl<R: Input> Reader<R> {
    pub(crate) const fn new(src: R) -> Self {
        Self { src, pos: 0 }
    }

    /// The current absolute byte offset. Used by `Code` to compute the alignment padding of
    /// `tableswitch` / `lookupswitch`, which is relative to the start of the code array.
    pub(crate) const fn pos(&self) -> usize {
        self.pos
    }

    /// Fill `out` from the source, or fail without advancing the tracked offset.
    fn fill(&mut self, out: &mut [u8]) -> Result<()> {
        match self.src.read_exact(out) {
            Ok(()) => {
                self.pos += out.len();
                Ok(())
            }
            Err(IoError::UnexpectedEof) => Err(ClassfileError::UnexpectedEof {
                offset: self.pos,
                needed: out.len(),
            }),
            Err(error @ IoError::Failed(_)) => Err(ClassfileError::Source(error)),
        }
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        let mut b = [0; 1];
        self.fill(&mut b)?;
        Ok(b[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        let mut b = [0; 2];
        self.fill(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        let mut b = [0; 4];
        self.fill(&mut b)?;
        Ok(u32::from_be_bytes(b))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        let mut b = [0; 8];
        self.fill(&mut b)?;
        Ok(u64::from_be_bytes(b))
    }

    /// Read and own the next `n` bytes verbatim.
    pub(crate) fn bytes(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        while out.len() < n {
            let start = out.len();
            let chunk = (n - start).min(ALLOCATION_CHUNK);
            out.resize(start + chunk, 0);
            self.fill(&mut out[start..])?;
        }
        Ok(out)
    }

    /// Read a `u16`-counted run of items, each parsed by `read_one`.
    pub(crate) fn list<T>(&mut self, read_one: impl Fn(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let count = self.u16()?;
        let mut v = Vec::with_capacity(count as usize);
        for _ in 0..count {
            v.push(read_one(self)?);
        }
        Ok(v)
    }

    /// Read a `u16`-counted run of raw `u16` indices.
    pub(crate) fn u16_list(&mut self) -> Result<Vec<u16>> {
        let count = self.u16()?;
        let mut v = Vec::with_capacity(count as usize);
        for _ in 0..count {
            v.push(self.u16()?);
        }
        Ok(v)
    }

    /// Require end of input — the top-level "no trailing bytes" check.
    pub(crate) fn expect_eof(&mut self) -> Result<()> {
        let mut probe = [0u8; 1];
        match self.src.read(&mut probe) {
            Ok(0) | Err(IoError::UnexpectedEof) => Ok(()),
            Ok(_) => Err(ClassfileError::TrailingBytes),
            Err(error) => Err(ClassfileError::Source(error)),
        }
    }
}

impl Reader<&[u8]> {
    /// How many bytes are left unread. Only slice-backed readers (attribute bodies, code
    /// arrays) can know this; a slice source consumes itself from the front, so its length is
    /// exactly the unread remainder.
    pub(crate) const fn remaining(&self) -> usize {
        self.src.len()
    }
}

/// A reserved 4-byte slot in a [`Writer`], to be filled in later with the byte length of whatever
/// was written after it. See [`Writer::reserve_u32_len`] / [`Writer::patch_u32_len`].
#[derive(Clone, Copy)]
#[must_use]
pub(crate) struct LenPatch(usize);

/// A big-endian byte sink.
#[derive(Default)]
pub(crate) struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Self::default()
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

    /// Write a `u16`-counted run of items, each emitted by `write_one`.
    pub(crate) fn list<T>(&mut self, items: &[T], write_one: impl Fn(&T, &mut Self)) {
        self.u16(items.len() as u16);
        for item in items {
            write_one(item, self);
        }
    }

    /// Write a `u16`-counted run of raw `u16` indices.
    pub(crate) fn u16_list(&mut self, items: &[u16]) {
        self.u16(items.len() as u16);
        for &i in items {
            self.u16(i);
        }
    }

    /// The number of bytes written so far. Used as the current code-array offset when emitting
    /// alignment padding for `tableswitch` / `lookupswitch`.
    pub(crate) const fn len(&self) -> usize {
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
