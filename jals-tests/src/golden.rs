//! Golden-corpus *formatter* verification.
//!
//! The crate root ([`crate`]) checks parser invariants (never panics, lossless,
//! always a tree). This module checks something different: how close `jals-fmt`'s
//! output, driven by one native formatter's style [`Config`], comes to the output
//! of that formatter itself.
//!
//! A corpus is a directory tree of `*.input` / `*.output` pairs (the same naming
//! google-java-format uses for its own regression suite): `Foo.input` is the
//! unformatted source and `Foo.output` is what the reference formatter produces
//! from it. We format each `.input` and compare the result against the paired
//! `.output`.
//!
//! # Targets and tiers
//!
//! Every corpus names a [`Target`]: the native formatter that produced its
//! `.output` files, the [`Config`] jals scores it with, and the accuracy
//! [`Tier`] `DESIGN.md` §18.1 promises for it.
//!
//! - **T1** (`gjf`) — `jals-fmt` runs a port of google-java-format's own greedy
//!   `computeBreaks`, so byte-matching it is the stated goal rather than an
//!   impossibility. It is not reached yet.
//! - **T2** (`palantir`, `eclipse`, `intellij`) — these three resolve layout with
//!   algorithms jals deliberately does not port (`DESIGN.md` §11 conclusion 1),
//!   so byte match is **not promised**. Where a T2 exact rate is nonetheless high
//!   — Palantir is a google-java-format fork, so much of its layout coincides with
//!   the ported engine's — that is incidental, not a contract. Mean similarity is
//!   the number that tracks convergence.
//!
//! Because a byte-equal rate alone would hide progress — one space of difference
//! sinks a whole file — every target reports a **similarity** metric (the mean
//! line-level diff ratio, plus the count of exact matches) rather than a hard
//! pass/fail.
//!
//! # Version pins
//!
//! All four reference formatters are version-unstable across releases
//! (`DESIGN.md` §7.1 / §11 conclusion 6), so "matching" is only defined against a
//! [`Pin`]. Generated corpora pin a release, which `TOOL_PINS` holds and
//! `.github/workflows/ci.yml` must agree with; vendored corpora are pinned by
//! their submodule commit.

use std::path::{Path, PathBuf};

use jals_config::fmt::Config;
use jals_fmt::import::ConfigImporter;
use rayon::prelude::*;
use similar::TextDiff;
use walkdir::WalkDir;

/// How close to a reference formatter jals promises to come (`DESIGN.md` §18.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Byte match with the pinned reference is the goal — this is the engine's own
    /// native semantics, not an approximation.
    T1,
    /// Layout approximation. The reference resolves breaks with an algorithm jals does
    /// not port, so byte match is explicitly not promised.
    T2,
}

impl Tier {
    /// Short label for the report tables.
    pub const fn label(self) -> &'static str {
        match self {
            Self::T1 => "T1 · byte match",
            Self::T2 => "T2 · approximate",
        }
    }
}

/// A native formatter jals is scored against: the style it is scored with, and the
/// promise attached to that score.
pub struct Target {
    /// Stable identifier, used by `--style` and in the report tables.
    pub name: &'static str,
    /// The reference tool's own name, as it is released.
    tool: &'static str,
    /// What matching this target is promised to mean.
    tier: Tier,
    /// The jals [`Config`] that stands in for this formatter's style.
    ///
    /// A function rather than a value because [`Config`] is not const-constructible; for
    /// the file-backed formatters it parses the very config file the corpus generator
    /// hands the native tool, so the two can never drift.
    config: fn() -> Config,
}

impl Target {
    /// The Google Java Style [`Config`], as the google-java-format importer produces it.
    ///
    /// This used to be a hand-written `Config` literal that restated the same style twice —
    /// once here and once in `jals_fmt::import::gjf` — and drifted from it. The importer's
    /// family profile is now the single definition; see `jals-fmt/MAPPING.md` §5 for what each
    /// value is anchored to.
    #[must_use]
    fn google_config() -> Config {
        jals_fmt::import::GoogleJavaFormatConfig::default().into()
    }

