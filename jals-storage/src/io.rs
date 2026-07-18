//! Portable byte-stream reading.
//!
//! [`Read`] and [`Seek`] mirror the `std::io` contract on `core + alloc` as `!Send` async
//! traits, so portable consumers (the class-file codec, the archive walker) can stream bytes
//! without materializing whole buffers or naming host types. In-memory sources complete every
//! read immediately; only host-backed readers ever suspend. Std interop never uses blanket
//! impls — a blanket over `std::io::Read` would collide with the slice and [`Cursor`] impls —
//! and is confined to the `std-io`-gated newtype bridge [`StdReader`].

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use core::fmt;

/// Failure of a portable byte source.
///
/// End of input is a data-shape fact; [`Failed`](Self::Failed) carries a source failure (host
/// I/O, decompression, ...) and is never equivalent to missing or truncated data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoError {
    /// The source ended before a required read completed.
    UnexpectedEof,
    /// The backing source failed.
    Failed(String),
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => f.write_str("unexpected end of input"),
            Self::Failed(message) => write!(f, "source read failed: {message}"),
        }
    }
}

impl core::error::Error for IoError {}

/// A position to [`seek`](Seek::seek) to, mirroring `std::io::SeekFrom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekFrom {
    Start(u64),
    End(i64),
    Current(i64),
}

impl SeekFrom {
    /// Resolve to an absolute offset for a source of `len` bytes positioned at `current`.
    /// Offsets past the end are allowed; before the start is an error.
    pub(crate) fn resolve(self, len: u64, current: u64) -> Result<u64, IoError> {
        let (base, offset) = match self {
            Self::Start(offset) => return Ok(offset),
            Self::End(offset) => (len, offset),
            Self::Current(offset) => (current, offset),
        };
        base.checked_add_signed(offset)
            .ok_or_else(|| IoError::Failed(String::from("seek before start or beyond u64::MAX")))
    }
}

/// An async byte source. Completion, not readiness: a resolved read either moved bytes or
/// failed.
#[allow(async_fn_in_trait)]
pub trait Read {
    /// Pull up to `buf.len()` bytes, returning how many arrived. `Ok(0)` means end of input,
    /// never "try again later".
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError>;

    /// Fill `buf` completely, or fail with [`IoError::UnexpectedEof`].
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), IoError> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.read(&mut buf[filled..]).await? {
                0 => return Err(IoError::UnexpectedEof),
                n => filled += n,
            }
        }
        Ok(())
    }
}

/// A byte source with a movable read position.
#[allow(async_fn_in_trait)]
pub trait Seek {
    /// Move the read position, returning the new offset from the start.
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError>;

    async fn stream_position(&mut self) -> Result<u64, IoError> {
        self.seek(SeekFrom::Current(0)).await
    }
}

impl<R: Read + ?Sized> Read for &mut R {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        (**self).read(buf).await
    }
}

impl<S: Seek + ?Sized> Seek for &mut S {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError> {
        (**self).seek(pos).await
    }
}

pub(crate) use detail::read_from_slice;

/// Slice plumbing shared by the in-memory readers, grouped per the repository's
/// no-free-functions layout.
mod detail {
    /// Consumes a slice from the front without suspending, mirroring `std`.
    pub(crate) fn read_from_slice(source: &mut &[u8], buf: &mut [u8]) -> usize {
        let n = source.len().min(buf.len());
        let (head, tail) = source.split_at(n);
        buf[..n].copy_from_slice(head);
        *source = tail;
        n
    }
}

/// Mirrors `std`: a slice reads by consuming itself from the front. Always ready.
impl Read for &[u8] {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        Ok(read_from_slice(self, buf))
    }
}

/// An owned, seekable view over in-memory bytes, mirroring `std::io::Cursor`. Always ready.
///
/// Seeking past the end is allowed; reads there return end of input. Seeking before the start
/// is an error.
#[derive(Debug, Clone)]
pub struct Cursor<T> {
    data: T,
    pos: u64,
}

impl<T> Cursor<T> {
    pub const fn new(data: T) -> Self {
        Self { data, pos: 0 }
    }
}

