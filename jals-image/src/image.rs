//! The raster an encoder writes, a decoder produces, and a comparison scores.
//!
//! One representation and no conversions: 8-bit RGBA, row-major, no stride padding. A PNG in any
//! other shape is widened to it on the way in ([`Png::decode`](crate::Png::decode)), so nothing
//! downstream branches on colour type or bit depth.

use alloc::vec;
use alloc::vec::Vec;

/// One pixel, non-premultiplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rgba {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
    pub(crate) a: u8,
}

impl Rgba {
    /// Fully opaque black — the ground a difference image is drawn over.
    pub const OPAQUE_BLACK: Self = Self::new(0, 0, 0, 255);

    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// A rectangle of pixels, in image coordinates with the origin at the top left.
///
/// Half-open on both axes — `x` in `left..right`, `y` in `top..bottom` — so two rectangles sharing
/// an edge cover it once. An empty or inverted rectangle covers nothing rather than being an
/// error: a mask that selects no pixel is a mask that changes no verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

impl Rect {
    #[must_use]
    pub const fn new(left: u32, top: u32, right: u32, bottom: u32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Whether `(x, y)` falls inside.
    #[must_use]
    pub(crate) const fn contains(self, x: u32, y: u32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// An 8-bit RGBA raster.
///
/// The pixel buffer is exactly `width * height * 4` bytes; every constructor upholds that, so
/// indexing needs no bounds arithmetic beyond the coordinate check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    width: u32,
    height: u32,
    /// Row-major RGBA, four bytes per pixel.
    pixels: Vec<u8>,
}

impl Image {
    /// Bytes per pixel in [`pixels`](Self::pixels). Named rather than spelled `4` at each of the
    /// half-dozen sites that index the buffer.
    pub(crate) const CHANNELS: usize = 4;

    /// An image of `fill`.
    #[must_use]
    pub fn filled(width: u32, height: u32, fill: Rgba) -> Self {
        let count = Self::buffer_len(width, height);
        let mut pixels = Vec::with_capacity(count);
        for _ in 0..count / Self::CHANNELS {
            pixels.extend_from_slice(&[fill.r, fill.g, fill.b, fill.a]);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// An image of transparent black.
    #[must_use]
    pub(crate) fn transparent(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; Self::buffer_len(width, height)],
        }
    }

    /// Adopt an existing RGBA buffer.
    ///
    /// # Errors
    /// [`BufferLength`] when `pixels` is not exactly `width * height * 4` bytes long.
    pub(crate) fn from_rgba(
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<Self, BufferLength> {
        let expected = Self::buffer_len(width, height);
        if pixels.len() == expected {
            Ok(Self {
                width,
                height,
                pixels,
            })
        } else {
            Err(BufferLength {
                expected,
                found: pixels.len(),
            })
        }
    }

    /// How many bytes back a `width * height` image.
    ///
    /// Saturating rather than wrapping: a dimension pair whose product overflows `usize` cannot be
    /// allocated anyway, and saturating turns it into an allocation failure at a named size rather
    /// than a buffer that silently fits a wrapped length.
    const fn buffer_len(width: u32, height: u32) -> usize {
        (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(Self::CHANNELS)
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// How many pixels the image holds.
    #[must_use]
    pub(crate) const fn pixel_count(&self) -> u32 {
        self.width * self.height
    }

    /// Whether `other` has the same dimensions.
    #[must_use]
    pub(crate) const fn same_size_as(&self, other: &Self) -> bool {
        self.width == other.width && self.height == other.height
    }

    /// The raw row-major RGBA buffer.
    #[must_use]
    pub(crate) fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The byte offset of `(x, y)`, without checking that it is in range.
    const fn offset(&self, x: u32, y: u32) -> usize {
        (y as usize * self.width as usize + x as usize) * Self::CHANNELS
    }

    /// The pixel at `(x, y)`, or `None` when the coordinate is outside the image.
    #[must_use]
    pub(crate) fn get(&self, x: u32, y: u32) -> Option<Rgba> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = self.offset(x, y);
        Some(Rgba::new(
            self.pixels[at],
            self.pixels[at + 1],
            self.pixels[at + 2],
            self.pixels[at + 3],
        ))
    }

    /// Write `pixel` at `(x, y)`. Out-of-range coordinates are ignored.
    pub fn set(&mut self, x: u32, y: u32, pixel: Rgba) {
        if x >= self.width || y >= self.height {
            return;
        }
        let at = self.offset(x, y);
        self.pixels[at] = pixel.r;
        self.pixels[at + 1] = pixel.g;
        self.pixels[at + 2] = pixel.b;
        self.pixels[at + 3] = pixel.a;
    }
}

/// A pixel buffer whose length does not match the dimensions it was offered under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferLength {
    pub expected: usize,
    pub found: usize,
}

impl core::fmt::Display for BufferLength {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self { expected, found } = self;
        write!(f, "pixel buffer is {found} bytes, expected {expected}")
    }
}

impl core::error::Error for BufferLength {}

#[cfg(test)]
mod tests {
    use super::{Image, Rect, Rgba};

    #[test]
    fn a_filled_image_holds_that_pixel_everywhere() {
        let fill = Rgba::new(3, 5, 7, 9);
        let image = Image::filled(4, 3, fill);
        assert_eq!(image.pixel_count(), 12);
        assert_eq!(image.pixels().len(), 48);
        for y in 0..3 {
            for x in 0..4 {
                assert_eq!(image.get(x, y), Some(fill));
            }
        }
    }

    #[test]
    fn coordinates_outside_the_image_read_none_and_write_nothing() {
        let mut image = Image::filled(2, 2, Rgba::OPAQUE_BLACK);
        assert_eq!(image.get(2, 0), None);
        assert_eq!(image.get(0, 2), None);
        image.set(2, 0, Rgba::new(255, 255, 255, 255));
        image.set(0, 2, Rgba::new(255, 255, 255, 255));
        assert_eq!(image.get(0, 0), Some(Rgba::OPAQUE_BLACK));
    }

    #[test]
    fn a_buffer_of_the_wrong_length_is_refused() {
        let error = Image::from_rgba(2, 2, alloc::vec![0; 15]).expect_err("15 is not 16");
        assert_eq!(error.expected, 16);
        assert_eq!(error.found, 15);
    }

    #[test]
    fn a_rect_is_half_open_on_both_axes() {
        let rect = Rect::new(1, 1, 3, 3);
        assert!(rect.contains(1, 1));
        assert!(rect.contains(2, 2));
        assert!(!rect.contains(3, 2));
        assert!(!rect.contains(2, 3));
        assert!(!rect.contains(0, 1));
    }

    #[test]
    fn an_inverted_rect_covers_nothing() {
        let rect = Rect::new(3, 3, 1, 1);
        for y in 0..5 {
            for x in 0..5 {
                assert!(!rect.contains(x, y));
            }
        }
    }
}
