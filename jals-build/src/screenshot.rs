//! Judging the screenshots a test target produced against its reference images.
//!
//! The split this module exists to hold: **the program takes the picture, jals decides whether it
//! is right.** A target's report only says "test `X` produced a shot named `title_screen` at this
//! path" ([`Shot`]); which bytes that should have been is a question about the golden set, which
//! the program has never seen and does not need to.
//!
//! Three consequences follow, and each is a case below rather than a failure:
//!
//! - **A shot with no reference image is not an error.** It is what the first run of a new test
//!   looks like, and what every run looks like before a golden archive has been published. It is
//!   reported as [`ShotOutcome::NoReference`] so `--update-golden` has something to collect.
//! - **A reference image with no shot is.** The golden set says a picture was expected; the run did
//!   not take it. That is the target failing to do what it said it does.
//! - **A shot that cannot be decoded is its own case**, never a difference: a truncated PNG is a
//!   broken run, not a changed picture, and reporting "409920 pixels differ" for one would name the
//!   wrong cause.

use std::path::{Path, PathBuf};

use jals_config::testing::Screenshots;
use jals_exec::tokio_rt::on_blocking_pool;
use jals_image::{Comparison, Image, Png, Rect};
use jals_storage::RelativePath;

use crate::test_report::Shot;

/// The file extension a reference image and a difference image are written with.
const PNG_EXTENSION: &str = "png";

/// What comparing one screenshot produced.
#[derive(Debug, Clone, PartialEq)]
pub enum ShotOutcome {
    /// The shot matched its reference image within the configured budgets.
    Matched {
        name: String,
        /// Anti-aliased pixels the comparison recognised and did not count. Carried even on a
        /// match, because a number that climbs run over run is the early warning that a renderer
        /// is drifting.
        antialiased: u32,
    },
    /// The shot and its reference image differ beyond the budgets.
    Differed(Box<ScreenshotDiff>),
    /// The golden set has no image under this name.
    NoReference { name: String, actual: PathBuf },
    /// The report named a shot the run did not write.
    Missing { name: String, actual: PathBuf },
    /// A PNG on either side could not be read.
    Unreadable {
        name: String,
        path: PathBuf,
        reason: String,
    },
    /// The report named a file the target does not claim to write screenshots into.
    ///
    /// The report is written by the program under test, so its paths are a claim and not a fact: a
    /// `..` in one would have jals read — and `--update-golden` package and publish — a file the
    /// run never produced. `[test-target.screenshots] dir` is the declaration this is checked
    /// against, which is why a target that photographs anything must make it.
    Misplaced {
        name: String,
        /// What the report said, verbatim.
        claimed: String,
        /// The declared directory it had to be under.
        expected: String,
    },
}

impl ShotOutcome {
    /// Whether this outcome fails the test that produced the shot.
    ///
    /// [`NoReference`](Self::NoReference) does **not**: a run with nothing to compare against has
    /// not disagreed with anything. Whether a suite tolerates that is the host's policy — CI wants
    /// it to be an error, a developer bringing a new test up wants it to be a prompt to bless — and
    /// this type does not decide it.
    #[must_use]
    pub const fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::Differed(_)
                | Self::Missing { .. }
                | Self::Unreadable { .. }
                | Self::Misplaced { .. }
        )
    }
}

/// How far apart one shot and its reference image are, and where a reader can look.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenshotDiff {
    pub name: String,
    /// Pixels that differ beyond the threshold and are not anti-aliasing.
    pub differing: u32,
    /// Pixels that differ but sit on an anti-aliased edge — seen, judged, not counted.
    antialiased: u32,
    /// Pixels actually compared: every pixel less those a mask excluded.
    pub compared: u32,
    /// [`differing`](Self::differing) over [`compared`](Self::compared).
    pub ratio: f64,
    pub reference: PathBuf,
    pub actual: PathBuf,
    /// The difference picture, absent only when it could not be written.
    pub diff: Option<PathBuf>,
    /// Set when the two images are not even the same size, which is a different failure from a
    /// changed picture and reads as one.
    pub size_mismatch: Option<(u32, u32, u32, u32)>,
}

