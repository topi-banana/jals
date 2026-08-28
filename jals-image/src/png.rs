//! The PNG subset a screenshot is written in, both directions.
//!
//! **Read** accepts 8-bit greyscale, greyscale+alpha, RGB and RGBA, non-interlaced, and widens all
//! four to [`Image`]'s RGBA. **Write** emits exactly one shape — 8-bit RGBA, non-interlaced, one
//! `IDAT` — because the only thing this crate writes is a difference image it also reads back.
//!
//! Everything outside that subset is a named error rather than a best-effort guess. A palette
//! image, a 16-bit image, an interlaced image and an unsupported filter each say so: a screenshot
//! comparison that silently mis-decoded one channel would report a difference that is not there,
//! which is worse than refusing the file.
//!
//! Ancillary chunks are skipped. `IEND` is required, so a truncated stream is refused rather than
//! decoded up to where it stopped — a half-written screenshot must not read as a valid one.

use alloc::vec;
use alloc::vec::Vec;

use crate::image::Image;

/// The eight bytes every PNG starts with (PNG 1.2 §5.2).
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

const IHDR: &[u8; 4] = b"IHDR";
const IDAT: &[u8; 4] = b"IDAT";
const IEND: &[u8; 4] = b"IEND";

/// The one bit depth this codec reads or writes.
const BIT_DEPTH: u8 = 8;

/// Deflate level for a written `IDAT`. 6 is zlib's own default: the difference image is a
/// throwaway artifact a person looks at once, so the last few percent of ratio is not worth the
/// time.
const COMPRESSION_LEVEL: u8 = 6;

/// How a PNG's colour type lays its samples out, for the four this codec reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorType {
    /// Colour type 0: one grey sample.
    Grey,
    /// Colour type 2: red, green, blue.
    Rgb,
    /// Colour type 4: grey and alpha.
    GreyAlpha,
    /// Colour type 6: red, green, blue, alpha.
    Rgba,
}

impl ColorType {
    /// The colour type this codec writes.
    const OUTPUT: u8 = 6;

    const fn from_byte(byte: u8) -> Result<Self, PngError> {
        match byte {
            0 => Ok(Self::Grey),
            2 => Ok(Self::Rgb),
            4 => Ok(Self::GreyAlpha),
            6 => Ok(Self::Rgba),
            3 => Err(PngError::PaletteUnsupported),
            other => Err(PngError::ColorType(other)),
        }
    }

    /// Samples per pixel, which at 8 bits per sample is also bytes per pixel.
    const fn channels(self) -> usize {
        match self {
            Self::Grey => 1,
            Self::GreyAlpha => 2,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }

    /// Widen one pixel's samples to RGBA.
    const fn widen(self, samples: &[u8]) -> [u8; 4] {
        match self {
            Self::Grey => [samples[0], samples[0], samples[0], 0xFF],
            Self::GreyAlpha => [samples[0], samples[0], samples[0], samples[1]],
            Self::Rgb => [samples[0], samples[1], samples[2], 0xFF],
            Self::Rgba => [samples[0], samples[1], samples[2], samples[3]],
        }
    }
}

/// What `IHDR` declared.
#[derive(Debug, Clone, Copy)]
struct Header {
    width: u32,
    height: u32,
    color: ColorType,
}

impl Header {
    /// Bytes one unfiltered scanline occupies, its leading filter byte excluded.
    const fn stride(self) -> usize {
        self.width as usize * self.color.channels()
    }

