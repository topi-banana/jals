//! Scoring two images against each other, and drawing where they differ.
//!
//! The algorithm is **pixelmatch**'s: a perceptual colour distance in YIQ rather than a channel
//! distance in RGB, plus a neighbourhood test that recognises an anti-aliased edge and declines to
//! count it. Both halves earn their place on the subject. A screenshot comparison that used a raw
//! RGB distance would rank a barely-visible shift in a dark region the same as a visible one in a
//! bright region; and text is the thing most likely to land a pixel differently between two runs,
//! which is exactly what the anti-aliasing test exists to absorb.
//!
//! A comparison answers three questions and keeps them apart: how many pixels **differ**, how many
//! were **anti-aliased** (seen, judged, not counted), and how many were **masked** (never looked
//! at). Collapsing those into one number would make a mask indistinguishable from a match.

use alloc::vec::Vec;

use crate::image::{Image, Rect, Rgba};

/// The largest possible squared YIQ distance between two 8-bit colours, from pixelmatch. The
/// configured threshold is a fraction of it, which is what makes the threshold resolution- and
/// palette-independent.
const MAX_YIQ_DELTA: f64 = 35215.0;

/// Colour a differing pixel is drawn in. Magenta rather than pixelmatch's red because the subject
/// is Minecraft: its palette is dense in greens, browns and greys and almost empty of magenta, so
/// a difference cannot be mistaken for content.
const DIFFERING: Rgba = Rgba::new(255, 0, 255, 255);

/// Colour an anti-aliased pixel is drawn in — seen and dismissed, so it is marked but distinct.
const ANTIALIASED: Rgba = Rgba::new(255, 224, 0, 255);

/// Colour a masked pixel is drawn in: never compared, and visibly so.
const MASKED: Rgba = Rgba::new(0, 128, 255, 255);

/// How two images are compared.
#[derive(Debug, Clone)]
pub struct Comparison {
    /// Matching sensitivity, as a fraction of the maximum YIQ distance. Smaller is stricter;
    /// `0.0` means any difference at all counts.
    ///
    /// **The default is `0.0`, not pixelmatch's `0.1`, and the difference is measured rather than
    /// preferred.** pixelmatch's default is calibrated for browser screenshots, where the same
    /// page renders with genuinely different subpixel anti-aliasing between runs. A pinned
    /// software renderer does not do that. Rendering one scene twice under Mesa's llvmpipe gives a
    /// byte-identical framebuffer — across thread counts as well as runs — so there is no
    /// run-to-run noise for a threshold to absorb. What a loose threshold absorbs instead is real
    /// failure: rendering the same scene under a *different* rasterizer (softpipe) changes 11.9%
    /// of pixels, and this is how much of that each threshold still sees:
    ///
    /// | `threshold` | differing pixels seen |
    /// | --- | --- |
    /// | `0.0` | 11.89% |
    /// | `0.001` | 10.74% |
    /// | `0.005` | 0.14% |
    /// | `0.01` | 1 pixel |
    /// | `0.05` and above | none at all |
    ///
    /// At pixelmatch's default a whole rasterizer swap reports a clean pass. So the default here
    /// is exactness, and loosening it is a decision a suite makes deliberately, against a number
    /// it has measured on its own renderer.
    pub threshold: f64,
    /// Count anti-aliased pixels as differences instead of recognising them.
    ///
    /// Off by default. Turn it on for a comparison where an edge moving by one pixel is the very
    /// thing under test.
    pub include_antialiasing: bool,
    /// How much of the unchanged image shows through the difference picture, `0.0` (white) to
    /// `1.0` (the original's own brightness).
    pub background_alpha: f64,
    /// Regions excluded from the comparison entirely.
    pub masks: Vec<Rect>,
    /// The most differing pixels that still passes.
    ///
    /// With this and [`max_diff_ratio`](Self::max_diff_ratio) both unset, the budget is **zero** —
    /// see [`Outcome::within_budget`].
    pub max_diff_pixels: Option<u32>,
    /// The largest differing fraction of the compared pixels that still passes.
    pub max_diff_ratio: Option<f64>,
}

impl Default for Comparison {
    fn default() -> Self {
        Self {
            threshold: 0.0,
            include_antialiasing: false,
            background_alpha: 0.1,
            masks: Vec::new(),
            max_diff_pixels: None,
            max_diff_ratio: None,
        }
    }
}