/// Compares a run's screenshots against a golden set.
#[derive(Debug, Clone)]
pub struct ScreenshotVerifier {
    comparison: Comparison,
    /// The materialized golden set. `None` when the selection activates no golden archive — the
    /// state a project is in before its first `--update-golden`.
    reference_dir: Option<PathBuf>,
    /// Where difference pictures are written.
    diff_dir: PathBuf,
    /// The directory below the run directory the target says it writes screenshots into.
    ///
    /// The containment check every reported path is held to. `None` only for a target that takes
    /// no screenshots, which never reaches a verifier.
    screenshot_dir: Option<RelativePath>,
}

impl ScreenshotVerifier {
    /// Build a verifier from a target's `[test-target.screenshots]` section.
    #[must_use]
    pub fn new(config: &Screenshots, reference_dir: Option<PathBuf>, diff_dir: PathBuf) -> Self {
        Self {
            screenshot_dir: RelativePath::parse(&config.dir).ok(),
            comparison: Comparison {
                threshold: config.threshold,
                max_diff_pixels: config.max_diff_pixels,
                max_diff_ratio: config.max_diff_ratio,
                masks: config
                    .masks
                    .iter()
                    .map(|mask| Rect::new(mask.left, mask.top, mask.right, mask.bottom))
                    .collect(),
                ..Comparison::default()
            },
            reference_dir,
            diff_dir,
        }
    }

    /// The path a reported shot may be read from, or `None` when the report overreached.
    ///
    /// Two conditions, and neither is a formality. `RelativePath::parse` rejects an absolute path,
    /// a drive letter, a UNC prefix and every `.`/`..` segment, so what comes back cannot leave the
    /// run directory by construction. The prefix test then holds it to the directory the target
    /// declared — because a run that writes its logs where its screenshots go is a target that has
    /// stopped meaning what its manifest says, and `--update-golden` would publish the difference.
    fn admit(&self, claimed: &str) -> Option<RelativePath> {
        let path = RelativePath::parse(claimed).ok()?;
        let dir = self.screenshot_dir.as_ref()?;
        path.starts_with(dir).then_some(path)
    }

    /// Compare one shot, reading both images and writing a difference picture if they disagree.
    ///
    /// `run_dir` is the target's working directory, which the report's paths are relative to.
    pub(crate) async fn verify(&self, shot: &Shot, run_dir: &Path) -> ShotOutcome {
        let Some(relative) = self.admit(&shot.path) else {
            return ShotOutcome::Misplaced {
                name: shot.name.clone(),
                claimed: shot.path.clone(),
                expected: self
                    .screenshot_dir
                    .as_ref()
                    .map(RelativePath::to_string)
                    .unwrap_or_default(),
            };
        };
        let actual_path = run_dir.join(relative.to_string());
        let reference_path = self
            .reference_dir
            .as_ref()
            .map(|dir| dir.join(&shot.name).with_extension(PNG_EXTENSION));

        let actual = match Self::read(&actual_path).await {
            Ok(Some(image)) => image,
            // The report named it and the run did not write it: the target did not do what it said.
            Ok(None) => {
                return ShotOutcome::Missing {
                    name: shot.name.clone(),
                    actual: actual_path,
                };
            }
            Err(reason) => {
                return ShotOutcome::Unreadable {
                    name: shot.name.clone(),
                    path: actual_path,
                    reason,
                };
            }
        };

        let Some(reference_path) = reference_path else {
            return ShotOutcome::NoReference {
                name: shot.name.clone(),
                actual: actual_path,
            };
        };
        let reference = match Self::read(&reference_path).await {
            Ok(Some(image)) => image,
            Ok(None) => {
                return ShotOutcome::NoReference {
                    name: shot.name.clone(),
                    actual: actual_path,
                };
            }
            Err(reason) => {
                return ShotOutcome::Unreadable {
                    name: shot.name.clone(),
                    path: reference_path,
                    reason,
                };
            }
        };

        match self.comparison.run(&reference, &actual) {
            Ok(outcome) if outcome.within_budget => ShotOutcome::Matched {
                name: shot.name.clone(),
                antialiased: outcome.antialiased,
            },
            Ok(outcome) => {
                let diff = self.write_diff(&shot.name, &outcome.diff).await;
                ShotOutcome::Differed(Box::new(ScreenshotDiff {
                    name: shot.name.clone(),
                    differing: outcome.differing,
                    antialiased: outcome.antialiased,
                    compared: outcome.compared,
                    ratio: outcome.ratio,
                    reference: reference_path,
                    actual: actual_path,
                    diff,
                    size_mismatch: None,
                }))
            }
            // Different sizes cannot produce a difference picture, so the numbers are reported as
            // zero and the dimensions carry the whole story.
            Err(mismatch) => ShotOutcome::Differed(Box::new(ScreenshotDiff {
                name: shot.name.clone(),
                differing: 0,
                antialiased: 0,
                compared: 0,
                ratio: 0.0,
                reference: reference_path,
                actual: actual_path,
                diff: None,
                size_mismatch: Some((
                    mismatch.expected.0,
                    mismatch.expected.1,
                    mismatch.actual.0,
                    mismatch.actual.1,
                )),
            })),
        }
    }

