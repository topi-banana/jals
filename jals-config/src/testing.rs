//! `[[test-target]]` and `[[golden.<name>]]`: running something other than the generated harness,
//! and the reference images its screenshots are judged against.
//!
//! `jals test` has one shape today — compile the project's `#[test]` methods, generate a harness,
//! run each test in its own JVM, read a sentinel line. That shape is right for a unit test and
//! wrong for anything that has to *boot*. A test target is the second shape: one long-lived
//! process, started once, which runs every selected test itself and says what happened in a report
//! it writes.
//!
//! Two things follow from that and are worth stating, because both are choices:
//!
//! - **The target names a `main-class` and jals does not infer one.** A harness jals generated is
//!   a class jals can name; a program someone else wrote is not. So the manifest says which class
//!   to start, and the placeholders below are how it reaches the paths only the host knows.
//! - **The golden images are a *fetched artifact*, exactly like a mapping set.** They are binary,
//!   they are large, and they are regenerated whenever the renderer moves — three properties that
//!   make them a bad fit for a repository and a good fit for the machinery `jals.toml` already has
//!   for pinning bytes by digest. [`GoldenEntry`] is deliberately the same shape as
//!   [`MappingEntry`](crate::manifest::MappingEntry), gate and exclusivity check included.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;

use serde::Deserialize;

use jals_storage::FileKey;

/// The build feature name a manifest may not gate on, because it is not a feature a selection
/// carries.
const DEFAULT_BUILD_FEATURE: &str = "default";

/// The character that makes a feature name a *routed* one (`dependency/feature`) rather than a
/// local one.
const FEATURE_SEPARATOR: char = '/';

/// One `[[test-target]]`: a program `jals test --target <name>` starts instead of the generated
/// harness.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct TestTarget {
    /// The name `--target` selects, unique across the array.
    pub name: String,
    /// Source roots compiled for this target *in addition to* `[build] source-dirs`, exactly as
    /// `[test] source-dirs` is additive.
    pub source_dirs: Vec<String>,
    /// Where this target's `.class` files go. Its own directory by default, because a lowering
    /// prunes whatever its destination holds and does not name.
    pub classes_dir: String,
    /// The fully-qualified class whose `main` is started.
    pub main_class: String,
    /// Arguments passed after the main class, with placeholders expanded (see
    /// [`Placeholder`]).
    ///
    /// The ids of the selected tests follow these, one argument each — the same shape the
    /// generated harness takes, so a target and a harness are driven by one contract.
    pub args: Vec<String>,
    /// The argument that makes the program enumerate its tests instead of running them.
    ///
    /// A target answers it by printing one `<id>` TAB `<flags>` line per test, exactly as the
    /// generated harness answers `--list`. Enumerating has to be possible **without** running
    /// anything: `jals test --list` on a target that boots a game must not boot the game, and the
    /// filters and `--partition` are applied to the list before a process is started.
    pub list_argument: String,
    /// JVM arguments passed before the classpath, with the same placeholders expanded.
    pub jvm_args: Vec<String>,
    /// Where the program writes what happened.
    pub report: Report,
    /// How the program's working directory is prepared.
    pub run_dir: RunDir,
    /// Glob patterns naming what to keep out of the run directory when a test fails — logs, crash
    /// reports, the screenshots themselves. Matched below the run directory.
    ///
    /// Empty by default and worth setting: a failing run whose only trace is an exit status tells
    /// a reader nothing they can act on.
    pub artifacts: Vec<String>,
    /// The reference screenshots this target's shots are compared against, naming a
    /// `[[golden.<name>]]` key. Absent when the target takes no screenshots.
    pub golden: Option<GoldenRef>,
    /// How screenshots are compared.
    pub screenshots: Screenshots,
    /// Seconds before the process is killed. `None` waits indefinitely.
    pub timeout: Option<u64>,
}

/// Where a `[[test-target]]`'s classes go when it names no `classes-dir`.
const DEFAULT_TARGET_CLASSES_ROOT: &str = "target/jals/test-target";

/// The enumerate argument a target takes when it names none. The generated harness's own spelling,
/// so the two contracts read alike.
const DEFAULT_LIST_ARGUMENT: &str = "--list";