impl Comparison {
    /// Compare `expected` against `actual`.
    ///
    /// # Errors
    /// [`SizeMismatch`] when the two images are not the same size. Resizing one to fit is
    /// deliberately not offered: a screenshot that came out a different size is a failure of the
    /// run that produced it, and scaling it would turn that into a diffuse pixel difference
    /// reported against the wrong cause.
    pub fn run(&self, expected: &Image, actual: &Image) -> Result<Outcome, SizeMismatch> {
        if !expected.same_size_as(actual) {
            return Err(SizeMismatch {
                expected: (expected.width(), expected.height()),
                actual: (actual.width(), actual.height()),
            });
        }
        let cutoff = MAX_YIQ_DELTA * self.threshold * self.threshold;
        let mut diff = Image::transparent(expected.width(), expected.height());
        let mut differing = 0u32;
        let mut antialiased = 0u32;
        let mut masked = 0u32;

        for y in 0..expected.height() {
            for x in 0..expected.width() {
                if self.masks.iter().any(|mask| mask.contains(x, y)) {
                    masked += 1;
                    diff.set(x, y, MASKED);
                    continue;
                }
                let left = expected.get(x, y).expect("in range");
                let right = actual.get(x, y).expect("in range");
                let delta = Yiq::delta(left, right);
                if delta.abs() <= cutoff {
                    diff.set(x, y, Self::faded(left, self.background_alpha));
                    continue;
                }
                if !self.include_antialiasing
                    && (Self::antialiased(expected, actual, x, y)
                        || Self::antialiased(actual, expected, x, y))
                {
                    antialiased += 1;
                    diff.set(x, y, ANTIALIASED);
                } else {
                    differing += 1;
                    diff.set(x, y, DIFFERING);
                }
            }
        }

        let compared = expected.pixel_count() - masked;
        let ratio = if compared == 0 {
            0.0
        } else {
            f64::from(differing) / f64::from(compared)
        };
        // With no budget configured at all, the budget is **zero**. The other reading — that an
        // unset maximum means an unlimited one — makes a comparison that can never fail, which is
        // the same trap a loose `threshold` sets from the other side. A suite that wants to
        // tolerate drift says how much; silence is not a tolerance.
        let within_budget = match (self.max_diff_pixels, self.max_diff_ratio) {
            (None, None) => differing == 0,
            (pixels, allowed) => {
                pixels.is_none_or(|max| differing <= max) && allowed.is_none_or(|max| ratio <= max)
            }
        };
        Ok(Outcome {
            differing,
            antialiased,
            compared,
            ratio,
            within_budget,
            diff,
        })
    }

    /// The unchanged ground of a difference image: `pixel`'s own brightness, pulled toward white
    /// so anything drawn on top reads immediately.
    fn faded(pixel: Rgba, alpha: f64) -> Rgba {
        let luma = Yiq::of(pixel).y;
        // Alpha is folded in here rather than left to the viewer, because the difference image is
        // written as an opaque PNG: an image with a real alpha channel would composite against
        // whatever happened to be behind it.
        let value =
            (255.0 + (luma - 255.0) * alpha * f64::from(pixel.a) / 255.0).clamp(0.0, 255.0) as u8;
        Rgba::new(value, value, value, 255)
    }