    /// Names the golden set holds that `taken` did not produce.
    ///
    /// The mirror of [`ShotOutcome::NoReference`], and the reason it exists: a target that stops
    /// taking a screenshot would otherwise simply report one fewer test and pass.
    ///
    /// # Errors
    /// The directory listing's failure, when a reference directory is set but cannot be read.
    pub(crate) async fn unmatched_references(
        &self,
        taken: &[String],
    ) -> std::io::Result<Vec<String>> {
        let Some(dir) = self.reference_dir.clone() else {
            return Ok(Vec::new());
        };
        let taken: Vec<String> = taken.to_vec();
        on_blocking_pool(move || {
            let mut missing = Vec::new();
            for entry in std::fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some(PNG_EXTENSION) {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                if !taken.iter().any(|shot| shot == name) {
                    missing.push(name.to_owned());
                }
            }
            // Sorted, because a directory listing's order is the filesystem's and a report's is a
            // promise.
            missing.sort();
            Ok(missing)
        })
        .await
    }

    /// Read a PNG. `Ok(None)` means the file is not there, which several callers treat as a state
    /// rather than a failure; `Err` carries a rendered reason for the ones that do not.
    async fn read(path: &Path) -> Result<Option<Image>, String> {
        let path = path.to_path_buf();
        let bytes = on_blocking_pool(move || match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.to_string()),
        })
        .await?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        Png::decode(&bytes)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    /// Write a difference picture, returning where it landed.
    ///
    /// A failure here is deliberately not a failure of the comparison: the difference has already
    /// been established, and losing the picture makes the report less useful without making it
    /// wrong.
    async fn write_diff(&self, name: &str, diff: &Image) -> Option<PathBuf> {
        let path = self.diff_dir.join(name).with_extension(PNG_EXTENSION);
        let bytes = Png::encode(diff);
        let target = path.clone();
        on_blocking_pool(move || {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, &bytes)
        })
        .await
        .ok()
        .map(|()| path)
    }
}

#[cfg(test)]
mod tests {
    use super::{ScreenshotVerifier, ShotOutcome};
    use crate::test_report::Shot;
    use jals_config::testing::{Mask, Screenshots};
    use jals_image::{Image, Png, Rgba};
    use std::path::Path;

    const WHITE: Rgba = Rgba::new(255, 255, 255, 255);

    fn write(path: &Path, image: &Image) {
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
        std::fs::write(path, Png::encode(image)).expect("write");
    }

    fn shot(name: &str) -> Shot {
        Shot {
            name: name.to_owned(),
            path: format!("screenshots/{name}.png"),
        }
    }