impl Default for TestTarget {
    fn default() -> Self {
        Self {
            name: String::new(),
            source_dirs: Vec::new(),
            classes_dir: String::new(),
            main_class: String::new(),
            args: Vec::new(),
            list_argument: DEFAULT_LIST_ARGUMENT.to_owned(),
            jvm_args: Vec::new(),
            report: Report::default(),
            run_dir: RunDir::default(),
            artifacts: Vec::new(),
            golden: None,
            screenshots: Screenshots::default(),
            timeout: None,
        }
    }
}

impl TestTarget {
    /// This target's class output directory, defaulted from its name.
    ///
    /// Defaulted here rather than in [`Default`] because the default depends on `name`, which
    /// serde fills in after constructing the default value.
    #[must_use]
    pub fn classes_dir(&self) -> String {
        if self.classes_dir.is_empty() {
            alloc::format!("{DEFAULT_TARGET_CLASSES_ROOT}/{}", self.name)
        } else {
            self.classes_dir.clone()
        }
    }

    /// Apply the value-level checks serde cannot express.
    ///
    /// # Errors
    /// The first [`TestTargetError`] found.
    pub(crate) fn validate(&self) -> Result<(), TestTargetError> {
        if self.name.is_empty() {
            return Err(TestTargetError::Empty {
                name: String::new(),
                field: "name",
            });
        }
        // The name reaches a filesystem path through `classes_dir`'s default and the staging root,
        // so it is restricted to what is safe there rather than merely non-empty.
        if !self
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(TestTargetError::InvalidName {
                name: self.name.clone(),
            });
        }
        if self.main_class.is_empty() {
            return Err(TestTargetError::Empty {
                name: self.name.clone(),
                field: "main-class",
            });
        }
        if self.list_argument.is_empty() {
            return Err(TestTargetError::Empty {
                name: self.name.clone(),
                field: "list-argument",
            });
        }
        if self.report.file.is_empty() {
            return Err(TestTargetError::Empty {
                name: self.name.clone(),
                field: "report.file",
            });
        }
        FileKey::parse(&self.report.file).map_err(|_| TestTargetError::InvalidPath {
            name: self.name.clone(),
            field: "report.file",
            value: self.report.file.clone(),
        })?;
        if self.run_dir.seed.as_ref().is_some_and(String::is_empty) {
            return Err(TestTargetError::Empty {
                name: self.name.clone(),
                field: "run-dir.seed",
            });
        }
        self.screenshots.validate(&self.name)?;
        // A target that compares screenshots needs somewhere to compare them against, and a target
        // that names a golden set but takes no shots has written a line that does nothing. Both
        // are reported, because each is a manifest that does not mean what it says.
        match (&self.golden, self.screenshots.dir.is_empty()) {
            (Some(_), true) => Err(TestTargetError::GoldenWithoutScreenshots {
                name: self.name.clone(),
            }),
            (None, false) => Err(TestTargetError::ScreenshotsWithoutGolden {
                name: self.name.clone(),
            }),
            _ => Ok(()),
        }
    }
}

/// `golden = { with = "…" }` — the `[[golden.<name>]]` key a target compares against.
///
/// A struct with one field rather than a bare string, so it reads the same as `[build] remap`'s
/// `{ with = "mojmap" }` and has room for a second key later without a schema break.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GoldenRef {
    /// The `[[golden.<name>]]` key.
    pub with: String,
}

/// `[test-target.report]` — where the started program says what happened.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct Report {
    /// The report's path below the run directory.
    pub file: String,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            file: "report.tsv".to_owned(),
        }
    }
}

/// `[test-target.run-dir]` — how the working directory is prepared before the process starts.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct RunDir {
    /// A directory in the project copied into the run directory before the process starts — the
    /// `options.txt`, `server.properties` or datapack a program needs in place to start at all.
    ///
    /// Copied rather than used directly, because the run is expected to write into its working
    /// directory and a test must not leave the project modified.
    pub seed: Option<String>,
}