    /// Whether `(x, y)` sits on an anti-aliased edge in `image`.
    ///
    /// pixelmatch's test, unchanged: a pixel is anti-aliased when it is the local brightness
    /// extreme of its 3×3 neighbourhood *and* the neighbour it is extreme against has many
    /// identical siblings in **both** images. The second half is what keeps a genuine one-pixel
    /// change from being dismissed — a real difference does not sit against flat surroundings in
    /// the other image too.
    fn antialiased(image: &Image, other: &Image, x: u32, y: u32) -> bool {
        let centre = image.get(x, y).expect("in range");
        let (x0, y0, x2, y2) = Self::neighbourhood(image, x, y);
        // A pixel on the image's border has fewer than eight neighbours; pixelmatch counts the
        // missing side as one identical sibling so an edge pixel is not judged more strictly than
        // an interior one.
        let mut zeroes = u32::from(x == x0 || x == x2 || y == y0 || y == y2);
        let mut min = 0.0f64;
        let mut max = 0.0f64;
        let mut min_at = (0u32, 0u32);
        let mut max_at = (0u32, 0u32);

        for ny in y0..=y2 {
            for nx in x0..=x2 {
                if nx == x && ny == y {
                    continue;
                }
                let delta = Yiq::brightness_delta(centre, image.get(nx, ny).expect("in range"));
                if delta == 0.0 {
                    zeroes += 1;
                    if zeroes > 2 {
                        return false;
                    }
                } else if delta < min {
                    min = delta;
                    min_at = (nx, ny);
                } else if delta > max {
                    max = delta;
                    max_at = (nx, ny);
                }
            }
        }

        if min == 0.0 || max == 0.0 {
            return false;
        }
        (Self::has_many_siblings(image, min_at.0, min_at.1)
            && Self::has_many_siblings(other, min_at.0, min_at.1))
            || (Self::has_many_siblings(image, max_at.0, max_at.1)
                && Self::has_many_siblings(other, max_at.0, max_at.1))
    }

    /// Whether more than two of `(x, y)`'s neighbours are identical to it — the flat surroundings
    /// an anti-aliased edge pixel sits against.
    fn has_many_siblings(image: &Image, x: u32, y: u32) -> bool {
        let centre = image.get(x, y).expect("in range");
        let (x0, y0, x2, y2) = Self::neighbourhood(image, x, y);
        let mut zeroes = u32::from(x == x0 || x == x2 || y == y0 || y == y2);
        for ny in y0..=y2 {
            for nx in x0..=x2 {
                if nx == x && ny == y {
                    continue;
                }
                if image.get(nx, ny) == Some(centre) {
                    zeroes += 1;
                    if zeroes > 2 {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// The 3×3 neighbourhood of `(x, y)`, clamped to the image.
    fn neighbourhood(image: &Image, x: u32, y: u32) -> (u32, u32, u32, u32) {
        (
            x.saturating_sub(1),
            y.saturating_sub(1),
            (x + 1).min(image.width() - 1),
            (y + 1).min(image.height() - 1),
        )
    }
}

/// One colour in the YIQ space the comparison measures in.
#[derive(Debug, Clone, Copy)]
struct Yiq {
    y: f64,
    i: f64,
    q: f64,
}

/// Every weighted sum here multiplies into a local and adds afterwards, rather than writing
/// `a * b + c` for clippy to fold into `mul_add`. That is deliberate and it is not style: a fused
/// multiply-add rounds **once** where two operations round **twice**, so the fused form computes
/// different numbers than the algorithm this ports — and a threshold carried over from pixelmatch
/// would then mean something subtly different here. Writing the products out states the rounding
/// the algorithm is defined in terms of.
impl Yiq {
    /// Convert, compositing any transparency onto white first so two colours that differ only
    /// under a background nobody will see do not register.
    fn of(pixel: Rgba) -> Self {
        let alpha = f64::from(pixel.a) / 255.0;
        let blend = |channel: u8| {
            let toward_white = (f64::from(channel) - 255.0) * alpha;
            255.0 + toward_white
        };
        let (r, g, b) = (blend(pixel.r), blend(pixel.g), blend(pixel.b));
        let weigh = |wr: f64, wg: f64, wb: f64| {
            let (r, g, b) = (r * wr, g * wg, b * wb);
            r + g + b
        };
        Self {
            y: weigh(0.298_895_31, 0.586_622_47, 0.114_482_23),
            i: weigh(0.595_977_99, -0.274_176_10, -0.321_801_89),
            q: weigh(0.211_470_17, -0.522_617_11, 0.311_146_94),
        }
    }

    /// The signed, weighted squared distance between two colours.
    ///
    /// Signed by which is brighter, because [`Comparison::antialiased`] needs to know whether a
    /// neighbour is lighter or darker, not merely how far away it is.
    fn delta(left: Rgba, right: Rgba) -> f64 {
        if left == right {
            return 0.0;
        }
        let (a, b) = (Self::of(left), Self::of(right));
        let (dy, di, dq) = (a.y - b.y, a.i - b.i, a.q - b.q);
        let (wy, wi, wq) = (0.5053 * dy * dy, 0.299 * di * di, 0.1957 * dq * dq);
        let delta = wy + wi + wq;
        if a.y > b.y { -delta } else { delta }
    }

    /// The brightness difference alone, which is what the neighbourhood test ranks by.
    fn brightness_delta(left: Rgba, right: Rgba) -> f64 {
        if left == right {
            return 0.0;
        }
        Self::of(left).y - Self::of(right).y
    }
}

/// What a comparison found.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Pixels that differ beyond the threshold and are not anti-aliasing.
    pub differing: u32,
    /// Pixels that differ but sit on an anti-aliased edge. Reported, not counted against the run.
    pub antialiased: u32,
    /// Pixels actually compared — every pixel less the masked ones. The denominator of
    /// [`ratio`](Self::ratio), so a mask tightens the ratio rather than diluting it.
    pub compared: u32,
    /// [`differing`](Self::differing) over [`compared`](Self::compared).
    pub ratio: f64,
    /// Whether the result is inside the configured budgets.
    ///
    /// With neither [`Comparison::max_diff_pixels`] nor [`Comparison::max_diff_ratio`] set, this is
    /// `differing == 0`: an unstated tolerance is no tolerance, not an infinite one.
    pub within_budget: bool,
    /// The difference picture: the expected image faded to grey, with differing pixels in magenta,
    /// anti-aliased ones in yellow and masked ones in blue.
    pub diff: Image,
}

/// Two images that cannot be compared because they are different sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeMismatch {
    pub expected: (u32, u32),
    pub actual: (u32, u32),
}

impl core::fmt::Display for SizeMismatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self {
            expected: (ew, eh),
            actual: (aw, ah),
        } = self;
        write!(f, "expected a {ew}x{eh} image, but it is {aw}x{ah}")
    }
}

impl core::error::Error for SizeMismatch {}

#[cfg(test)]
mod tests {
    use super::{ANTIALIASED, Comparison, DIFFERING, MASKED};
    use crate::image::{Image, Rect, Rgba};