    /// The Palantir Java Style [`Config`]: block 4 / continuation 8 / 120 columns, with
    /// Javadoc formatting off.
    ///
    /// Both Palantir corpora are generated that way — the vendored suite by
    /// `FormatterIntegrationTest` (`Style.PALANTIR`, a default `JavaFormatterOptions`
    /// builder), the OpenJDK one by the CLI's `--palantir` — which is exactly
    /// [`PalantirJavaFormatConfig`](jals_fmt::import::PalantirJavaFormatConfig)'s default.
    #[must_use]
    fn palantir_config() -> Config {
        jals_fmt::import::PalantirJavaFormatConfig::default().into()
    }

    /// The Eclipse JDT style, imported from the very profile the corpus was generated with.
    ///
    /// `jals-tests/config/eclipse-jals.prefs` is JDT's own built-in default profile;
    /// `gen-openjdk-corpus.sh eclipse` hands that file to JDT and this reads the same bytes
    /// back through the importer, so the corpus and its score cannot come from two different
    /// styles.
    ///
    /// # Panics
    /// If the committed profile stops parsing — a broken checkout or an importer
    /// regression, neither of which should be reported as a low similarity score.
    #[must_use]
    fn eclipse_config() -> Config {
        jals_fmt::import::EclipsePrefs::import(include_str!("../config/eclipse-jals.prefs"))
            .expect("the committed Eclipse profile should import")
    }

    /// The IntelliJ IDEA style, imported from the very scheme the corpus was generated with.
    ///
    /// `jals-tests/config/intellij-jals.xml` states the right margin, the indent options and
    /// the `KEEP_*` family (forced off, so the corpus does not carry `DESIGN.md` §18.2's D5);
    /// everything else is IDEA's default on one side and the importer's model default on the
    /// other. `gen-openjdk-corpus.sh intellij` hands IDEA that same file.
    ///
    /// # Panics
    /// If the committed scheme stops parsing — a broken checkout or an importer regression,
    /// neither of which should be reported as a low similarity score.
    #[must_use]
    fn intellij_config() -> Config {
        jals_fmt::import::IntellijXmlScheme::import(include_str!("../config/intellij-jals.xml"))
            .expect("the committed IntelliJ scheme should import")
    }

    /// This target's scoring [`Config`].
    #[must_use]
    fn config(&self) -> Config {
        (self.config)()
    }

    /// `tool version` — what the `.output` files of a corpus this target formatted at
    /// `pin` are defined by. Half the metric's meaning (`DESIGN.md` §7.1), so it is
    /// spelled once here rather than at each place a report names its reference.
    fn reference(&self, pin: Pin) -> String {
        format!("{} {}", self.tool, pin.version())
    }

    /// Look up a target by its `--style` name.
    pub fn by_name(name: &str) -> Option<&'static Self> {
        TARGETS.iter().copied().find(|t| t.name == name)
    }
}

/// google-java-format's Google Java Style — the one target byte match is promised for.
const GJF: Target = Target {
    name: "gjf",
    tool: "google-java-format",
    tier: Tier::T1,
    config: Target::google_config,
};

/// palantir-java-format's Palantir style.
const PALANTIR: Target = Target {
    name: "palantir",
    tool: "palantir-java-format",
    tier: Tier::T2,
    config: Target::palantir_config,
};

/// The Eclipse JDT formatter's built-in default profile.
const ECLIPSE: Target = Target {
    name: "eclipse",
    tool: "Eclipse JDT",
    tier: Tier::T2,
    config: Target::eclipse_config,
};

/// IntelliJ IDEA's Java code style, with the input-line-break memory turned off.
const INTELLIJ: Target = Target {
    name: "intellij",
    tool: "IntelliJ IDEA",
    tier: Tier::T2,
    config: Target::intellij_config,
};

/// Every native formatter the harness can score against. Add an entry here to register a new one.
pub const TARGETS: &[&Target] = &[&GJF, &PALANTIR, &ECLIPSE, &INTELLIJ];

/// The pinned release of each reference formatter that the generated corpora are built with.
///
/// `.github/workflows/ci.yml` downloads these exact versions, and
/// [`the pins match CI`](tests::the_tool_pins_match_ci) fails when the two drift.
const TOOL_PINS: &[(&str, &str)] = &[
    ("GJF_VERSION", "1.35.0"),
    ("PJF_VERSION", "2.96.0"),
    ("ECLIPSE_JDT_VERSION", "3.46.0"),
    ("IDEA_VERSION", "2025.3"),
];