/// `[test-target.screenshots]` — how a target's shots are compared against the golden set.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct Screenshots {
    /// Where the program writes its shots, below the run directory. Empty means the target takes
    /// no screenshots.
    pub dir: String,
    /// Matching sensitivity as a fraction of the maximum perceptual distance; `0.0` — the default
    /// — means any difference counts.
    ///
    /// **Leave it at zero unless a measurement says otherwise.** A pinned software renderer
    /// reproduces a frame byte for byte, so there is no run-to-run noise for a threshold to
    /// absorb; what a loose one absorbs instead is real failure. A whole rasterizer swap changes
    /// about 12% of pixels, and a threshold of `0.05` already reports that as a clean pass.
    pub threshold: f64,
    /// The most differing pixels a shot may have and still pass.
    pub max_diff_pixels: Option<u32>,
    /// The largest differing fraction of compared pixels a shot may have and still pass.
    pub max_diff_ratio: Option<f64>,
    /// Regions excluded from every comparison — the splash text, a clock, anything whose content
    /// legitimately varies.
    pub masks: Vec<Mask>,
}

impl Screenshots {
    fn validate(&self, target: &str) -> Result<(), TestTargetError> {
        if !self.dir.is_empty() {
            FileKey::parse(&self.dir).map_err(|_| TestTargetError::InvalidPath {
                name: target.to_owned(),
                field: "screenshots.dir",
                value: self.dir.clone(),
            })?;
        }
        if !(0.0..=1.0).contains(&self.threshold) {
            return Err(TestTargetError::Threshold {
                name: target.to_owned(),
                value: self.threshold,
            });
        }
        if let Some(ratio) = self.max_diff_ratio
            && !(0.0..=1.0).contains(&ratio)
        {
            return Err(TestTargetError::DiffRatio {
                name: target.to_owned(),
                value: ratio,
            });
        }
        for mask in &self.masks {
            if mask.right <= mask.left || mask.bottom <= mask.top {
                return Err(TestTargetError::EmptyMask {
                    name: target.to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// One excluded rectangle, half-open on both axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Mask {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

/// A placeholder a target's `args` and `jvm-args` may contain.
///
/// Placeholders exist because the three things a started program needs to be told are things the
/// *host* computes and the manifest cannot spell: a scratch directory named after the process, and
/// the content-addressed directories a build task's artifacts were materialized into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder {
    /// `{run-dir}` — the working directory the process is started in.
    RunDir,
    /// `{dir:<name>}` — the materialized directory a build task published under `<name>`.
    RuntimeDir,
}

impl Placeholder {
    /// The literal `{run-dir}` spells.
    pub const RUN_DIR: &'static str = "{run-dir}";
    /// What a `{dir:<name>}` placeholder opens with.
    pub const RUNTIME_DIR_PREFIX: &'static str = "{dir:";
    /// What every placeholder closes with.
    pub const CLOSE: char = '}';
}

/// Named golden image sets (`[[golden.<name>]]`), in one of two forms.
///
/// The same either/or [`MappingEntry`](crate::manifest::MappingEntry) uses, for the same reason: a
/// project that targets several releases needs one golden set per release under one name, gated so
/// that at most one is ever active.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum GoldenEntry {
    /// `[golden.<name>]` — one set, active whenever its own `required-features` are met.
    One(GoldenSource),
    /// `[[golden.<name>]]` — alternatives in declaration order.
    Many(Vec<GoldenSource>),
}

impl GoldenEntry {
    /// Every alternative, in declaration order.
    #[must_use]
    pub fn alternatives(&self) -> &[GoldenSource] {
        match self {
            Self::One(source) => core::slice::from_ref(source),
            Self::Many(sources) => sources,
        }
    }

    /// The one alternative `enabled` activates, or `None` when none does.
    ///
    /// `None` is not a mistake: it is how a manifest says "this selection has no reference images
    /// yet", which is exactly the state a target is in the first time it runs.
    ///
    /// # Errors
    /// [`AmbiguousGolden`] when more than one alternative is active. Unreachable for a validated
    /// manifest, and still an error rather than a tiebreak — which reference set a screenshot was
    /// judged against is not a question a silent first-wins rule should answer.
    pub fn active(
        &self,
        name: &str,
        enabled: &BTreeSet<String>,
    ) -> Result<Option<&GoldenSource>, AmbiguousGolden> {
        let mut active = self
            .alternatives()
            .iter()
            .enumerate()
            .filter(|(_, source)| source.is_active(enabled));
        let Some((first, source)) = active.next() else {
            return Ok(None);
        };
        match active.next() {
            Some((second, _)) => Err(AmbiguousGolden {
                name: name.to_owned(),
                first: first + 1,
                second: second + 1,
            }),
            None => Ok(Some(source)),
        }
    }

    /// Apply the per-alternative checks and the cross-alternative exclusivity rule.
    ///
    /// # Errors
    /// The first [`GoldenError`] found.
    pub(crate) fn validate(&self, name: &str) -> Result<(), GoldenError> {
        let alternatives = self.alternatives();
        if alternatives.is_empty() {
            return Err(GoldenError::NoAlternatives {
                name: name.to_owned(),
            });
        }
        for source in alternatives {
            source.validate(name)?;
        }
        // The same statically decided exclusivity `[[mappings.x]]` carries: if one alternative's
        // gate is a subset of another's, every selection that activates the superset activates the
        // subset too, so the pair could be ambiguous and the manifest is rejected before any
        // selection exists.
        let gates: Vec<BTreeSet<&str>> = alternatives
            .iter()
            .map(|source| {
                source
                    .required_features
                    .iter()
                    .map(String::as_str)
                    .collect()
            })
            .collect();
        for (i, left) in gates.iter().enumerate() {
            for (j, right) in gates.iter().enumerate().skip(i + 1) {
                if left.is_subset(right) || right.is_subset(left) {
                    return Err(GoldenError::AmbiguousAlternatives {
                        name: name.to_owned(),
                        first: i + 1,
                        second: j + 1,
                    });
                }
            }
        }
        Ok(())
    }
}

/// One golden set: an archive of reference images.
///
/// Unlike a mapping set this has one digest field and not two. A mapping text is published by
/// someone else and jals takes the digest they publish, which for Mojang is SHA-1; a golden
/// archive is produced by the project that consumes it, so there is exactly one algorithm to
/// choose and no legacy to accommodate.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GoldenSource {
    /// An `https://` URL naming an archive of PNG files, one per screenshot name.
    pub url: String,
    /// The expected SHA-256 of the fetched archive, hex.
    pub sha256: String,
    /// The byte cap for the fetch. Required, like every other fetch jals performs.
    pub max_bytes: u64,
    /// The build features that must **all** be enabled for this entry to be active.
    #[serde(default)]
    required_features: Vec<String>,
}

impl GoldenSource {
    /// How long a hex SHA-256 is.
    const SHA256_HEX_LEN: usize = 64;

    /// Whether `enabled` satisfies this entry's `required-features`.
    fn is_active(&self, enabled: &BTreeSet<String>) -> bool {
        self.required_features
            .iter()
            .all(|feature| enabled.contains(feature))
    }

    /// Apply the value-level checks serde cannot express.
    ///
    /// # Errors
    /// The first [`GoldenError`] found.
    fn validate(&self, name: &str) -> Result<(), GoldenError> {
        if self.url.is_empty() {
            return Err(GoldenError::Empty {
                name: name.to_owned(),
                field: "url",
            });
        }
        if !self.url.starts_with("https://") {
            return Err(GoldenError::NotHttps {
                name: name.to_owned(),
                value: self.url.clone(),
            });
        }
        if self.sha256.len() != Self::SHA256_HEX_LEN
            || !self.sha256.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(GoldenError::Digest {
                name: name.to_owned(),
                value: self.sha256.clone(),
            });
        }
        if self.max_bytes == 0 {
            return Err(GoldenError::EmptyByteCap {
                name: name.to_owned(),
            });
        }
        for feature in &self.required_features {
            if feature.is_empty()
                || feature == DEFAULT_BUILD_FEATURE
                || feature.contains(FEATURE_SEPARATOR)
            {
                return Err(GoldenError::RequiredFeature {
                    name: name.to_owned(),
                    feature: feature.clone(),
                });
            }
        }
        Ok(())
    }
}

/// More than one alternative of one `[[golden.<name>]]` entry is active under a feature selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousGolden {
    name: String,
    /// The 1-based position of the first alternative found active.
    first: usize,
    /// The 1-based position of the second.
    second: usize,
}

impl fmt::Display for AmbiguousGolden {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            name,
            first,
            second,
        } = self;
        write!(
            f,
            "golden set `{name}` has two active alternatives (#{first} and #{second}): at most one \
             may be active under one feature selection"
        )
    }
}