    const WHITE: Rgba = Rgba::new(255, 255, 255, 255);
    const BLACK: Rgba = Rgba::OPAQUE_BLACK;

    #[test]
    fn an_image_never_differs_from_itself() {
        let mut image = Image::filled(9, 9, WHITE);
        for i in 0..9 {
            image.set(i, i, BLACK);
        }
        let outcome = Comparison::default()
            .run(&image, &image)
            .expect("same size");
        assert_eq!(outcome.differing, 0);
        assert_eq!(outcome.antialiased, 0);
        assert!(outcome.ratio.abs() < f64::EPSILON);
        assert!(outcome.within_budget);
    }

    #[test]
    fn a_single_changed_pixel_is_found_and_drawn() {
        let expected = Image::filled(5, 5, WHITE);
        let mut actual = expected.clone();
        actual.set(2, 2, BLACK);
        let outcome = Comparison::default()
            .run(&expected, &actual)
            .expect("same size");
        assert_eq!(outcome.differing, 1);
        assert_eq!(outcome.compared, 25);
        assert_eq!(outcome.diff.get(2, 2), Some(DIFFERING));
        assert_ne!(outcome.diff.get(0, 0), Some(DIFFERING));
    }

    #[test]
    fn different_sizes_are_refused_rather_than_scaled() {
        let error = Comparison::default()
            .run(&Image::filled(4, 4, WHITE), &Image::filled(4, 5, WHITE))
            .expect_err("4x4 is not 4x5");
        assert_eq!(error.expected, (4, 4));
        assert_eq!(error.actual, (4, 5));
    }

    #[test]
    fn a_mask_excludes_its_region_from_both_the_count_and_the_denominator() {
        let expected = Image::filled(4, 4, WHITE);
        let mut actual = expected.clone();
        for y in 0..2 {
            for x in 0..2 {
                actual.set(x, y, BLACK);
            }
        }
        let masked = Comparison {
            masks: alloc::vec![Rect::new(0, 0, 2, 2)],
            ..Comparison::default()
        };
        let outcome = masked.run(&expected, &actual).expect("same size");
        assert_eq!(outcome.differing, 0);
        // 12 rather than 16: `compared` is the mask's effect stated where a consumer reads it, so
        // there is no second field carrying the count that was taken out.
        assert_eq!(outcome.compared, 12);
        assert_eq!(outcome.diff.get(0, 0), Some(MASKED));
    }

