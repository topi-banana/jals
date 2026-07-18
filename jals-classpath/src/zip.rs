//! In-house, read-only zip decoding over portable byte streams.
//!
//! jals only ever *reads* jars: enumerate members, stream one member, materialize one member.
//! Everything is driven off the central directory, so sizes are always known up front and flag
//! bit 3 data descriptors never need parsing. Only the two methods jars actually use are
//! supported — stored (0) and deflate (8); an encrypted or otherwise-compressed member is a
//! per-member diagnostic at open time, never an archive-level failure.
//!
//! The whole module operates on [`jals_storage::io`] readers (`no_std + alloc`), decompressing
//! through `miniz_oxide` and verifying member crc32s with `crc32fast`.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use jals_exec::Yielder;
use jals_storage::io::{self as sio, IoError, SeekFrom};
use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZFlush, MZStatus};

const LOCAL_HEADER_SIG: u32 = 0x0403_4b50; // PK\x03\x04
const CENTRAL_HEADER_SIG: u32 = 0x0201_4b50; // PK\x01\x02
const EOCD_SIG: u32 = 0x0605_4b50; // PK\x05\x06
const EOCD64_LOCATOR_SIG: u32 = 0x0706_4b50; // PK\x06\x07
const EOCD64_SIG: u32 = 0x0606_4b50; // PK\x06\x06

/// Fixed sizes of the records above (without variable-length trailers).
const EOCD_LEN: usize = 22;
const EOCD64_LOCATOR_LEN: usize = 20;
const EOCD64_LEN: usize = 56;
const CENTRAL_HEADER_LEN: usize = 46;
const LOCAL_HEADER_LEN: usize = 30;

/// The compressed refill window a [`MemberStream`] reads through.
const COMPRESSED_WINDOW: usize = 64 * 1024;

/// Little-endian field accessors shared by the record parsers.
mod le {
    pub(super) fn u16le(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    pub(super) fn u32le(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    pub(super) fn u64le(bytes: &[u8], at: usize) -> u64 {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&bytes[at..at + 8]);
        u64::from_le_bytes(buf)
    }

    pub(super) fn read_failed(error: &super::IoError) -> alloc::string::String {
        alloc::string::ToString::to_string(error)
    }
}

use le::{read_failed, u16le, u32le, u64le};

/// One central-directory entry, with any zip64 extra-field values already folded in.
#[derive(Debug, Clone)]
pub(crate) struct MemberRecord {
    /// The raw member name, `/`-separated as stored.
    pub(crate) name: String,
    /// General-purpose flag bits; bit 0 marks an encrypted member.
    flags: u16,
    /// Compression method: only stored (0) and deflate (8) are readable.
    method: u16,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    /// Offset of the member's local file header from the start of the archive.
    header_offset: u64,
    /// Whether the entry is a directory (trailing `/`).
    pub(crate) is_dir: bool,
}

impl MemberRecord {
    const fn is_encrypted(&self) -> bool {
        self.flags & 0x0001 != 0
    }
}

/// A parsed central directory: the plain-data member table shared (behind an `Arc`) by every
/// decode worker.
#[derive(Debug)]
pub(crate) struct CentralDirectory {
    pub(crate) members: Vec<MemberRecord>,
}

impl CentralDirectory {
    /// Parse the central directory of the archive behind `reader`.
    ///
    /// Locates the last valid end-of-central-directory record inside the final
    /// `22 + 65535` bytes (a candidate is valid only when its comment length reaches exactly
    /// the end of the file), follows the zip64 locator/record when any EOCD field carries a
    /// sentinel value, then makes one forward pass over the entries.
    pub(crate) async fn parse<R: sio::Read + sio::Seek>(reader: &mut R) -> Result<Self, String> {
        let len = reader
            .seek(SeekFrom::End(0))
            .await
            .map_err(|error| read_failed(&error))?;
        if len < EOCD_LEN as u64 {
            return Err("not a zip archive (too short)".to_owned());
        }

        // The EOCD lives in the last 22 + comment (≤ 65535) bytes.
        let window_len = len.min(EOCD_LEN as u64 + u64::from(u16::MAX));
        let window_start = len - window_len;
        reader
            .seek(SeekFrom::Start(window_start))
            .await
            .map_err(|error| read_failed(&error))?;
        let mut tail = vec![0u8; usize::try_from(window_len).expect("window fits usize")];
        reader
            .read_exact(&mut tail)
            .await
            .map_err(|error| read_failed(&error))?;

        let eocd_local = Self::find_eocd(&tail)
            .ok_or_else(|| "not a zip archive (no end-of-central-directory record)".to_owned())?;
        let eocd = &tail[eocd_local..];
        let eocd_offset = window_start + eocd_local as u64;

        let disk = u16le(eocd, 4);
        let cd_disk = u16le(eocd, 6);
        let raw_total = u16le(eocd, 10);
        let raw_cd_size = u32le(eocd, 12);
        let raw_cd_offset = u32le(eocd, 16);

        let needs_zip64 = disk == u16::MAX
            || cd_disk == u16::MAX
            || raw_total == u16::MAX
            || raw_cd_size == u32::MAX
            || raw_cd_offset == u32::MAX;

        let (total, cd_size, cd_offset) = if needs_zip64 {
            Self::read_zip64_directory_location(reader, eocd_offset).await?
        } else {
            if disk != 0 || cd_disk != 0 {
                return Err("multi-disk archives are not supported".to_owned());
            }
            (
                u64::from(raw_total),
                u64::from(raw_cd_size),
                u64::from(raw_cd_offset),
            )
        };

        if cd_offset.checked_add(cd_size).is_none_or(|end| end > len) {
            return Err("central directory is truncated".to_owned());
        }

        reader
            .seek(SeekFrom::Start(cd_offset))
            .await
            .map_err(|error| read_failed(&error))?;
        let mut directory =
            vec![0u8; usize::try_from(cd_size).map_err(|_| "central directory is too large")?];
        reader
            .read_exact(&mut directory)
            .await
            .map_err(|error| read_failed(&error))?;

        let members = Self::parse_entries(&directory).await?;
        if members.len() as u64 != total {
            return Err(format!(
                "central directory entry count mismatch: expected {total}, found {}",
                members.len()
            ));
        }
        Ok(Self { members })
    }