impl Error for AmbiguousGolden {}

/// A `[[golden.<name>]]` entry that could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoldenError {
    /// A field expected to carry a value is present but empty.
    Empty { name: String, field: &'static str },
    /// `[[golden.<name>]]` was written with no alternatives at all.
    NoAlternatives { name: String },
    /// The `url` is not `https://`.
    NotHttps { name: String, value: String },
    /// The `sha256` is not 64 hex characters.
    Digest { name: String, value: String },
    /// `max-bytes` is zero, which would admit nothing.
    EmptyByteCap { name: String },
    /// A `required-features` entry that is empty, is `default`, or routes into a dependency.
    RequiredFeature { name: String, feature: String },
    /// Two alternatives whose gates are comparable by inclusion, so some selection activates both.
    AmbiguousAlternatives {
        name: String,
        first: usize,
        second: usize,
    },
}

impl fmt::Display for GoldenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { name, field } => {
                write!(f, "golden set `{name}` has an empty `{field}`")
            }
            Self::NoAlternatives { name } => write!(
                f,
                "golden set `{name}` declares no alternatives; write one `[golden.{name}]` table \
                 or at least one `[[golden.{name}]]`"
            ),
            Self::NotHttps { name, value } => write!(
                f,
                "golden set `{name}` has `url = \"{value}\"`, which is not `https://`"
            ),
            Self::Digest { name, value } => write!(
                f,
                "golden set `{name}` has `sha256 = \"{value}\"`, which is not 64 hex characters"
            ),
            Self::EmptyByteCap { name } => write!(
                f,
                "golden set `{name}` has `max-bytes = 0`, which would admit no archive at all"
            ),
            Self::RequiredFeature { name, feature } => write!(
                f,
                "golden set `{name}` requires feature `{feature}`, which is not a feature this \
                 package declares locally"
            ),
            Self::AmbiguousAlternatives {
                name,
                first,
                second,
            } => write!(
                f,
                "golden set `{name}` alternatives #{first} and #{second} have comparable \
                 `required-features`, so some selection would activate both"
            ),
        }
    }
}