    #[test]
    fn the_threshold_decides_whether_a_faint_change_counts() {
        let expected = Image::filled(3, 3, Rgba::new(128, 128, 128, 255));
        let mut actual = expected.clone();
        actual.set(1, 1, Rgba::new(132, 128, 128, 255));

        let strict = Comparison {
            threshold: 0.0,
            ..Comparison::default()
        };
        assert_eq!(
            strict.run(&expected, &actual).expect("same size").differing,
            1
        );

        let loose = Comparison {
            threshold: 0.5,
            ..Comparison::default()
        };
        assert_eq!(
            loose.run(&expected, &actual).expect("same size").differing,
            0
        );
    }

    #[test]
    fn an_unstated_budget_is_zero_and_a_stated_one_is_what_it_says() {
        let expected = Image::filled(10, 10, WHITE);
        let mut actual = expected.clone();
        for x in 0..5 {
            actual.set(x, 0, BLACK);
        }

        let unstated = Comparison::default()
            .run(&expected, &actual)
            .expect("same size");
        assert_eq!(unstated.differing, 5);
        assert!(
            !unstated.within_budget,
            "an unstated tolerance is zero, not infinity"
        );

        let allowed = Comparison {
            max_diff_pixels: Some(5),
            ..Comparison::default()
        };
        assert!(
            allowed
                .run(&expected, &actual)
                .expect("same size")
                .within_budget
        );

        let by_count = Comparison {
            max_diff_pixels: Some(4),
            ..Comparison::default()
        };
        assert!(
            !by_count
                .run(&expected, &actual)
                .expect("same size")
                .within_budget
        );

        let by_ratio = Comparison {
            max_diff_ratio: Some(0.04),
            ..Comparison::default()
        };
        let outcome = by_ratio.run(&expected, &actual).expect("same size");
        assert!((outcome.ratio - 0.05).abs() < 1e-9);
        assert!(!outcome.within_budget);
    }

    #[test]
    fn an_antialiased_edge_is_recognised_rather_than_counted() {
        // A hard black/white boundary down the middle in `expected`; in `actual` the boundary
        // column carries one intermediate value, which is what an edge landing differently
        // between two renders looks like.
        let mut expected = Image::filled(9, 9, WHITE);
        let mut actual = Image::filled(9, 9, WHITE);
        for y in 0..9 {
            for x in 0..4 {
                expected.set(x, y, BLACK);
                actual.set(x, y, BLACK);
            }
            actual.set(4, y, Rgba::new(128, 128, 128, 255));
        }

        let recognised = Comparison::default()
            .run(&expected, &actual)
            .expect("same size");
        assert_eq!(
            recognised.differing, 0,
            "an antialiased edge is not a difference"
        );
        assert!(recognised.antialiased > 0);
        assert_eq!(recognised.diff.get(4, 4), Some(ANTIALIASED));

        // And the escape hatch: a suite testing the edge itself counts them.
        let counted = Comparison {
            include_antialiasing: true,
            ..Comparison::default()
        };
        let outcome = counted.run(&expected, &actual).expect("same size");
        assert!(outcome.differing > 0);
        assert_eq!(outcome.antialiased, 0);
    }

    #[test]
    fn transparency_is_composited_before_it_is_judged() {
        // Two fully transparent pixels whose colour channels disagree are the same picture.
        let mut expected = Image::filled(3, 3, WHITE);
        let mut actual = expected.clone();
        expected.set(1, 1, Rgba::new(255, 0, 0, 0));
        actual.set(1, 1, Rgba::new(0, 0, 255, 0));
        let outcome = Comparison {
            threshold: 0.0,
            ..Comparison::default()
        }
        .run(&expected, &actual)
        .expect("same size");
        assert_eq!(outcome.differing, 0);
    }

    #[test]
    fn a_fully_masked_comparison_reports_no_ratio_rather_than_dividing_by_zero() {
        let image = Image::filled(2, 2, WHITE);
        let outcome = Comparison {
            masks: alloc::vec![Rect::new(0, 0, 2, 2)],
            ..Comparison::default()
        }
        .run(&image, &image)
        .expect("same size");
        assert_eq!(outcome.compared, 0);
        assert!(outcome.ratio.abs() < f64::EPSILON);
        assert!(outcome.within_budget);
    }
}
