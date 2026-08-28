#![cfg_attr(not(test), no_std)]
// This crate reads and writes an 8-bit-per-channel raster format. Narrowing a weighted `f64` back
// to a channel byte is what the arithmetic is *for*, and the value is clamped into range at every
// such site, so the two value-crossing casts are allowed crate-wide rather than at each of them.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

//! `jals-image`: the PNG codec and image comparison a screenshot test needs.
//!
//! One job, in two halves. [`Png`] reads and writes the subset of PNG a screenshot is written in —
//! 8 bits per channel, non-interlaced — and [`Comparison`] scores two [`Image`]s against each other
//! and draws the difference. Nothing here knows what a test is; a consumer hands over two buffers
//! of bytes and gets back a count and a picture.
//!
//! **Why this is a crate and not a module.** It is a portable codec, like `jals-classfile` and the
//! zip reader in `jals-classpath` — `core + alloc`, no host, no filesystem, testable with nothing
//! installed. Its consumer is `jals-build`'s native half, whose *portable* core deliberately
//! carries no compression dependency; putting the decoder there would push `miniz_oxide` into a
//! configuration that has no use for it. The dependencies taken here (`miniz_oxide`, `crc32fast`)
//! are the two `jals-classpath`'s `archive` feature already takes, in the same `no_std` shape.
//!
//! **Why it is written rather than depended on.** Every PNG crate on crates.io that reads a full
//! chunk stream wants `std`, and the comparison this needs is a specific published algorithm
//! rather than a general image-processing surface. The same reasoning `jals-project` records for
//! its Jinja subset applies unchanged: a portable crate writes the subset it needs.
//!
//! ```
//! use jals_image::{Comparison, Image, Png, Rgba};
//!
//! let mut left = Image::filled(2, 1, Rgba::OPAQUE_BLACK);
//! let mut right = left.clone();
//! right.set(1, 0, Rgba::new(255, 255, 255, 255));
//!
//! let outcome = Comparison::default().run(&left, &right).expect("same size");
//! assert_eq!(outcome.differing, 1);
//!
//! // And the codec round-trips what it wrote.
//! let bytes = Png::encode(&right);
//! assert_eq!(Png::decode(&bytes).expect("valid png"), right);
//! ```

extern crate alloc;

mod compare;
mod image;
mod png;

pub use compare::{Comparison, Outcome, SizeMismatch};
pub use image::{Image, Rect, Rgba};
pub use png::{Png, PngError};