    /// The offset (within `tail`) of the last EOCD record whose comment length is consistent
    /// with the end of the file.
    fn find_eocd(tail: &[u8]) -> Option<usize> {
        let mut at = tail.len().checked_sub(EOCD_LEN)?;
        loop {
            if u32le(tail, at) == EOCD_SIG
                && usize::from(u16le(tail, at + 20)) == tail.len() - at - EOCD_LEN
            {
                return Some(at);
            }
            if at == 0 {
                return None;
            }
            at -= 1;
        }
    }

    /// Follow the zip64 EOCD locator (immediately before the EOCD) to the zip64 EOCD record and
    /// return `(total entries, cd size, cd offset)`.
    async fn read_zip64_directory_location<R: sio::Read + sio::Seek>(
        reader: &mut R,
        eocd_offset: u64,
    ) -> Result<(u64, u64, u64), String> {
        let Some(locator_offset) = eocd_offset.checked_sub(EOCD64_LOCATOR_LEN as u64) else {
            return Err("zip64 end-of-central-directory locator is missing".to_owned());
        };
        reader
            .seek(SeekFrom::Start(locator_offset))
            .await
            .map_err(|error| read_failed(&error))?;
        let mut locator = [0u8; EOCD64_LOCATOR_LEN];
        reader
            .read_exact(&mut locator)
            .await
            .map_err(|error| read_failed(&error))?;
        if u32le(&locator, 0) != EOCD64_LOCATOR_SIG {
            return Err("zip64 end-of-central-directory locator is missing".to_owned());
        }
        if u32le(&locator, 4) != 0 || u32le(&locator, 16) > 1 {
            return Err("multi-disk archives are not supported".to_owned());
        }

        reader
            .seek(SeekFrom::Start(u64le(&locator, 8)))
            .await
            .map_err(|error| read_failed(&error))?;
        let mut record = [0u8; EOCD64_LEN];
        reader
            .read_exact(&mut record)
            .await
            .map_err(|error| read_failed(&error))?;
        if u32le(&record, 0) != EOCD64_SIG {
            return Err("zip64 end-of-central-directory record is malformed".to_owned());
        }
        if u32le(&record, 16) != 0 || u32le(&record, 20) != 0 {
            return Err("multi-disk archives are not supported".to_owned());
        }
        Ok((u64le(&record, 32), u64le(&record, 40), u64le(&record, 48)))
    }