impl Error for GoldenError {}

/// A `[[test-target]]` that could not be accepted.
#[derive(Debug, Clone, PartialEq)]
pub enum TestTargetError {
    /// A field expected to carry a value is present but empty.
    Empty { name: String, field: &'static str },
    /// A target name with a character that cannot appear in the paths derived from it.
    InvalidName { name: String },
    /// A path field that is not a portable, non-root project path.
    InvalidPath {
        name: String,
        field: &'static str,
        value: String,
    },
    /// `threshold` outside `0.0..=1.0`.
    Threshold { name: String, value: f64 },
    /// `max-diff-ratio` outside `0.0..=1.0`.
    DiffRatio { name: String, value: f64 },
    /// A mask that covers no pixel.
    EmptyMask { name: String },
    /// Two targets with the same `name`.
    DuplicateName { name: String },
    /// `golden` names a `[[golden.<name>]]` key the manifest does not declare.
    UnknownGolden { name: String, golden: String },
    /// A target that names a golden set but declares no `screenshots.dir`.
    GoldenWithoutScreenshots { name: String },
    /// A target that takes screenshots but names no golden set to judge them against.
    ScreenshotsWithoutGolden { name: String },
}

impl fmt::Display for TestTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { name, field } if name.is_empty() => {
                write!(f, "a `[[test-target]]` has an empty `{field}`")
            }
            Self::Empty { name, field } => {
                write!(f, "test target `{name}` has an empty `{field}`")
            }
            Self::InvalidName { name } => write!(
                f,
                "test target name `{name}` may hold only letters, digits, `-` and `_`: the name \
                 becomes a directory under `target/`"
            ),
            Self::InvalidPath { name, field, value } => write!(
                f,
                "test target `{name}` has `{field} = \"{value}\"`, which is not a relative path \
                 inside the project"
            ),
            Self::Threshold { name, value } => write!(
                f,
                "test target `{name}` has `threshold = {value}`, which is outside 0.0..=1.0"
            ),
            Self::DiffRatio { name, value } => write!(
                f,
                "test target `{name}` has `max-diff-ratio = {value}`, which is outside 0.0..=1.0"
            ),
            Self::EmptyMask { name } => write!(
                f,
                "test target `{name}` declares a mask that covers no pixel: `right` must exceed \
                 `left` and `bottom` must exceed `top`"
            ),
            Self::DuplicateName { name } => {
                write!(f, "two `[[test-target]]` entries are both named `{name}`")
            }
            Self::UnknownGolden { name, golden } => write!(
                f,
                "test target `{name}` compares against golden set `{golden}`, which no \
                 `[[golden.{golden}]]` declares"
            ),
            Self::GoldenWithoutScreenshots { name } => write!(
                f,
                "test target `{name}` names a golden set but declares no `screenshots.dir`, so \
                 there is nothing to compare against it"
            ),
            Self::ScreenshotsWithoutGolden { name } => write!(
                f,
                "test target `{name}` declares `screenshots.dir` but no `golden`, so its shots \
                 would be taken and never judged"
            ),
        }
    }
}