    /// Bytes the whole inflated stream occupies: every scanline, each with one filter byte.
    const fn raw_len(self) -> usize {
        self.height as usize * (1 + self.stride())
    }
}

/// The PNG codec.
///
/// A unit struct rather than a module of functions, because a `pub fn` at a module's top level is
/// rejected workspace-wide; the two entry points hang off the type the format is named for.
#[derive(Debug, Clone, Copy)]
pub struct Png;

impl Png {
    /// Decode `bytes` into an RGBA image.
    ///
    /// # Errors
    /// [`PngError`] for a stream that is not a PNG, is outside the supported subset, fails its
    /// CRC, or does not reconstruct to the size `IHDR` declared.
    pub fn decode(bytes: &[u8]) -> Result<Image, PngError> {
        if bytes.len() < SIGNATURE.len() || bytes[..SIGNATURE.len()] != SIGNATURE {
            return Err(PngError::NotAPng);
        }
        let mut cursor = SIGNATURE.len();
        let mut header: Option<Header> = None;
        let mut compressed = Vec::new();
        let mut ended = false;

        while cursor < bytes.len() {
            let chunk = Chunk::read(bytes, cursor)?;
            match &chunk.kind {
                IHDR => {
                    if header.is_some() {
                        return Err(PngError::DuplicateHeader);
                    }
                    header = Some(Self::header(chunk.data)?);
                }
                IDAT => {
                    if header.is_none() {
                        return Err(PngError::MissingHeader);
                    }
                    compressed.extend_from_slice(chunk.data);
                }
                IEND => {
                    ended = true;
                    break;
                }
                _ => {}
            }
            cursor = chunk.end;
        }

        if !ended {
            return Err(PngError::Truncated);
        }
        let header = header.ok_or(PngError::MissingHeader)?;
        // The inflated length is known exactly from `IHDR`, so it is the decompression limit as
        // well as the expected size. A stream that would expand past it is refused before the
        // memory is committed rather than after.
        let raw =
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&compressed, header.raw_len())
                .map_err(|_| PngError::Deflate)?;
        if raw.len() != header.raw_len() {
            return Err(PngError::RawLength {
                expected: header.raw_len(),
                found: raw.len(),
            });
        }
        Self::reconstruct(header, &raw)
    }

    /// Encode `image` as 8-bit RGBA, non-interlaced.
    #[must_use]
    pub fn encode(image: &Image) -> Vec<u8> {
        let stride = image.width() as usize * Image::CHANNELS;
        // One filter byte per scanline, always `None`: the difference image is written once and
        // read once, and choosing a filter per row would trade encode time for a ratio nobody
        // measures.
        let mut raw = Vec::with_capacity(image.height() as usize * (1 + stride));
        for row in image.pixels().chunks_exact(stride) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&raw, COMPRESSION_LEVEL);

        let mut out = Vec::with_capacity(SIGNATURE.len() + 64 + compressed.len());
        out.extend_from_slice(&SIGNATURE);

        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&image.width().to_be_bytes());
        ihdr.extend_from_slice(&image.height().to_be_bytes());
        ihdr.push(BIT_DEPTH);
        ihdr.push(ColorType::OUTPUT);
        ihdr.extend_from_slice(&[0, 0, 0]); // deflate, adaptive filtering, no interlace
        Chunk::write(&mut out, *IHDR, &ihdr);
        Chunk::write(&mut out, *IDAT, &compressed);
        Chunk::write(&mut out, *IEND, &[]);
        out
    }

    /// Parse the 13 bytes of an `IHDR`.
    fn header(data: &[u8]) -> Result<Header, PngError> {
        if data.len() != 13 {
            return Err(PngError::MalformedHeader);
        }
        let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        if width == 0 || height == 0 {
            return Err(PngError::ZeroDimension);
        }
        if data[8] != BIT_DEPTH {
            return Err(PngError::BitDepth(data[8]));
        }
        let color = ColorType::from_byte(data[9])?;
        if data[12] != 0 {
            return Err(PngError::Interlaced);
        }
        Ok(Header {
            width,
            height,
            color,
        })
    }

    /// Undo the per-scanline filters and widen every pixel to RGBA.
    fn reconstruct(header: Header, raw: &[u8]) -> Result<Image, PngError> {
        let stride = header.stride();
        let channels = header.color.channels();
        let mut previous = vec![0u8; stride];
        let mut current = vec![0u8; stride];
        let mut pixels =
            Vec::with_capacity(header.width as usize * header.height as usize * Image::CHANNELS);

        for row in 0..header.height as usize {
            let at = row * (1 + stride);
            let filter = raw[at];
            current.copy_from_slice(&raw[at + 1..at + 1 + stride]);
            Filter::from_byte(filter)?.undo(&mut current, &previous, channels);
            for pixel in current.chunks_exact(channels) {
                pixels.extend_from_slice(&header.color.widen(pixel));
            }
            core::mem::swap(&mut previous, &mut current);
        }

        Image::from_rgba(header.width, header.height, pixels).map_err(|_| PngError::MalformedHeader)
    }
}