/// What makes "the reference output" a defined thing for one corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// The corpus is the reference tool's own vendored test suite: the submodule commit
    /// this repository pins *is* the version.
    Submodule,
    /// The corpus was generated by a pinned release, named by its `TOOL_PINS` key.
    Release(&'static str),
    /// An ad-hoc `--dir` corpus: whatever produced it is the caller's business, so the
    /// number it yields is not comparable across runs.
    Unpinned,
}

impl Pin {
    /// The pinned version as it should be printed in a report.
    fn version(self) -> String {
        match self {
            Self::Submodule => "submodule pin".to_string(),
            Self::Unpinned => "unpinned".to_string(),
            Self::Release(key) => TOOL_PINS
                .iter()
                .find(|(k, _)| *k == key)
                .map_or_else(|| format!("<{key} unset>"), |(_, v)| (*v).to_string()),
        }
    }
}

/// A named golden corpus, rooted at a path relative to the `sources/` directory.
pub struct GoldenSource {
    /// Stable identifier used on the command line.
    pub name: &'static str,
    /// Root directory, relative to the `sources/` dir.
    pub root_rel: &'static str,
    /// The native formatter that produced this corpus's `.output` files.
    pub target: &'static Target,
    /// Which version of that formatter produced them.
    pub pin: Pin,
    /// Human-readable description.
    pub description: &'static str,
}

/// Every golden corpus the CLI knows about. Add an entry here to register a new one.
pub const GOLDEN_SOURCES: &[GoldenSource] = &[
    GoldenSource {
        name: "gjf-testdata",
        root_rel: "google-java-format/core/src/test/resources/com/google/googlejavaformat/java/testdata",
        target: &GJF,
        pin: Pin::Submodule,
        description: "google-java-format's own .input/.output regression corpus (Apache-2.0)",
    },
    GoldenSource {
        name: "openjdk-gjf",
        root_rel: "openjdk-gjf",
        target: &GJF,
        pin: Pin::Release("GJF_VERSION"),
        description: "OpenJDK src/ library sources formatted with google-java-format (generated; see scripts/gen-openjdk-corpus.sh)",
    },
    GoldenSource {
        name: "palantir-testdata",
        root_rel: "palantir-java-format/palantir-java-format/src/test/resources/com/palantir/javaformat/java/testdata",
        target: &PALANTIR,
        pin: Pin::Submodule,
        description: "palantir-java-format's own .input/.output regression corpus, Palantir style (Apache-2.0)",
    },
    GoldenSource {
        name: "openjdk-palantir",
        root_rel: "openjdk-palantir",
        target: &PALANTIR,
        pin: Pin::Release("PJF_VERSION"),
        description: "OpenJDK src/ library sources formatted with palantir-java-format (generated; see scripts/gen-openjdk-corpus.sh)",
    },
    GoldenSource {
        name: "openjdk-eclipse",
        root_rel: "openjdk-eclipse",
        target: &ECLIPSE,
        pin: Pin::Release("ECLIPSE_JDT_VERSION"),
        description: "OpenJDK src/ library sources formatted with the Eclipse JDT default profile (generated; see scripts/gen-openjdk-corpus.sh)",
    },
    GoldenSource {
        name: "openjdk-intellij",
        root_rel: "openjdk-intellij",
        target: &INTELLIJ,
        pin: Pin::Release("IDEA_VERSION"),
        description: "OpenJDK src/java.base only — IDEA's formatter runs a whole IDE, so this corpus is narrower than the other three (generated; see scripts/gen-openjdk-corpus.sh)",
    },
];

impl GoldenSource {
    /// Look up a golden source by its command-line name.
    pub fn by_name(name: &str) -> Option<&'static Self> {
        GOLDEN_SOURCES.iter().find(|s| s.name == name)
    }
}

/// The outcome of formatting a single `.input` and comparing it to its `.output`.
#[derive(Debug, Clone)]
pub struct PairResult {
    /// The `.input` path, relative to the corpus root.
    pub rel: PathBuf,
    /// Line-level similarity of the formatted output to the expected output, in
    /// `0.0..=1.0` (1.0 = identical). The Ratcliff/Obershelp ratio over lines.
    pub similarity: f64,
    /// Whether the formatted output is byte-for-byte equal to the expected output.
    exact: bool,
}