    /// One forward pass over the raw central-directory bytes.
    async fn parse_entries(directory: &[u8]) -> Result<Vec<MemberRecord>, String> {
        let mut members = Vec::new();
        let mut at = 0usize;
        let mut yielder = Yielder::new();
        while at < directory.len() {
            if directory.len() - at < CENTRAL_HEADER_LEN
                || u32le(directory, at) != CENTRAL_HEADER_SIG
            {
                return Err("central directory is malformed".to_owned());
            }
            let flags = u16le(directory, at + 8);
            let method = u16le(directory, at + 10);
            let crc32 = u32le(directory, at + 16);
            let raw_compressed = u32le(directory, at + 20);
            let raw_uncompressed = u32le(directory, at + 24);
            let name_len = usize::from(u16le(directory, at + 28));
            let extra_len = usize::from(u16le(directory, at + 30));
            let comment_len = usize::from(u16le(directory, at + 32));
            let raw_header_offset = u32le(directory, at + 42);

            let name_start = at + CENTRAL_HEADER_LEN;
            let extra_start = name_start + name_len;
            let entry_end = extra_start + extra_len + comment_len;
            if entry_end > directory.len() {
                return Err("central directory is malformed".to_owned());
            }
            let name = String::from_utf8_lossy(&directory[name_start..extra_start]).into_owned();

            let mut compressed_size = u64::from(raw_compressed);
            let mut uncompressed_size = u64::from(raw_uncompressed);
            let mut header_offset = u64::from(raw_header_offset);
            Self::fold_zip64_extra(
                &directory[extra_start..extra_start + extra_len],
                (raw_uncompressed == u32::MAX).then_some(&mut uncompressed_size),
                (raw_compressed == u32::MAX).then_some(&mut compressed_size),
                (raw_header_offset == u32::MAX).then_some(&mut header_offset),
            )?;

            let is_dir = name.ends_with('/');
            members.push(MemberRecord {
                name,
                flags,
                method,
                crc32,
                compressed_size,
                uncompressed_size,
                header_offset,
                is_dir,
            });
            at = entry_end;
            yielder.tick().await;
        }
        Ok(members)
    }

    /// Fold a zip64 extended-information extra field (id `0x0001`) into an entry's sizes/offset.
    /// Per the spec, the field carries values only for the header fields that hold the sentinel,
    /// in fixed order: uncompressed size, compressed size, local header offset (then disk number,
    /// which is irrelevant on a single-disk archive).
    fn fold_zip64_extra<'a>(
        extra: &[u8],
        mut uncompressed: Option<&'a mut u64>,
        mut compressed: Option<&'a mut u64>,
        mut header_offset: Option<&'a mut u64>,
    ) -> Result<(), String> {
        let mut at = 0usize;
        while at + 4 <= extra.len() {
            let id = u16le(extra, at);
            let size = usize::from(u16le(extra, at + 2));
            let data_start = at + 4;
            let data_end = data_start + size;
            if data_end > extra.len() {
                // A malformed trailing extra field; nothing beyond it can be parsed.
                break;
            }
            if id == 0x0001 {
                let mut data = &extra[data_start..data_end];
                for slot in [&mut uncompressed, &mut compressed, &mut header_offset] {
                    if let Some(value) = slot.take() {
                        if data.len() < 8 {
                            return Err("zip64 extra field is truncated".to_owned());
                        }
                        *value = u64le(data, 0);
                        data = &data[8..];
                    }
                }
                return Ok(());
            }
            at = data_end;
        }
        if uncompressed.is_some() || compressed.is_some() || header_offset.is_some() {
            return Err("zip64 sizes are missing from the extra field".to_owned());
        }
        Ok(())
    }
}