impl<T: AsRef<[u8]>> Read for Cursor<T> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        let data = self.data.as_ref();
        let start = usize::try_from(self.pos).map_or(data.len(), |pos| pos.min(data.len()));
        let mut pending = &data[start..];
        let n = read_from_slice(&mut pending, buf);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<T: AsRef<[u8]>> Seek for Cursor<T> {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError> {
        self.pos = pos.resolve(self.data.as_ref().len() as u64, self.pos)?;
        Ok(self.pos)
    }
}

pub(crate) const BUFFER_CAPACITY: usize = 64 * 1024;

/// A buffered sequential reader.
///
/// Read-only by design: it fronts forward-only streams (an archive member, a verification
/// pass), never a source whose `Seek` a consumer still needs — buffering would silently
/// desynchronize the underlying position.
#[derive(Debug, Clone)]
pub struct Buffered<R> {
    inner: R,
    buf: Box<[u8]>,
    start: usize,
    filled: usize,
}

impl<R: Read> Buffered<R> {
    pub fn new(inner: R) -> Self {
        Self::with_capacity(BUFFER_CAPACITY, inner)
    }

    pub fn with_capacity(capacity: usize, inner: R) -> Self {
        Self {
            inner,
            buf: vec![0; capacity.max(1)].into_boxed_slice(),
            start: 0,
            filled: 0,
        }
    }
}

impl<R: Read> Read for Buffered<R> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        if self.start == self.filled {
            // Requests at least as large as the buffer bypass it straight to the source.
            if buf.len() >= self.buf.len() {
                return self.inner.read(buf).await;
            }
            self.start = 0;
            self.filled = 0;
            self.filled = self.inner.read(&mut self.buf).await?;
            if self.filled == 0 {
                return Ok(0);
            }
        }
        let mut pending = &self.buf[self.start..self.filled];
        let n = read_from_slice(&mut pending, buf);
        self.start += n;
        Ok(n)
    }
}

#[cfg(any(feature = "std-io", test))]
mod bridge {
    use super::{IoError, Read, Seek, SeekFrom};

    /// Portable view of a std reader. `ErrorKind::Interrupted` is retried, so `Ok(0)` keeps the
    /// end-of-input meaning the portable contract requires.
    ///
    /// The wrapped reader is driven on the calling thread: this bridge is for sources whose
    /// reads complete without meaningfully blocking (in-memory std readers, decompressors over
    /// in-memory input). Host files go through the native artifact reader, which suspends
    /// properly.
    #[derive(Debug, Clone)]
    pub struct StdReader<R>(pub R);

    impl<R: std::io::Read> Read for StdReader<R> {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
            loop {
                match self.0.read(buf) {
                    Ok(n) => return Ok(n),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                        return Err(IoError::UnexpectedEof);
                    }
                    Err(error) => return Err(IoError::Failed(error.to_string())),
                }
            }
        }
    }

    impl<S: std::io::Seek> Seek for StdReader<S> {
        async fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError> {
            let pos = match pos {
                SeekFrom::Start(offset) => std::io::SeekFrom::Start(offset),
                SeekFrom::End(offset) => std::io::SeekFrom::End(offset),
                SeekFrom::Current(offset) => std::io::SeekFrom::Current(offset),
            };
            self.0
                .seek(pos)
                .map_err(|error| IoError::Failed(error.to_string()))
        }
    }
}