impl PairResult {
    /// Format `input` with `cfg` and score it against the expected `expected` output:
    /// a line-level similarity ratio plus whether the two are byte-identical.
    fn score(input: &str, expected: &str, cfg: &Config) -> (f64, bool) {
        let formatted =
            jals_exec::block_on_inline(jals_fmt::FormatOutput::format_source(input, cfg)).formatted;
        let exact = formatted == expected;
        let ratio = TextDiff::from_lines(expected, &formatted).ratio() as f64;
        (ratio, exact)
    }
}

/// Aggregated golden outcomes for one corpus.
#[derive(Debug, Clone)]
pub struct GoldenReport {
    /// Corpus name.
    pub name: String,
    /// `tool version` — the reference the `.output` files came from.
    pub reference: String,
    /// The accuracy promised for that reference.
    pub tier: Tier,
    /// Resolved root directory that was walked.
    pub root: PathBuf,
    /// Total `.input`/`.output` pairs found.
    pub total: usize,
    /// Pairs whose formatted output exactly matched the expected output.
    pub exact: usize,
    /// Mean line-level similarity across all pairs (`0.0..=1.0`).
    pub mean_similarity: f64,
    /// Every pair's result, sorted worst (lowest similarity) first.
    pub results: Vec<PairResult>,
}