/// A streaming reader over one archive member's uncompressed bytes.
///
/// Owns its reader (callers hand in a clone; every clone of a cache reader reads at an
/// independent position). Opening seeks to the member's local header, verifies the signature,
/// and skips the *local* name/extra lengths (they can legitimately differ from the central
/// directory's). Reading refills a fixed compressed window from the source and inflates through
/// `miniz_oxide` (stored members pass straight through), hashing every produced byte; at end of
/// member the crc32 and uncompressed size are verified and a mismatch is a read error.
pub(crate) struct MemberStream<R> {
    source: R,
    /// `None` for a stored member; the raw-deflate state otherwise.
    inflater: Option<Box<InflateState>>,
    compressed_remaining: u64,
    uncompressed_remaining: u64,
    expected_crc: u32,
    hasher: crc32fast::Hasher,
    window: Box<[u8]>,
    window_start: usize,
    window_filled: usize,
    verified: bool,
}

impl<R: sio::Read + sio::Seek> MemberStream<R> {
    /// Open `member` for reading. An encrypted member, an unsupported compression method, and a
    /// malformed local header are all per-member diagnostics.
    pub(crate) async fn open(mut source: R, member: &MemberRecord) -> Result<Self, String> {
        if member.is_encrypted() {
            return Err(format!(
                "skipped encrypted archive member `{}`",
                member.name
            ));
        }
        let inflater = match member.method {
            0 => None,
            8 => Some(InflateState::new_boxed(DataFormat::Raw)),
            method => {
                return Err(format!(
                    "skipped archive member `{}` with unsupported compression method {method}",
                    member.name
                ));
            }
        };

        source
            .seek(SeekFrom::Start(member.header_offset))
            .await
            .map_err(|error| read_failed(&error))?;
        let mut header = [0u8; LOCAL_HEADER_LEN];
        source
            .read_exact(&mut header)
            .await
            .map_err(|error| read_failed(&error))?;
        if u32le(&header, 0) != LOCAL_HEADER_SIG {
            return Err(format!(
                "archive member `{}` has a malformed local header",
                member.name
            ));
        }
        // The local header's own name/extra lengths locate the data start; they are allowed to
        // differ from the central directory's copies.
        let name_len = i64::from(u16le(&header, 26));
        let extra_len = i64::from(u16le(&header, 28));
        source
            .seek(SeekFrom::Current(name_len + extra_len))
            .await
            .map_err(|error| read_failed(&error))?;

        let window = usize::try_from(member.compressed_size.min(COMPRESSED_WINDOW as u64))
            .expect("window fits usize")
            .max(1);
        Ok(Self {
            source,
            inflater,
            compressed_remaining: member.compressed_size,
            uncompressed_remaining: member.uncompressed_size,
            expected_crc: member.crc32,
            hasher: crc32fast::Hasher::new(),
            window: vec![0u8; window].into_boxed_slice(),
            window_start: 0,
            window_filled: 0,
            verified: false,
        })
    }

    /// Refill the compressed window from the source. Returns an error when the source ends
    /// before the declared compressed size arrived.
    async fn refill(&mut self) -> Result<(), IoError> {
        let want = usize::try_from(self.compressed_remaining.min(self.window.len() as u64))
            .expect("window fits usize");
        let n = self.source.read(&mut self.window[..want]).await?;
        if n == 0 {
            return Err(IoError::UnexpectedEof);
        }
        self.window_start = 0;
        self.window_filled = n;
        self.compressed_remaining -= n as u64;
        Ok(())
    }

    /// End-of-member verification: crc32 over everything produced must match the directory.
    /// (The size half is implicit — production is capped at the declared uncompressed size and
    /// a short stream errors before reaching this point.)
    fn verify_end(&mut self) -> Result<(), IoError> {
        if self.verified {
            return Ok(());
        }
        self.verified = true;
        let actual = self.hasher.clone().finalize();
        if actual != self.expected_crc {
            return Err(IoError::Failed(format!(
                "archive member crc32 mismatch: expected {:08x}, got {actual:08x}",
                self.expected_crc
            )));
        }
        Ok(())
    }