    /// The section a target that photographs anything declares, which is what the containment
    /// check is built from — `Screenshots::default()` alone declares no directory, and a verifier
    /// is never built for a target in that state.
    fn declared() -> Screenshots {
        Screenshots {
            dir: "screenshots".to_owned(),
            ..Screenshots::default()
        }
    }

    /// A verifier over a fresh scratch tree, plus the run directory its shots live under.
    fn fixture(config: &Screenshots) -> (tempfile::TempDir, ScreenshotVerifier) {
        let dir = tempfile::tempdir().expect("scratch");
        let verifier = ScreenshotVerifier::new(
            config,
            Some(dir.path().join("golden")),
            dir.path().join("diff"),
        );
        (dir, verifier)
    }

    #[test]
    fn an_identical_shot_matches() {
        let (dir, verifier) = fixture(&declared());
        let image = Image::filled(8, 8, WHITE);
        write(&dir.path().join("golden/title.png"), &image);
        write(&dir.path().join("run/screenshots/title.png"), &image);

        let outcome =
            jals_exec::block_on_inline(verifier.verify(&shot("title"), &dir.path().join("run")));
        assert!(
            matches!(outcome, ShotOutcome::Matched { .. }),
            "{outcome:?}"
        );
        assert!(!outcome.is_failure());
    }

    #[test]
    fn a_changed_shot_differs_and_leaves_a_picture_to_look_at() {
        let (dir, verifier) = fixture(&declared());
        let reference = Image::filled(8, 8, WHITE);
        let mut actual = reference.clone();
        actual.set(3, 3, Rgba::OPAQUE_BLACK);
        write(&dir.path().join("golden/title.png"), &reference);
        write(&dir.path().join("run/screenshots/title.png"), &actual);

        let outcome =
            jals_exec::block_on_inline(verifier.verify(&shot("title"), &dir.path().join("run")));
        let ShotOutcome::Differed(diff) = outcome else {
            panic!("expected a difference, got {outcome:?}");
        };
        assert_eq!(diff.differing, 1);
        assert_eq!(diff.compared, 64);
        let written = diff.diff.expect("a difference picture is written");
        assert!(written.exists());
        // And it is a PNG this crate can read back, not merely bytes on disk — at the size of the
        // pictures it is the difference between, which is the half a width alone would not catch.
        let bytes = std::fs::read(&written).expect("read");
        let picture = Png::decode(&bytes).expect("valid");
        assert_eq!((picture.width(), picture.height()), (8, 8));
    }