impl GoldenReport {
    /// Fraction of pairs (0.0–1.0) that matched exactly.
    pub fn exact_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.exact as f64 / self.total as f64
        }
    }

    /// Recursively collect every `*.input` under `root` that has a sibling `*.output`.
    fn collect_pairs(root: &Path) -> Vec<(PathBuf, PathBuf)> {
        WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(walkdir::DirEntry::into_path)
            .filter(|path| path.extension().is_some_and(|ext| ext == "input"))
            .filter_map(|input| {
                let output = input.with_extension("output");
                output.is_file().then_some((input, output))
            })
            .collect()
    }

    /// Walk `root`, format every `.input` with `target`'s style in parallel, and aggregate
    /// the similarity of each result to its `.output`.
    ///
    /// `pin` records which release of `target`'s tool produced those `.output` files; it is
    /// carried into the report because a similarity number is only comparable against a
    /// fixed reference version (`DESIGN.md` §7.1).
    pub fn run(name: &str, root: &Path, target: &Target, pin: Pin) -> Self {
        let cfg = &target.config();
        let mut results: Vec<PairResult> = Self::collect_pairs(root)
            .into_par_iter()
            .filter_map(|(input_path, output_path)| {
                let input = std::fs::read_to_string(&input_path).ok()?;
                let expected = std::fs::read_to_string(&output_path).ok()?;
                let (similarity, exact) = PairResult::score(&input, &expected, cfg);
                let rel = input_path
                    .strip_prefix(root)
                    .unwrap_or(&input_path)
                    .to_path_buf();
                Some(PairResult {
                    rel,
                    similarity,
                    exact,
                })
            })
            .collect();

        // Worst first, so a truncated listing surfaces the most divergent constructs.
        results.sort_by(|a, b| a.similarity.total_cmp(&b.similarity));

        let total = results.len();
        let exact = results.iter().filter(|r| r.exact).count();
        let mean_similarity = if total == 0 {
            0.0
        } else {
            results.iter().map(|r| r.similarity).sum::<f64>() / total as f64
        };

        Self {
            name: name.to_string(),
            reference: target.reference(pin),
            tier: target.tier,
            root: root.to_path_buf(),
            total,
            exact,
            mean_similarity,
            results,
        }
    }

    /// Render the reports as a GitHub-flavored Markdown summary, suitable for a CI step
    /// summary or a pull-request comment.
    ///
    /// `worst` is how many least-similar files to list per corpus (0 = none); the list
    /// is wrapped in a collapsed `<details>` so it stays tidy in a PR comment.
    pub fn markdown_report(reports: &[Self], worst: usize) -> String {
        let mut out = String::from("## jals-fmt vs native formatters\n\n");
        out.push_str(
            "Similarity of `jals-fmt` output, configured for each target's style, to that \
             formatter's own output. Only **T1** (google-java-format) promises a byte match; \
             the **T2** targets resolve line breaks with algorithms jals does not port \
             (`jals-fmt/DESIGN.md` §18), so any exact match they show is incidental rather \
             than contracted, and mean similarity is the number that tracks convergence.\n\n",
        );
        out.push_str(
            "| corpus | reference | tier | pairs | exact | exact rate | mean similarity |\n",
        );
        out.push_str("| --- | --- | --- | --: | --: | --: | --: |\n");
        for r in reports {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {:.2}% | {:.2}% |\n",
                r.name,
                r.reference,
                r.tier.label(),
                r.total,
                r.exact,
                r.exact_rate() * 100.0,
                r.mean_similarity * 100.0
            ));
        }
        if worst > 0 {
            for r in reports {
                // Only the inexact files are worth listing; exact matches are at 100%.
                let divergent: Vec<&PairResult> =
                    r.results.iter().filter(|p| !p.exact).take(worst).collect();
                if divergent.is_empty() {
                    continue;
                }
                out.push_str(&format!(
                    "\n<details><summary>{}: {} least similar</summary>\n\n",
                    r.name,
                    divergent.len()
                ));
                out.push_str("| similarity | file |\n| --: | --- |\n");
                for p in divergent {
                    out.push_str(&format!(
                        "| {:.2}% | `{}` |\n",
                        p.similarity * 100.0,
                        p.rel.display()
                    ));
                }
                out.push_str("\n</details>\n");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use jals_config::fmt::{ImportOrder, IndentStyle, ParenPositions, WrapPolicy};

    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn google_config_has_google_defaults() {
        let c = Target::google_config();
        assert_eq!(c.layout.indent_style, IndentStyle::Space);
        assert_eq!(c.layout.indent_width, 2);
        // Google style wraps continuation lines at +4 columns (double the +2 block indent).
        assert_eq!(c.layout.continuation_indent, Some(4));
        assert_eq!(c.layout.max_width, 100);
        // Imports: a static group, a blank line, then everything else, each sorted.
        assert_eq!(c.imports.order, ImportOrder::Group);
        assert_eq!(c.imports.groups, ["static", "*"]);
        assert!(c.imports.reorder_modifiers);
        // The comment rewrites that exist to mirror google-java-format.
        assert!(c.comments.normalize_parameter_comments);
        assert!(c.comments.inline_block_comments);
        assert!(c.wrapping.tabular_array_initializers);
        // A chain that does not fit goes one call per line, and so does a long `case` label list.
        assert_eq!(c.wrapping.method_chain, WrapPolicy::IfLongPerItem);
        assert_eq!(c.wrapping.case_labels, WrapPolicy::IfLongPerItem);
        // google-java-format spaces the enhanced-`for` colon and never dangles a `)`.
        assert!(c.spacing.before_foreach_colon);
        assert_eq!(
            c.wrapping.paren_method_invocation,
            ParenPositions::CommonLines
        );
    }

    #[test]
    fn the_google_preset_toml_matches_the_importer() {
        // `jals-fmt.toml` at the workspace root is the readable form of the same preset. If it
        // drifts, the file stops documenting what the harness actually scores against.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../jals-fmt.toml");
        let text = fs::read_to_string(&path).expect("the preset file should exist");
        let from_file: Config = toml::from_str(&text).expect("the preset should parse");
        assert_eq!(from_file, Target::google_config());
    }

    /// Every documented `jalsfmt.toml` sample, with the path it lives at.
    ///
    /// Each one claims its values *are* the defaults, so each has to parse and come back equal
    /// to `Config::default()`. Without this they drift silently: the config is sectioned and
    /// `Config` ignores unknown keys, so a sample written against an older schema still parses —
    /// it just yields the defaults instead of what it says.
    const DEFAULT_SAMPLES: [&str; 3] = [
        "../jals-fmt/jalsfmt.toml",
        "../README.md",
        "../README_jp.md",
    ];

    /// Pull the `jalsfmt.toml` fenced TOML block out of a Markdown page, or take the whole file
    /// when it already is one.
    fn default_sample(text: &str, path: &str) -> String {
        if !path.ends_with(".md") {
            return text.to_owned();
        }
        text.split("```toml")
            .skip(1)
            .filter_map(|block| block.split("```").next())
            .find(|block| block.contains("# jalsfmt.toml"))
            .unwrap_or_else(|| panic!("{path} should document a jalsfmt.toml block"))
            .to_owned()
    }

    #[test]
    fn the_documented_samples_are_the_defaults() {
        for path in DEFAULT_SAMPLES {
            let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
            let text = fs::read_to_string(&full).unwrap_or_else(|_| panic!("{path} should exist"));
            let sample = default_sample(&text, path);
            let parsed: Config = toml::from_str(&sample)
                .unwrap_or_else(|err| panic!("{path}: the sample should parse: {err}"));
            assert_eq!(
                parsed,
                Config::default(),
                "{path} drifted from the defaults"
            );
        }
    }

    #[test]
    fn score_is_one_for_already_formatted_input() {
        // A trivially-formatted class, in Google's 2-space style, is a fixed point.
        let cfg = Target::google_config();
        let expected = "class A {\n  void m() {}\n}\n";
        let (similarity, exact) = PairResult::score(expected, expected, &cfg);
        assert!(exact, "expected an exact match for already-formatted input");
        assert_eq!(similarity, 1.0);
    }

    #[test]
    fn score_rewards_closeness() {
        // The formatted input matches the expected output except for one extra line:
        // not exact, but highly similar. (Independent of any wrapping behavior — the
        // input is already in Google's 2-space style, so jals reproduces it verbatim.)
        let cfg = Target::google_config();
        let input = "class A {\n  int x;\n}\n";
        let expected = "class A {\n  int x;\n  int y;\n}\n";
        let (similarity, exact) = PairResult::score(input, expected, &cfg);
        assert!(!exact, "the extra `int y;` line should make this inexact");
        assert!(
            similarity > 0.5 && similarity < 1.0,
            "similarity was {similarity}"
        );
    }

    #[test]
    fn collect_pairs_finds_only_paired_inputs() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("A.input"), "class A{}").unwrap();
        fs::write(dir.path().join("A.output"), "class A {}\n").unwrap();
        // An input with no matching output is not a pair.
        fs::write(dir.path().join("B.input"), "class B{}").unwrap();
        let pairs = GoldenReport::collect_pairs(dir.path());
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].0.ends_with("A.input"));
    }

    #[test]
    fn run_golden_aggregates_and_sorts_worst_first() {
        let dir = tempdir().unwrap();
        // An exact pair (already Google-formatted).
        fs::write(dir.path().join("Exact.input"), "class A {\n  int x;\n}\n").unwrap();
        fs::write(dir.path().join("Exact.output"), "class A {\n  int x;\n}\n").unwrap();
        // A divergent pair: expect a wildly different output.
        fs::write(dir.path().join("Off.input"), "class B{int y;}").unwrap();
        fs::write(
            dir.path().join("Off.output"),
            "class B {\n\n\n  // totally different\n  int y;\n}\n",
        )
        .unwrap();

        let report = GoldenReport::run("tmp", dir.path(), &GJF, Pin::Unpinned);
        assert_eq!(report.total, 2);
        assert_eq!(report.exact, 1);
        assert!(report.mean_similarity > 0.0 && report.mean_similarity < 1.0);
        // Worst first.
        assert!(report.results[0].similarity <= report.results[1].similarity);
    }

    #[test]
    fn markdown_report_has_a_row_per_corpus() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("A.input"), "class A {}\n").unwrap();
        fs::write(dir.path().join("A.output"), "class A {}\n").unwrap();
        let report = GoldenReport::run("gjf-testdata", dir.path(), &GJF, Pin::Submodule);
        let md = GoldenReport::markdown_report(std::slice::from_ref(&report), 0);
        assert!(md.contains("| corpus |"), "missing header:\n{md}");
        assert!(md.contains("gjf-testdata"), "missing corpus row:\n{md}");
    }

    #[test]
    fn markdown_report_lists_divergent_files_in_details() {
        let dir = tempdir().unwrap();
        // One exact pair and one divergent pair.
        fs::write(dir.path().join("Exact.input"), "class A {\n  int x;\n}\n").unwrap();
        fs::write(dir.path().join("Exact.output"), "class A {\n  int x;\n}\n").unwrap();
        fs::write(dir.path().join("Off.input"), "class B {\n  int y;\n}\n").unwrap();
        fs::write(
            dir.path().join("Off.output"),
            "class B {\n  int y;\n  int z;\n}\n",
        )
        .unwrap();
        let report = GoldenReport::run("c", dir.path(), &GJF, Pin::Unpinned);
        let md = GoldenReport::markdown_report(std::slice::from_ref(&report), 20);
        assert!(md.contains("<details>"), "missing details block:\n{md}");
        assert!(md.contains("Off.input"), "missing divergent file:\n{md}");
        // The exact pair must not appear in the least-similar list.
        assert!(!md.contains("Exact.input"), "exact file listed:\n{md}");
    }

    #[test]
    fn palantir_config_has_palantir_defaults() {
        let c = Target::palantir_config();
        // Palantir doubles Google's indents and widens the column limit to 120.
        assert_eq!(c.layout.indent_width, 4);
        assert_eq!(c.layout.continuation_indent, Some(8));
        assert_eq!(c.layout.max_width, 120);
        // Unlike google-java-format, Javadoc formatting is off unless asked for, and both
        // Palantir corpora are generated with a default `JavaFormatterOptions`.
        assert!(!c.comments.format_javadoc);
    }

    #[test]
    fn every_corpus_names_a_registered_target_and_pin() {
        for source in GOLDEN_SOURCES {
            assert!(
                Target::by_name(source.target.name).is_some(),
                "{}: target `{}` is not in TARGETS",
                source.name,
                source.target.name
            );
            // A `Release` pin whose key is not in TOOL_PINS would silently report
            // `<KEY unset>` as the reference version instead of failing.
            if let Pin::Release(key) = source.pin {
                assert!(
                    TOOL_PINS.iter().any(|(k, _)| *k == key),
                    "{}: pin key `{key}` is not in TOOL_PINS",
                    source.name
                );
            }
        }
    }

    /// Read a file that lives beside this crate, by a path relative to its manifest dir.
    fn repo_file(rel: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        fs::read_to_string(&path).unwrap_or_else(|_| panic!("{rel} should exist"))
    }

    /// Every pinned reference version has to be the one CI actually downloads.
    ///
    /// The pin *is* half the metric's definition (`DESIGN.md` §7.1): a similarity number
    /// scored against 1.35.0 and one scored against 1.36.0 are different measurements. The
    /// workflow holds the version as an `env:` entry per tool, so the two spellings are
    /// checked against each other here rather than being allowed to drift.
    #[test]
    fn the_tool_pins_match_ci() {
        let ci = repo_file("../.github/workflows/ci.yml");
        for (key, version) in TOOL_PINS {
            let entry = format!("{key}: \"{version}\"");
            assert!(
                ci.contains(&entry),
                "ci.yml does not pin `{entry}` — TOOL_PINS and the workflow have drifted"
            );
        }
    }

    /// The JDT pin lives in a third place: the fetch script's resolved coordinate list.
    ///
    /// That list is what actually decides which formatter builds the corpus, so a bump that
    /// updates `TOOL_PINS` and CI but misses the script would generate against the old JDT
    /// while the report names the new one — the exact confusion the pin exists to prevent.
    #[test]
    fn the_eclipse_pin_matches_the_fetch_script() {
        let script = repo_file("scripts/fetch-eclipse-jdt.sh");
        let (_, version) = TOOL_PINS
            .iter()
            .find(|(key, _)| *key == "ECLIPSE_JDT_VERSION")
            .expect("the Eclipse pin should be registered");
        let coordinate = format!("org.eclipse.jdt:org.eclipse.jdt.core:{version}");
        assert!(
            script.contains(&coordinate),
            "fetch-eclipse-jdt.sh does not fetch `{coordinate}` — the pin has drifted"
        );
    }

    /// The two file-backed targets score with a config parsed from a committed file, so a
    /// profile that stopped parsing would surface as a silent panic mid-corpus.
    #[test]
    fn the_committed_profiles_import() {
        let eclipse = Target::eclipse_config();
        // JDT's stock profile: tabs, 4 wide, 120 columns, comments reflowed at 80.
        assert_eq!(eclipse.layout.max_width, 120);
        assert_eq!(eclipse.comments.width, 80);

        let intellij = Target::intellij_config();
        assert_eq!(intellij.layout.max_width, 120);
        assert_eq!(intellij.layout.indent_width, 4);
        assert_eq!(intellij.layout.continuation_indent, Some(8));
    }
}