/// One chunk, borrowed out of the stream.
struct Chunk<'a> {
    kind: [u8; 4],
    data: &'a [u8],
    /// Offset just past this chunk's CRC — where the next chunk begins.
    end: usize,
}

impl<'a> Chunk<'a> {
    fn read(bytes: &'a [u8], at: usize) -> Result<Self, PngError> {
        let header_end = at.checked_add(8).ok_or(PngError::Truncated)?;
        if header_end > bytes.len() {
            return Err(PngError::Truncated);
        }
        let length =
            u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
        let data_end = header_end.checked_add(length).ok_or(PngError::Truncated)?;
        let end = data_end.checked_add(4).ok_or(PngError::Truncated)?;
        if end > bytes.len() {
            return Err(PngError::Truncated);
        }
        let kind = [bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]];
        let data = &bytes[header_end..data_end];
        let declared = u32::from_be_bytes([
            bytes[data_end],
            bytes[data_end + 1],
            bytes[data_end + 2],
            bytes[data_end + 3],
        ]);
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&kind);
        hasher.update(data);
        if hasher.finalize() != declared {
            return Err(PngError::Crc { kind });
        }
        Ok(Self { kind, data, end })
    }

    fn write(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(&kind);
        out.extend_from_slice(data);
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&kind);
        hasher.update(data);
        out.extend_from_slice(&hasher.finalize().to_be_bytes());
    }
}

/// A scanline filter (PNG 1.2 §6).
#[derive(Debug, Clone, Copy)]
enum Filter {
    None,
    Sub,
    Up,
    Average,
    Paeth,
}

impl Filter {
    const fn from_byte(byte: u8) -> Result<Self, PngError> {
        match byte {
            0 => Ok(Self::None),
            1 => Ok(Self::Sub),
            2 => Ok(Self::Up),
            3 => Ok(Self::Average),
            4 => Ok(Self::Paeth),
            other => Err(PngError::Filter(other)),
        }
    }

    /// Reconstruct `line` in place. `previous` is the already-reconstructed scanline above, all
    /// zeroes for the first row, and `bpp` the filter's byte offset to the pixel on the left.
    fn undo(self, line: &mut [u8], previous: &[u8], bpp: usize) {
        for i in 0..line.len() {
            let left = if i >= bpp { line[i - bpp] } else { 0 };
            let above = previous[i];
            let above_left = if i >= bpp { previous[i - bpp] } else { 0 };
            let addend = match self {
                Self::None => 0,
                Self::Sub => left,
                Self::Up => above,
                // The sum of two bytes does not fit in one, and PNG floors the average;
                // `midpoint` is exactly that, without the widening.
                Self::Average => u8::midpoint(left, above),
                Self::Paeth => Self::paeth(left, above, above_left),
            };
            line[i] = line[i].wrapping_add(addend);
        }
    }

    /// The Paeth predictor: whichever of `a`, `b`, `c` is closest to `a + b - c`.
    fn paeth(a: u8, b: u8, c: u8) -> u8 {
        let p = i16::from(a) + i16::from(b) - i16::from(c);
        let pa = (p - i16::from(a)).abs();
        let pb = (p - i16::from(b)).abs();
        let pc = (p - i16::from(c)).abs();
        if pa <= pb && pa <= pc {
            a
        } else if pb <= pc {
            b
        } else {
            c
        }
    }
}