impl Error for TestTargetError {}

#[cfg(test)]
mod tests {
    use super::{
        GoldenEntry, GoldenError, GoldenSource, Mask, Screenshots, TestTarget, TestTargetError,
    };
    use alloc::borrow::ToOwned;
    use alloc::collections::BTreeSet;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    fn source(features: &[&str]) -> GoldenSource {
        GoldenSource {
            url: "https://example.invalid/golden.zip".to_owned(),
            sha256: "a".repeat(64),
            max_bytes: 1024,
            required_features: features.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    fn target() -> TestTarget {
        TestTarget {
            name: "client-e2e".to_owned(),
            main_class: "com.example.Driver".to_owned(),
            ..TestTarget::default()
        }
    }

    #[test]
    fn a_target_defaults_its_classes_dir_from_its_name() {
        assert_eq!(target().classes_dir(), "target/jals/test-target/client-e2e");
        let explicit = TestTarget {
            classes_dir: "out".to_owned(),
            ..target()
        };
        assert_eq!(explicit.classes_dir(), "out");
    }

    #[test]
    fn a_name_that_would_escape_its_directory_is_refused() {
        for bad in ["../evil", "a/b", "has space", ""] {
            let candidate = TestTarget {
                name: bad.to_owned(),
                ..target()
            };
            assert!(
                candidate.validate().is_err(),
                "`{bad}` should not be a target name"
            );
        }
    }

    #[test]
    fn screenshots_and_golden_must_be_declared_together() {
        let shots_only = TestTarget {
            screenshots: Screenshots {
                dir: "screenshots".to_owned(),
                ..Screenshots::default()
            },
            ..target()
        };
        assert!(matches!(
            shots_only.validate(),
            Err(TestTargetError::ScreenshotsWithoutGolden { .. })
        ));

        let golden_only = TestTarget {
            golden: Some(super::GoldenRef {
                with: "client-e2e".to_owned(),
            }),
            ..target()
        };
        assert!(matches!(
            golden_only.validate(),
            Err(TestTargetError::GoldenWithoutScreenshots { .. })
        ));
    }

    #[test]
    fn a_threshold_outside_zero_to_one_is_refused() {
        for bad in [-0.1, 1.5] {
            let candidate = TestTarget {
                screenshots: Screenshots {
                    dir: "shots".to_owned(),
                    threshold: bad,
                    ..Screenshots::default()
                },
                golden: Some(super::GoldenRef {
                    with: "g".to_owned(),
                }),
                ..target()
            };
            assert!(matches!(
                candidate.validate(),
                Err(TestTargetError::Threshold { .. })
            ));
        }
    }

    #[test]
    fn a_mask_that_covers_nothing_is_refused() {
        let candidate = TestTarget {
            screenshots: Screenshots {
                dir: "shots".to_owned(),
                masks: vec![Mask {
                    left: 5,
                    top: 5,
                    right: 5,
                    bottom: 9,
                }],
                ..Screenshots::default()
            },
            golden: Some(super::GoldenRef {
                with: "g".to_owned(),
            }),
            ..target()
        };
        assert!(matches!(
            candidate.validate(),
            Err(TestTargetError::EmptyMask { .. })
        ));
    }

    #[test]
    fn a_golden_url_must_be_https_with_a_full_sha256_and_a_cap() {
        let bad_scheme = GoldenSource {
            url: "http://example.invalid/g.zip".to_owned(),
            ..source(&[])
        };
        assert!(matches!(
            GoldenEntry::One(bad_scheme).validate("g"),
            Err(GoldenError::NotHttps { .. })
        ));

        let short_digest = GoldenSource {
            sha256: "abc".to_owned(),
            ..source(&[])
        };
        assert!(matches!(
            GoldenEntry::One(short_digest).validate("g"),
            Err(GoldenError::Digest { .. })
        ));

        let no_cap = GoldenSource {
            max_bytes: 0,
            ..source(&[])
        };
        assert!(matches!(
            GoldenEntry::One(no_cap).validate("g"),
            Err(GoldenError::EmptyByteCap { .. })
        ));
    }

    #[test]
    fn comparable_alternatives_are_rejected_statically() {
        // `["a"]` is a subset of `["a", "b"]`, so a selection naming both activates both.
        let entry = GoldenEntry::Many(vec![source(&["a"]), source(&["a", "b"])]);
        assert!(matches!(
            entry.validate("g"),
            Err(GoldenError::AmbiguousAlternatives {
                first: 1,
                second: 2,
                ..
            })
        ));

        // Disjoint gates are fine, and this is the shape a per-release table takes.
        let ok = GoldenEntry::Many(vec![source(&["1.20.1"]), source(&["1.21.11"])]);
        assert_eq!(ok.validate("g"), Ok(()));
    }

    #[test]
    fn an_empty_alternative_list_is_refused() {
        assert!(matches!(
            GoldenEntry::Many(Vec::new()).validate("g"),
            Err(GoldenError::NoAlternatives { .. })
        ));
    }

    #[test]
    fn selection_picks_the_one_gate_it_satisfies_and_none_is_not_an_error() {
        let entry = GoldenEntry::Many(vec![source(&["1.20.1"]), source(&["1.21.11"])]);
        let enabled: BTreeSet<String> = core::iter::once("1.21.11".to_owned()).collect();
        let picked = entry.active("g", &enabled).expect("unambiguous");
        assert_eq!(picked, Some(&entry.alternatives()[1]));

        // No release selected: no reference images, which is a state and not a failure.
        let none = entry.active("g", &BTreeSet::new()).expect("unambiguous");
        assert_eq!(none, None);
    }

    #[test]
    fn a_gate_naming_default_or_a_routed_feature_is_refused() {
        for bad in ["default", "dep/feature", ""] {
            let entry = GoldenEntry::One(source(&[bad]));
            assert!(
                matches!(
                    entry.validate("g"),
                    Err(GoldenError::RequiredFeature { .. })
                ),
                "`{bad}` should not be a gate"
            );
        }
    }
}

/// The TOML these types actually parse from, checked end to end rather than by construction.
///
/// Kept apart from the unit tests above because these are the only tests here that go through
/// serde: a field renamed to the wrong kebab-case spelling, or a table that should have been an
/// array, is invisible to a test that builds the struct in Rust.
#[cfg(test)]
mod toml_tests {
    use crate::manifest::{Manifest, ValidationError};
    use alloc::borrow::ToOwned;
    use alloc::collections::BTreeSet;
    use alloc::string::String;

    const PROJECT: &str = r#"
[package]
name = "hellomod"

[[test-target]]
name = "client-e2e"
source-dirs = ["src/e2e/java"]
main-class = "net.minecraft.client.main.Main"
args = ["--width", "854", "--gameDir", "{run-dir}", "--assetsDir", "{dir:assets}"]
jvm-args = ["-Djava.library.path={dir:natives}"]
artifacts = ["logs/**", "screenshots/**"]
golden = { with = "client-e2e" }
timeout = 900

[test-target.report]
file = "report.tsv"

[test-target.run-dir]
seed = "fixtures/run"

[test-target.screenshots]
dir = "screenshots"
threshold = 0.0
max-diff-ratio = 0.0001
masks = [{ left = 0, top = 0, right = 200, bottom = 20 }]

[[golden.client-e2e]]
required-features = ["1.21.11"]
url = "https://example.invalid/golden-1.21.11.zip"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
max-bytes = 4194304

[[golden.client-e2e]]
required-features = ["1.20.1"]
url = "https://example.invalid/golden-1.20.1.zip"
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
max-bytes = 4194304
"#;

    #[test]
    fn the_documented_shape_parses_and_validates() {
        let manifest: Manifest = toml::from_str(PROJECT).expect("the example manifest parses");
        assert_eq!(manifest.validate(), Ok(()));

        let target = &manifest.test_target[0];
        assert_eq!(target.name, "client-e2e");
        assert_eq!(target.source_dirs, ["src/e2e/java"]);
        assert_eq!(target.main_class, "net.minecraft.client.main.Main");
        assert_eq!(target.report.file, "report.tsv");
        assert_eq!(target.run_dir.seed.as_deref(), Some("fixtures/run"));
        assert_eq!(target.golden.as_ref().expect("declared").with, "client-e2e");
        assert_eq!(target.timeout, Some(900));
        assert_eq!(target.screenshots.dir, "screenshots");
        assert_eq!(target.screenshots.masks.len(), 1);
        // Not named in the TOML, so it is derived from the target's name.
        assert_eq!(target.classes_dir(), "target/jals/test-target/client-e2e");

        // Both alternatives are read, and a selection picks exactly one.
        let entry = &manifest.golden["client-e2e"];
        assert_eq!(entry.alternatives().len(), 2);
        let enabled: BTreeSet<String> = core::iter::once("1.20.1".to_owned()).collect();
        let picked = entry
            .active("client-e2e", &enabled)
            .expect("unambiguous")
            .expect("one alternative is gated on 1.20.1");
        assert!(picked.url.ends_with("golden-1.20.1.zip"));
    }

    #[test]
    fn a_target_naming_an_undeclared_golden_set_is_rejected() {
        let text = PROJECT.replace("with = \"client-e2e\"", "with = \"nope\"");
        let manifest: Manifest = toml::from_str(&text).expect("parses");
        assert!(matches!(
            manifest.validate(),
            Err(ValidationError::TestTarget(
                super::TestTargetError::UnknownGolden { .. }
            ))
        ));
    }

    #[test]
    fn two_targets_with_one_name_are_rejected() {
        let text = alloc::format!(
            "{PROJECT}\n[[test-target]]\nname = \"client-e2e\"\nmain-class = \"X\"\n"
        );
        let manifest: Manifest = toml::from_str(&text).expect("parses");
        assert!(matches!(
            manifest.validate(),
            Err(ValidationError::TestTarget(
                super::TestTargetError::DuplicateName { .. }
            ))
        ));
    }

    #[test]
    fn an_unknown_key_in_a_target_is_a_parse_error_rather_than_a_late_diagnostic() {
        let text = PROJECT.replace("timeout = 900", "timeout = 900\nnope = 1");
        assert!(toml::from_str::<Manifest>(&text).is_err());
    }

    #[test]
    fn a_manifest_that_declares_no_target_is_unchanged() {
        let manifest: Manifest =
            toml::from_str("[package]\nname = \"x\"\n").expect("the smallest manifest");
        assert!(manifest.test_target.is_empty());
        assert!(manifest.golden.is_empty());
        assert_eq!(manifest.validate(), Ok(()));
    }
}