    async fn read_stored(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        if self.window_start == self.window_filled {
            self.refill().await?;
        }
        let pending = &self.window[self.window_start..self.window_filled];
        let n = pending.len().min(buf.len());
        buf[..n].copy_from_slice(&pending[..n]);
        self.window_start += n;
        Ok(n)
    }

    async fn read_deflate(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        loop {
            if self.window_start == self.window_filled && self.compressed_remaining > 0 {
                self.refill().await?;
            }
            let input_done = self.window_start == self.window_filled;
            let flush = if input_done {
                MZFlush::Finish
            } else {
                MZFlush::None
            };
            let state = self
                .inflater
                .as_mut()
                .expect("deflate reads only happen with an inflater");
            let result = inflate(
                state,
                &self.window[self.window_start..self.window_filled],
                buf,
                flush,
            );
            self.window_start += result.bytes_consumed;
            let status = result.status.map_err(|error| {
                IoError::Failed(format!(
                    "archive member deflate stream is corrupt: {error:?}"
                ))
            })?;
            if result.bytes_written > 0 {
                return Ok(result.bytes_written);
            }
            if status == MZStatus::StreamEnd {
                // The deflate stream ended with declared bytes still owed.
                return Err(IoError::Failed(
                    "archive member is shorter than its directory entry".into(),
                ));
            }
            if input_done && result.bytes_consumed == 0 {
                return Err(IoError::Failed(
                    "archive member deflate stream made no progress".into(),
                ));
            }
        }
    }
}

impl<R: sio::Read + sio::Seek> sio::Read for MemberStream<R> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.uncompressed_remaining == 0 {
            self.verify_end()?;
            return Ok(0);
        }
        // Never ask for more than the member owes: producing beyond the declared size is
        // structural corruption, and capping the request makes it impossible.
        let cap = usize::try_from(self.uncompressed_remaining.min(buf.len() as u64))
            .expect("read caps at buf.len()");
        let n = if self.inflater.is_some() {
            self.read_deflate(&mut buf[..cap]).await?
        } else {
            self.read_stored(&mut buf[..cap]).await?
        };
        self.hasher.update(&buf[..n]);
        self.uncompressed_remaining -= n as u64;
        Ok(n)
    }
}