/// Why a PNG could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PngError {
    /// The stream does not start with the PNG signature.
    NotAPng,
    /// A chunk header, body or CRC ran past the end of the stream, or `IEND` never arrived.
    Truncated,
    /// A chunk's stored CRC does not match its contents.
    Crc { kind: [u8; 4] },
    /// No `IHDR` preceded the image data.
    MissingHeader,
    /// More than one `IHDR`.
    DuplicateHeader,
    /// `IHDR` was not 13 bytes, or its dimensions do not match the data that followed.
    MalformedHeader,
    /// `IHDR` declared a zero width or height.
    ZeroDimension,
    /// A bit depth other than 8.
    BitDepth(u8),
    /// A colour type this codec does not read.
    ColorType(u8),
    /// A palette image. Reading one needs `PLTE`, which a screenshot never uses.
    PaletteUnsupported,
    /// An Adam7-interlaced image.
    Interlaced,
    /// A scanline filter byte outside 0..=4.
    Filter(u8),
    /// The `IDAT` stream is not valid zlib, or expands past the size `IHDR` implies.
    Deflate,
    /// The inflated stream is not the length `IHDR` implies.
    RawLength { expected: usize, found: usize },
}

impl core::fmt::Display for PngError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAPng => write!(f, "not a PNG: the file does not begin with the signature"),
            Self::Truncated => write!(f, "the PNG ends before IEND"),
            Self::Crc { kind } => {
                write!(f, "the `{}` chunk fails its CRC", Self::name(*kind))
            }
            Self::MissingHeader => write!(f, "the PNG has no IHDR"),
            Self::DuplicateHeader => write!(f, "the PNG has more than one IHDR"),
            Self::MalformedHeader => write!(f, "the PNG's IHDR is malformed"),
            Self::ZeroDimension => write!(f, "the PNG declares a zero width or height"),
            Self::BitDepth(depth) => {
                write!(
                    f,
                    "unsupported bit depth {depth}: only 8 bits per channel is read"
                )
            }
            Self::ColorType(kind) => write!(f, "unsupported PNG colour type {kind}"),
            Self::PaletteUnsupported => {
                write!(
                    f,
                    "a palette PNG is not read; a screenshot is greyscale, RGB or RGBA"
                )
            }
            Self::Interlaced => write!(f, "an interlaced PNG is not read"),
            Self::Filter(byte) => write!(f, "unsupported scanline filter {byte}"),
            Self::Deflate => write!(f, "the PNG's image data is not valid zlib"),
            Self::RawLength { expected, found } => write!(
                f,
                "the PNG's image data unpacks to {found} bytes, but IHDR implies {expected}"
            ),
        }
    }
}

impl PngError {
    /// A chunk type rendered for a message, with any non-ASCII byte shown as `?` so a corrupt
    /// stream cannot put arbitrary bytes into a diagnostic.
    fn name(kind: [u8; 4]) -> alloc::string::String {
        kind.iter()
            .map(|byte| {
                if byte.is_ascii_graphic() {
                    *byte as char
                } else {
                    '?'
                }
            })
            .collect()
    }
}

impl core::error::Error for PngError {}

#[cfg(test)]
mod tests {
    use super::{Png, PngError, SIGNATURE};
    use crate::image::{Image, Rgba};
    use alloc::vec::Vec;

    /// An image whose every pixel differs from its neighbours, so a filter bug cannot cancel out.
    fn checkerboard(width: u32, height: u32) -> Image {
        let mut image = Image::transparent(width, height);
        for y in 0..height {
            for x in 0..width {
                image.set(
                    x,
                    y,
                    Rgba::new(
                        (x * 7 % 256) as u8,
                        (y * 13 % 256) as u8,
                        ((x * y) % 256) as u8,
                        if (x + y) % 3 == 0 { 128 } else { 255 },
                    ),
                );
            }
        }
        image
    }

    #[test]
    fn encode_then_decode_reproduces_the_image() {
        let image = checkerboard(37, 19);
        let decoded = Png::decode(&Png::encode(&image)).expect("its own output decodes");
        assert_eq!(decoded, image);
    }

    #[test]
    fn a_single_pixel_round_trips() {
        let mut image = Image::transparent(1, 1);
        image.set(0, 0, Rgba::new(1, 2, 3, 4));
        assert_eq!(Png::decode(&Png::encode(&image)).expect("valid"), image);
    }

    #[test]
    fn bytes_that_are_not_a_png_are_refused() {
        assert_eq!(Png::decode(b"").unwrap_err(), PngError::NotAPng);
        assert_eq!(
            Png::decode(b"not a png at all").unwrap_err(),
            PngError::NotAPng
        );
    }