    #[test]
    fn a_mask_can_forgive_the_region_it_covers() {
        let config = Screenshots {
            masks: vec![Mask {
                left: 0,
                top: 0,
                right: 8,
                bottom: 2,
            }],
            ..declared()
        };
        let (dir, verifier) = fixture(&config);
        let reference = Image::filled(8, 8, WHITE);
        let mut actual = reference.clone();
        for x in 0..8 {
            actual.set(x, 0, Rgba::OPAQUE_BLACK);
        }
        write(&dir.path().join("golden/title.png"), &reference);
        write(&dir.path().join("run/screenshots/title.png"), &actual);

        let outcome =
            jals_exec::block_on_inline(verifier.verify(&shot("title"), &dir.path().join("run")));
        assert!(
            matches!(outcome, ShotOutcome::Matched { .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_shot_with_no_reference_is_a_state_and_not_a_failure() {
        let (dir, verifier) = fixture(&declared());
        write(
            &dir.path().join("run/screenshots/title.png"),
            &Image::filled(4, 4, WHITE),
        );
        let outcome =
            jals_exec::block_on_inline(verifier.verify(&shot("title"), &dir.path().join("run")));
        assert!(
            matches!(outcome, ShotOutcome::NoReference { .. }),
            "{outcome:?}"
        );
        assert!(!outcome.is_failure());
    }

    #[test]
    fn a_shot_the_run_never_wrote_is_a_failure() {
        let (dir, verifier) = fixture(&declared());
        write(
            &dir.path().join("golden/title.png"),
            &Image::filled(4, 4, WHITE),
        );
        let outcome =
            jals_exec::block_on_inline(verifier.verify(&shot("title"), &dir.path().join("run")));
        assert!(
            matches!(outcome, ShotOutcome::Missing { .. }),
            "{outcome:?}"
        );
        assert!(outcome.is_failure());
    }

    #[test]
    fn a_truncated_png_is_unreadable_rather_than_different() {
        let (dir, verifier) = fixture(&declared());
        write(
            &dir.path().join("golden/title.png"),
            &Image::filled(4, 4, WHITE),
        );
        let broken = dir.path().join("run/screenshots/title.png");
        std::fs::create_dir_all(broken.parent().expect("parent")).expect("mkdir");
        std::fs::write(&broken, b"not a png").expect("write");

        let outcome =
            jals_exec::block_on_inline(verifier.verify(&shot("title"), &dir.path().join("run")));
        assert!(
            matches!(outcome, ShotOutcome::Unreadable { .. }),
            "{outcome:?}"
        );
        assert!(outcome.is_failure());
    }

    #[test]
    fn differently_sized_images_report_the_dimensions_rather_than_a_pixel_count() {
        let (dir, verifier) = fixture(&declared());
        write(
            &dir.path().join("golden/title.png"),
            &Image::filled(8, 8, WHITE),
        );
        write(
            &dir.path().join("run/screenshots/title.png"),
            &Image::filled(8, 6, WHITE),
        );
        let outcome =
            jals_exec::block_on_inline(verifier.verify(&shot("title"), &dir.path().join("run")));
        let ShotOutcome::Differed(diff) = outcome else {
            panic!("expected a difference");
        };
        assert_eq!(diff.size_mismatch, Some((8, 8, 8, 6)));
        assert_eq!(diff.differing, 0);
    }

    /// The report is written by the program under test, so a path in it is a claim. These are the
    /// claims that are refused before any file is opened.
    #[test]
    fn a_reported_path_that_leaves_the_screenshot_directory_is_refused() {
        let (dir, verifier) = fixture(&declared());
        let run = dir.path().join("run");
        // Something a `--update-golden` would otherwise package and publish.
        write(&dir.path().join("secret.png"), &Image::filled(4, 4, WHITE));
        write(
            &run.join("screenshots/title.png"),
            &Image::filled(4, 4, WHITE),
        );
        write(
            &dir.path().join("golden/title.png"),
            &Image::filled(4, 4, WHITE),
        );

        for claimed in [
            "../secret.png",
            "screenshots/../../secret.png",
            "/etc/passwd",
            // Inside the run directory, but not where the target says its screenshots go.
            "logs/latest.png",
        ] {
            let outcome = jals_exec::block_on_inline(verifier.verify(
                &Shot {
                    name: "title".to_owned(),
                    path: claimed.to_owned(),
                },
                &run,
            ));
            assert!(
                matches!(outcome, ShotOutcome::Misplaced { .. }),
                "`{claimed}` should not be admitted, got {outcome:?}"
            );
            assert!(outcome.is_failure(), "`{claimed}` must fail its test");
        }

        // And the path the target actually declares still is.
        let outcome = jals_exec::block_on_inline(verifier.verify(&shot("title"), &run));
        assert!(
            matches!(outcome, ShotOutcome::Matched { .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_reference_nothing_shot_is_reported_so_a_dropped_test_cannot_pass_quietly() {
        let (dir, verifier) = fixture(&declared());
        write(
            &dir.path().join("golden/title.png"),
            &Image::filled(4, 4, WHITE),
        );
        write(
            &dir.path().join("golden/hud.png"),
            &Image::filled(4, 4, WHITE),
        );
        let missing =
            jals_exec::block_on_inline(verifier.unmatched_references(&["title".to_owned()]))
                .expect("the golden directory is readable");
        assert_eq!(missing, ["hud"]);
    }
}
