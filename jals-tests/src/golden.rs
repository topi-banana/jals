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
//! jals cannot byte-match google-java-format on line wrapping yet — it has no
//! separate continuation indent and uses a different (Wadler/Prettier) wrapping
//! algorithm — so this reports a **similarity** metric (the mean line-level diff
//! ratio, plus the count of exact matches) to track convergence as formatter
//! options are added, rather than a hard pass/fail.

use std::path::{Path, PathBuf};

use jals_fmt::{
    AnnotationPlacement, BinopLayout, BinopSeparator, BraceStyle, ClosingParen, Config,
    ControlBraceStyle, IndentStyle, LineEnding,
};
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
    pub description: &'static str,
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

/// Look up a golden source by its command-line name.
pub fn golden_source_by_name(name: &str) -> Option<&'static GoldenSource> {
    GOLDEN_SOURCES.iter().find(|s| s.name == name)
}

/// A best-effort Google Java Style [`Config`], expressed with the options jals has
/// today.
///
/// This deliberately lives in the test crate, not in `jals-fmt`, until a first-class
/// `Config::google()` preset exists. The **continuation indent** — Google style indents
/// wrapped continuation lines by +4 columns while the block indent is +2 — is now
/// modeled with the dedicated `continuation-indent` option (see below); it had been the
/// single largest gap in the similarity metric this harness reports.
pub fn google_config() -> Config {
    Config {
        // Block indentation: Google style is 2 spaces.
        indent_style: IndentStyle::Space,
        indent_width: 2,
        // Continuation indentation: Google style indents wrapped lines by +4 columns
        // (double the +2 block indent).
        continuation_indent: Some(4),
        // Column limit and blank-line policy.
        max_width: 100,
        max_blank_lines: 1,
        // google-java-format preserves a blank line at the start of a braced body (right after
        // `{`), e.g. a class body or a control-flow block, instead of dropping it.
        blank_line_at_block_start: true,
        line_ending: LineEnding::Lf,
        insert_final_newline: true,
        // K&R / "Egyptian" braces for both declarations and control flow.
        brace_style: BraceStyle::SameLine,
        control_brace_style: ControlBraceStyle::SameLine,
        // Imports: a static group, a blank line, then a non-static group, each sorted.
        group_imports: true,
        import_groups: vec!["static".to_string(), "*".to_string()],
        // Modifiers in canonical JLS order; annotations on their own lines.
        reorder_modifiers: true,
        annotation_placement: AnnotationPlacement::Expanded,
        // Break before binary operators, packing as many operands per line as fit (fill).
        binop_separator: BinopSeparator::Front,
        binop_layout: BinopLayout::Compressed,
        // Google style has no fixed per-construct width heuristics — it wraps purely
        // against the 100-column limit, so push every threshold up to the column limit.
        chain_width: 100,
        fn_call_width: 100,
        array_width: 100,
        single_line_if_else_max_width: 100,
        // google-java-format normalizes parameter-name block comments (`/*a=*/` → `/* a= */`)
        // and hugs them to the following argument.
        normalize_parameter_comments: true,
        // google-java-format keeps a block comment written immediately before a token on the same
        // line (e.g. `java.lang./* @A */ String`) hugging that token instead of flushing it to end
        // of line.
        inline_block_comments: true,
        // google-java-format never puts the closing `)` of a paren-delimited list (call /
        // annotation args, parameters, record header) on its own line — it hugs the last item.
        closing_paren: ClosingParen::Hug,
        // google-java-format preserves the source row breaks of a tabular (grid-shaped) array
        // initializer instead of reflowing it by width.
        tabular_array_initializers: true,
        // google-java-format puts a `switch` expression that is the value of a `=` (a variable /
        // field initializer or an assignment) on its own continuation-indented line.
        switch_expression_on_new_line: true,
        // google-java-format wraps a `case` label's constant list across lines when the arm
        // overflows the column limit (e.g. `ExpressionSwitch`'s `breakLongCaseArgs`).
        wrap_case_labels: true,
        // google-java-format surrounds an operator colon — an enhanced `for` (`for (T x : xs)`),
        // a ternary (`a ? b : c`), and an `assert` message (`assert c : m`) — with spaces, while
        // still hugging the colon of an unnamed `_` for-each variable (`for (T _: xs)`) and of
        // label / `case` colons (`label:`, `case x:`).
        space_around_operator_colon: true,
        ..Config::default()
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
    pub exact: bool,
}

/// Format `input` with `cfg` and score it against the expected `expected` output:
/// a line-level similarity ratio plus whether the two are byte-identical.
pub fn score(input: &str, expected: &str, cfg: &Config) -> (f64, bool) {
    let formatted = jals_fmt::format_source(input, cfg).formatted;
    let exact = formatted == expected;
    let ratio = TextDiff::from_lines(expected, &formatted).ratio() as f64;
    (ratio, exact)
}

/// Recursively collect every `*.input` under `root` that has a sibling `*.output`.
pub fn collect_pairs(root: &Path) -> Vec<(PathBuf, PathBuf)> {
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
}

/// Walk `root`, format every `.input` with `cfg` in parallel, and aggregate the
/// similarity of each result to its `.output`.
pub fn run_golden(name: &str, root: &Path, cfg: &Config) -> GoldenReport {
    let mut results: Vec<PairResult> = collect_pairs(root)
        .into_par_iter()
        .filter_map(|(input_path, output_path)| {
            let input = std::fs::read_to_string(&input_path).ok()?;
            let expected = std::fs::read_to_string(&output_path).ok()?;
            let (similarity, exact) = score(&input, &expected, cfg);
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

    GoldenReport {
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
pub fn markdown_report(reports: &[GoldenReport], worst: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn google_config_has_google_defaults() {
        let c = google_config();
        assert_eq!(c.indent_width, 2);
        // Google style wraps continuation lines at +4 columns (double the +2 block indent).
        assert_eq!(c.continuation_indent, Some(4));
        assert_eq!(c.max_width, 100);
        assert!(c.group_imports);
        assert_eq!(c.import_groups, ["static", "*"]);
        assert!(c.reorder_modifiers);
        assert_eq!(c.annotation_placement, AnnotationPlacement::Expanded);
        assert!(c.normalize_parameter_comments);
        assert!(c.inline_block_comments);
        assert!(c.tabular_array_initializers);
        assert!(c.switch_expression_on_new_line);
        assert!(c.wrap_case_labels);
        // google-java-format spaces the operator colon (enhanced-`for` / ternary / `assert`).
        assert!(c.space_around_operator_colon);
        // google-java-format breaks and indents a legacy (colon-form) switch's case bodies; this
        // is jals's default, so `google_config` inherits it.
        assert_eq!(c.switch_case_body, jals_fmt::SwitchCaseBody::Always);
    }

    #[test]
    fn score_is_one_for_already_formatted_input() {
        // A trivially-formatted class, in Google's 2-space style, is a fixed point.
        let cfg = google_config();
        let expected = "class A {\n  void m() {}\n}\n";
        let (similarity, exact) = score(expected, expected, &cfg);
        assert!(exact, "expected an exact match for already-formatted input");
        assert_eq!(similarity, 1.0);
    }

    #[test]
    fn score_rewards_closeness() {
        let cfg = google_config();
        // The formatted input matches the expected output except for one extra line:
        // not exact, but highly similar. (Independent of any wrapping behavior — the
        // input is already in Google's 2-space style, so jals reproduces it verbatim.)
        let input = "class A {\n  int x;\n}\n";
        let expected = "class A {\n  int x;\n  int y;\n}\n";
        let (similarity, exact) = score(input, expected, &cfg);
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
        let pairs = collect_pairs(dir.path());
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

        let report = run_golden("tmp", dir.path(), &google_config());
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
        let report = run_golden("gjf-testdata", dir.path(), &google_config());
        let md = markdown_report(std::slice::from_ref(&report), 0);
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
        let report = run_golden("c", dir.path(), &google_config());
        let md = markdown_report(std::slice::from_ref(&report), 20);
        assert!(md.contains("<details>"), "missing details block:\n{md}");
        assert!(md.contains("Off.input"), "missing divergent file:\n{md}");
        // The exact pair must not appear in the least-similar list.
        assert!(!md.contains("Exact.input"), "exact file listed:\n{md}");
    }
}