#[cfg(test)]
// The hand-built zip64 fixture writes known-small lengths into fixed-width header fields.
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use std::io::Write;

    use jals_exec::block_on_inline;
    use jals_storage::io::{Cursor, Read as _};

    use super::*;

    /// Build a jar with the `zip` crate — the cross-verification oracle. Each entry picks its
    /// own compression method.
    fn oracle_jar(entries: &[(&str, &[u8], zip::CompressionMethod)]) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut bytes);
        for (name, contents, method) in entries {
            let options = zip::write::SimpleFileOptions::default().compression_method(*method);
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
        bytes.into_inner()
    }

    fn parse(bytes: &[u8]) -> CentralDirectory {
        block_on_inline(CentralDirectory::parse(&mut Cursor::new(bytes)))
            .expect("oracle jar parses")
    }

    fn read_member_bytes(archive: &[u8], member: &MemberRecord) -> Vec<u8> {
        block_on_inline(async {
            let mut stream = MemberStream::open(Cursor::new(archive), member)
                .await
                .expect("member opens");
            let mut out = Vec::new();
            let mut chunk = [0u8; 173]; // odd size to exercise partial reads
            loop {
                match stream.read(&mut chunk).await.expect("member reads") {
                    0 => return out,
                    n => out.extend_from_slice(&chunk[..n]),
                }
            }
        })
    }

    /// Names and byte-identical contents must match what the `zip` crate reads back from the
    /// same archive, across stored and deflated members and nested directories.
    #[test]
    fn cross_verifies_against_the_zip_crate() {
        let payload: Vec<u8> = (0u32..40_000).flat_map(u32::to_le_bytes).collect();
        let entries: &[(&str, &[u8], zip::CompressionMethod)] = &[
            ("a.txt", b"stored bytes", zip::CompressionMethod::Stored),
            (
                "dir/nested/b.bin",
                &payload,
                zip::CompressionMethod::Deflated,
            ),
            ("dir/c.txt", b"", zip::CompressionMethod::Stored),
            (
                "d.class",
                b"\xca\xfe\xba\xbe rest",
                zip::CompressionMethod::Deflated,
            ),
            (
                "e.txt",
                b"another deflated member",
                zip::CompressionMethod::Deflated,
            ),
        ];
        let archive = oracle_jar(entries);

        let directory = parse(&archive);
        let mut oracle =
            zip::ZipArchive::new(std::io::Cursor::new(archive.clone())).expect("oracle opens");
        assert_eq!(directory.members.len(), oracle.len());
        for (index, member) in directory.members.iter().enumerate() {
            let mut expected = oracle.by_index(index).expect("oracle reads");
            assert_eq!(member.name, expected.name());
            assert_eq!(member.is_dir, expected.is_dir());
            let mut expected_bytes = Vec::new();
            std::io::copy(&mut expected, &mut expected_bytes).unwrap();
            assert_eq!(
                read_member_bytes(&archive, member),
                expected_bytes,
                "{}",
                member.name
            );
        }
    }

    /// An archive with more members than one decode chunk still enumerates completely and in
    /// writer order.
    #[test]
    fn enumerates_many_members_in_order() {
        let contents: Vec<(String, Vec<u8>)> = (0..300)
            .map(|n| {
                (
                    format!("pkg/f{n:03}.txt"),
                    format!("member {n}").into_bytes(),
                )
            })
            .collect();
        let entries: Vec<(&str, &[u8], zip::CompressionMethod)> = contents
            .iter()
            .map(|(name, bytes)| {
                (
                    name.as_str(),
                    bytes.as_slice(),
                    zip::CompressionMethod::Deflated,
                )
            })
            .collect();
        let archive = oracle_jar(&entries);
        let directory = parse(&archive);
        assert_eq!(directory.members.len(), 300);
        for (member, (name, bytes)) in directory.members.iter().zip(&contents) {
            assert_eq!(&member.name, name);
            assert_eq!(&read_member_bytes(&archive, member), bytes);
        }
    }

    /// A comment on the archive must not confuse the EOCD scan (the last *consistent* record
    /// wins).
    #[test]
    fn finds_the_eocd_behind_a_comment() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut bytes);
        writer
            .set_comment("trailing archive comment PK\x05\x06 with a decoy signature")
            .unwrap();
        writer
            .start_file("x.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"payload").unwrap();
        writer.finish().unwrap();
        let archive = bytes.into_inner();
        let directory = parse(&archive);
        assert_eq!(directory.members.len(), 1);
        assert_eq!(
            read_member_bytes(&archive, &directory.members[0]),
            b"payload"
        );
    }

    #[test]
    fn rejects_non_zip_bytes_structurally() {
        for bytes in [&b"not a zip archive at all"[..], &b""[..], &[0u8; 21]] {
            assert!(
                block_on_inline(CentralDirectory::parse(&mut Cursor::new(bytes))).is_err(),
                "{bytes:?}"
            );
        }
    }

    /// A corrupted deflate stream (or a wrong crc) is a member-level read error, never a panic.
    #[test]
    fn corrupt_member_is_a_read_error() {
        let archive = oracle_jar(&[(
            "a.bin",
            &(0u32..10_000)
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<u8>>(),
            zip::CompressionMethod::Deflated,
        )]);
        let directory = parse(&archive);
        let mut tampered = archive;
        // Flip bytes in the middle of the compressed data (after the local header).
        for byte in &mut tampered[60..80] {
            *byte ^= 0x5a;
        }
        let outcome = block_on_inline(async {
            let mut stream = MemberStream::open(Cursor::new(tampered), &directory.members[0])
                .await
                .expect("open still succeeds; corruption is in the stream");
            let mut sink = [0u8; 4096];
            loop {
                match stream.read(&mut sink).await {
                    Ok(0) => return Ok(()),
                    Ok(_) => {}
                    Err(error) => return Err(error),
                }
            }
        });
        assert!(outcome.is_err(), "{outcome:?}");
    }

    /// Hand-built minimal zip64 archive: sentinel EOCD fields, a zip64 locator + record, and a
    /// central-directory entry whose sizes/offset live in the 0x0001 extra field.
    #[test]
    fn parses_a_zip64_archive() {
        let name = b"a.txt";
        let contents = b"hello";
        let crc = crc32fast::hash(contents);
        let mut bytes = Vec::new();

        // Local header at offset 0.
        bytes.extend_from_slice(&LOCAL_HEADER_SIG.to_le_bytes());
        bytes.extend_from_slice(&45u16.to_le_bytes()); // version needed
        bytes.extend_from_slice(&0u16.to_le_bytes()); // flags
        bytes.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        bytes.extend_from_slice(&[0u8; 4]); // time + date
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes()); // extra len
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(contents);

        // Central directory entry with zip64 sentinels.
        let cd_offset = bytes.len() as u64;
        bytes.extend_from_slice(&CENTRAL_HEADER_SIG.to_le_bytes());
        bytes.extend_from_slice(&45u16.to_le_bytes()); // version made by
        bytes.extend_from_slice(&45u16.to_le_bytes()); // version needed
        bytes.extend_from_slice(&0u16.to_le_bytes()); // flags
        bytes.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        bytes.extend_from_slice(&[0u8; 4]); // time + date
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // compressed: sentinel
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // uncompressed: sentinel
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&28u16.to_le_bytes()); // extra len: 4 + 24
        bytes.extend_from_slice(&0u16.to_le_bytes()); // comment len
        bytes.extend_from_slice(&0u16.to_le_bytes()); // disk start
        bytes.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        bytes.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // header offset: sentinel
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(&0x0001u16.to_le_bytes()); // zip64 extra id
        bytes.extend_from_slice(&24u16.to_le_bytes()); // zip64 extra size
        bytes.extend_from_slice(&(contents.len() as u64).to_le_bytes()); // uncompressed
        bytes.extend_from_slice(&(contents.len() as u64).to_le_bytes()); // compressed
        bytes.extend_from_slice(&0u64.to_le_bytes()); // header offset
        let cd_size = bytes.len() as u64 - cd_offset;

        // Zip64 EOCD record.
        let record_offset = bytes.len() as u64;
        bytes.extend_from_slice(&EOCD64_SIG.to_le_bytes());
        bytes.extend_from_slice(&44u64.to_le_bytes()); // size of remainder
        bytes.extend_from_slice(&45u16.to_le_bytes()); // version made by
        bytes.extend_from_slice(&45u16.to_le_bytes()); // version needed
        bytes.extend_from_slice(&0u32.to_le_bytes()); // disk
        bytes.extend_from_slice(&0u32.to_le_bytes()); // cd start disk
        bytes.extend_from_slice(&1u64.to_le_bytes()); // entries on disk
        bytes.extend_from_slice(&1u64.to_le_bytes()); // total entries
        bytes.extend_from_slice(&cd_size.to_le_bytes());
        bytes.extend_from_slice(&cd_offset.to_le_bytes());

        // Zip64 EOCD locator.
        bytes.extend_from_slice(&EOCD64_LOCATOR_SIG.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // disk with the record
        bytes.extend_from_slice(&record_offset.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // total disks

        // EOCD with sentinel fields.
        bytes.extend_from_slice(&EOCD_SIG.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes()); // disk
        bytes.extend_from_slice(&0u16.to_le_bytes()); // cd disk
        bytes.extend_from_slice(&u16::MAX.to_le_bytes()); // entries on disk: sentinel
        bytes.extend_from_slice(&u16::MAX.to_le_bytes()); // total entries: sentinel
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // cd size: sentinel
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // cd offset: sentinel
        bytes.extend_from_slice(&0u16.to_le_bytes()); // comment len

        let directory = parse(&bytes);
        assert_eq!(directory.members.len(), 1);
        let member = &directory.members[0];
        assert_eq!(member.name, "a.txt");
        assert_eq!(member.uncompressed_size, contents.len() as u64);
        assert_eq!(member.header_offset, 0);
        assert_eq!(read_member_bytes(&bytes, member), contents);
    }
}
