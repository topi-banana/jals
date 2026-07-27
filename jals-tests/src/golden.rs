//! Golden-corpus *formatter* verification.
//!
//! The crate root ([`crate`]) checks parser invariants (never panics, lossless,
//! always a tree). This module checks something different: how close `jals-fmt`'s
//! output, driven by a Google Java Style [`Config`], comes to the output of
//! `google-java-format` itself.
//!
//! A corpus is a directory tree of `*.input` / `*.output` pairs (the same naming
//! google-java-format uses for its own regression suite): `Foo.input` is the
//! unformatted source and `Foo.output` is what google-java-format produces from it.
//! We format each `.input` and compare the result against the paired `.output`.
//!
//! `jals-fmt` runs a port of google-java-format's own greedy `computeBreaks`, so
//! byte-matching it is the stated goal for the `gjf` profile (`DESIGN.md` §18.1's
//! tier T1) rather than an impossibility. It is not reached yet, and a byte-equal
//! rate alone would hide progress — one space of difference sinks a whole file —
//! so this reports a **similarity** metric (the mean line-level diff ratio, plus
//! the count of exact matches) to track convergence, rather than a hard
//! pass/fail.

use std::path::{Path, PathBuf};

use jals_config::fmt::Config;
use rayon::prelude::*;
use similar::TextDiff;
use walkdir::WalkDir;

/// A named golden corpus, rooted at a path relative to the `sources/` directory.
pub struct GoldenSource {
    /// Stable identifier used on the command line.
    pub name: &'static str,
    /// Root directory, relative to the `sources/` dir.
    pub root_rel: &'static str,
    /// Human-readable description.
    #[allow(dead_code)]
    description: &'static str,
}

/// Every golden corpus the CLI knows about. Add an entry here to register a new one.
pub const GOLDEN_SOURCES: &[GoldenSource] = &[
    GoldenSource {
        name: "gjf-testdata",
        root_rel: "google-java-format/core/src/test/resources/com/google/googlejavaformat/java/testdata",
        description: "google-java-format's own .input/.output regression corpus (Apache-2.0)",
    },
    GoldenSource {
        name: "openjdk-gjf",
        root_rel: "openjdk-gjf",
        description: "OpenJDK src/ library sources formatted with google-java-format (generated; see scripts/gen-openjdk-gjf.sh)",
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

    /// The Google Java Style [`Config`], as the google-java-format importer produces it.
    ///
    /// This used to be a hand-written `Config` literal that restated the same style twice —
    /// once here and once in `jals_fmt::import::gjf` — and drifted from it. The importer's
    /// family profile is now the single definition; see `jals-fmt/MAPPING.md` §5 for what each
    /// value is anchored to.
    #[must_use]
    pub fn google_config() -> Config {
        jals_fmt::import::GoogleJavaFormatConfig::default().into()
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

    /// Walk `root`, format every `.input` with `cfg` in parallel, and aggregate the
    /// similarity of each result to its `.output`.
    pub fn run(name: &str, root: &Path, cfg: &Config) -> Self {
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
        let mut out = String::from("## jals-fmt vs google-java-format\n\n");
        out.push_str(
            "Similarity of `jals-fmt` (Google-style config) output to `google-java-format`.\n\n",
        );
        out.push_str("| corpus | pairs | exact | exact rate | mean similarity |\n");
        out.push_str("| --- | --: | --: | --: | --: |\n");
        for r in reports {
            out.push_str(&format!(
                "| {} | {} | {} | {:.2}% | {:.2}% |\n",
                r.name,
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
        let c = GoldenReport::google_config();
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
        assert_eq!(from_file, GoldenReport::google_config());
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
        let cfg = GoldenReport::google_config();
        let expected = "class A {\n  void m() {}\n}\n";
        let (similarity, exact) = PairResult::score(expected, expected, &cfg);
        assert!(exact, "expected an exact match for already-formatted input");
        assert_eq!(similarity, 1.0);
    }

    #[test]
    fn score_rewards_closeness() {
        let cfg = GoldenReport::google_config();
        // The formatted input matches the expected output except for one extra line:
        // not exact, but highly similar. (Independent of any wrapping behavior — the
        // input is already in Google's 2-space style, so jals reproduces it verbatim.)
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

        let report = GoldenReport::run("tmp", dir.path(), &GoldenReport::google_config());
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
        let report = GoldenReport::run("gjf-testdata", dir.path(), &GoldenReport::google_config());
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
        let report = GoldenReport::run("c", dir.path(), &GoldenReport::google_config());
        let md = GoldenReport::markdown_report(std::slice::from_ref(&report), 20);
        assert!(md.contains("<details>"), "missing details block:\n{md}");
        assert!(md.contains("Off.input"), "missing divergent file:\n{md}");
        // The exact pair must not appear in the least-similar list.
        assert!(!md.contains("Exact.input"), "exact file listed:\n{md}");
    }
}