    #[test]
    fn a_stream_that_stops_before_iend_is_truncated_not_decoded() {
        let full = Png::encode(&checkerboard(8, 8));
        // Drop the 12-byte IEND chunk. Everything before it is intact and CRC-clean.
        let cut = &full[..full.len() - 12];
        assert_eq!(Png::decode(cut).unwrap_err(), PngError::Truncated);
    }

    #[test]
    fn a_corrupted_chunk_body_fails_its_crc() {
        let mut bytes = Png::encode(&checkerboard(8, 8));
        // The first chunk after the signature is IHDR; flip a bit in its width.
        bytes[SIGNATURE.len() + 8] ^= 0x01;
        assert!(matches!(Png::decode(&bytes), Err(PngError::Crc { .. })));
    }

    #[test]
    fn unsupported_shapes_are_named_rather_than_guessed() {
        let base = checkerboard(4, 4);
        let encoded = Png::encode(&base);
        // IHDR's data begins 8 bytes into the chunk, which itself begins after the signature.
        let ihdr = SIGNATURE.len() + 8;
        let repaired = |mut bytes: Vec<u8>| {
            let mut hasher = crc32fast::Hasher::new();
            hasher.update(&bytes[SIGNATURE.len() + 4..ihdr + 13]);
            let crc = hasher.finalize().to_be_bytes();
            bytes[ihdr + 13..ihdr + 17].copy_from_slice(&crc);
            bytes
        };

        let mut depth = encoded.clone();
        depth[ihdr + 8] = 16;
        assert_eq!(
            Png::decode(&repaired(depth)).unwrap_err(),
            PngError::BitDepth(16)
        );

        let mut palette = encoded.clone();
        palette[ihdr + 9] = 3;
        assert_eq!(
            Png::decode(&repaired(palette)).unwrap_err(),
            PngError::PaletteUnsupported
        );

        let mut interlaced = encoded.clone();
        interlaced[ihdr + 12] = 1;
        assert_eq!(
            Png::decode(&repaired(interlaced)).unwrap_err(),
            PngError::Interlaced
        );

        let mut color = encoded;
        color[ihdr + 9] = 7;
        assert_eq!(
            Png::decode(&repaired(color)).unwrap_err(),
            PngError::ColorType(7)
        );
    }

    #[test]
    fn every_scanline_filter_reconstructs_the_same_image() {
        // Re-encode the checkerboard's raw scanlines under each filter in turn and check all five
        // decode back to one image. This is the half `Png::encode` alone never exercises: it only
        // ever writes filter 0.
        let image = checkerboard(16, 9);
        for filter in 0..=4u8 {
            let bytes = super::tests::filtered(&image, filter);
            let decoded = Png::decode(&bytes).expect("a filtered PNG decodes");
            assert_eq!(decoded, image, "filter {filter} did not reconstruct");
        }
    }

    /// Encode `image` with every scanline carrying `filter`, applying that filter's forward
    /// transform — the inverse of what the decoder undoes.
    fn filtered(image: &Image, filter: u8) -> Vec<u8> {
        let stride = image.width() as usize * Image::CHANNELS;
        let bpp = Image::CHANNELS;
        let mut raw = Vec::new();
        let mut previous = alloc::vec![0u8; stride];
        for row in image.pixels().chunks_exact(stride) {
            raw.push(filter);
            for i in 0..stride {
                let left = if i >= bpp { row[i - bpp] } else { 0 };
                let above = previous[i];
                let above_left = if i >= bpp { previous[i - bpp] } else { 0 };
                let subtrahend = match filter {
                    1 => left,
                    2 => above,
                    3 => u8::midpoint(left, above),
                    4 => super::Filter::paeth(left, above, above_left),
                    _ => 0,
                };
                raw.push(row[i].wrapping_sub(subtrahend));
            }
            previous.copy_from_slice(row);
        }

        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        let mut out = Vec::new();
        out.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&image.width().to_be_bytes());
        ihdr.extend_from_slice(&image.height().to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        super::Chunk::write(&mut out, *super::IHDR, &ihdr);
        super::Chunk::write(&mut out, *super::IDAT, &compressed);
        super::Chunk::write(&mut out, *super::IEND, &[]);
        out
    }
}