#[cfg(any(feature = "std-io", test))]
pub use bridge::StdReader;

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use jals_exec::block_on_inline;

    use super::*;

    #[test]
    fn slice_reads_consume_from_the_front() {
        block_on_inline(async {
            let mut source: &[u8] = &[1, 2, 3, 4, 5];
            let mut buf = [0; 2];
            assert_eq!(source.read(&mut buf).await, Ok(2));
            assert_eq!(buf, [1, 2]);
            assert_eq!(source, &[3, 4, 5]);
            let mut rest = [0; 8];
            assert_eq!(source.read(&mut rest).await, Ok(3));
            assert_eq!(source.read(&mut rest).await, Ok(0));
        });
    }

    #[test]
    fn read_exact_reports_truncation() {
        block_on_inline(async {
            let mut source: &[u8] = &[1, 2];
            let mut buf = [0; 4];
            assert_eq!(
                source.read_exact(&mut buf).await,
                Err(IoError::UnexpectedEof)
            );
        });
    }

    #[test]
    fn cursor_reads_and_seeks() {
        block_on_inline(async {
            let mut cursor = Cursor::new([10u8, 11, 12, 13]);
            let mut buf = [0; 3];
            cursor.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, [10, 11, 12]);
            assert_eq!(cursor.seek(SeekFrom::Start(1)).await, Ok(1));
            assert_eq!(cursor.read(&mut buf).await, Ok(3));
            assert_eq!(buf, [11, 12, 13]);
            assert_eq!(cursor.seek(SeekFrom::End(-2)).await, Ok(2));
            assert_eq!(cursor.stream_position().await, Ok(2));
            assert_eq!(cursor.seek(SeekFrom::Current(1)).await, Ok(3));
            assert_eq!(cursor.read(&mut buf).await, Ok(1));
            assert_eq!(buf[0], 13);
        });
    }

    #[test]
    fn cursor_allows_seeking_past_the_end_but_not_before_the_start() {
        block_on_inline(async {
            let mut cursor = Cursor::new([1u8, 2]);
            assert_eq!(cursor.seek(SeekFrom::Start(10)).await, Ok(10));
            let mut buf = [0; 1];
            assert_eq!(cursor.read(&mut buf).await, Ok(0));
            assert!(matches!(
                cursor.seek(SeekFrom::Current(-11)).await,
                Err(IoError::Failed(_))
            ));
        });
    }

    /// Yields one byte per call so buffer refills and short reads are exercised.
    struct OneByteAtATime(Vec<u8>);

    impl Read for OneByteAtATime {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
            if self.0.is_empty() || buf.is_empty() {
                return Ok(0);
            }
            buf[0] = self.0.remove(0);
            Ok(1)
        }
    }

    #[test]
    fn buffered_reads_across_refill_boundaries() {
        block_on_inline(async {
            let data: Vec<u8> = (0..=255).collect();
            let mut buffered = Buffered::with_capacity(7, OneByteAtATime(data.clone()));
            let mut out = Vec::new();
            let mut chunk = [0; 5];
            loop {
                match buffered.read(&mut chunk).await.unwrap() {
                    0 => break,
                    n => out.extend_from_slice(&chunk[..n]),
                }
            }
            assert_eq!(out, data);
        });
    }

    #[test]
    fn buffered_bypasses_the_buffer_for_large_requests() {
        block_on_inline(async {
            let mut buffered = Buffered::with_capacity(2, [1u8, 2, 3, 4].as_slice());
            let mut buf = [0; 4];
            // As large as the buffer: served straight from the source in one call.
            assert_eq!(buffered.read(&mut buf).await, Ok(4));
            assert_eq!(buf, [1, 2, 3, 4]);
        });
    }

    #[test]
    fn buffered_error_does_not_leave_stale_bytes() {
        struct FailAfter(usize);
        impl Read for FailAfter {
            async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
                if self.0 == 0 {
                    return Err(IoError::Failed(String::from("boom")));
                }
                self.0 -= 1;
                buf[0] = 42;
                Ok(1)
            }
        }
        block_on_inline(async {
            let mut buffered = Buffered::with_capacity(4, FailAfter(1));
            let mut buf = [0; 1];
            assert_eq!(buffered.read(&mut buf).await, Ok(1));
            assert!(buffered.read(&mut buf).await.is_err());
            assert!(buffered.read(&mut buf).await.is_err());
        });
    }

    #[test]
    fn std_bridge_reads_and_seeks() {
        block_on_inline(async {
            let bytes = [1u8, 2, 3, 4, 5];
            let std_reader = std::io::Cursor::new(bytes);
            let mut portable = StdReader(std_reader);
            let mut buf = [0; 5];
            portable.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, bytes);
            assert_eq!(portable.seek(SeekFrom::End(-1)).await, Ok(4));
            assert_eq!(portable.read(&mut buf).await, Ok(1));
            assert_eq!(buf[0], 5);
        });
    }
}
