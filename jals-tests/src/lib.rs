//! Core logic for `jals-tests`: walk corpora of real Java source, parse every
//! file with `jals_syntax`, and tally the outcomes.
//!
//! The point is to exercise the parser's invariants — never panics, lossless,
//! always produces a tree — against large real-world corpora, and to surface a
//! parse-failure rate plus the offending files.
//!
//! [`golden`] is a separate harness that checks formatter *fidelity* against
//! `google-java-format` output, rather than parser soundness.

pub mod golden;

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use walkdir::WalkDir;

/// A named corpus, rooted at a path relative to the `sources/` directory.
pub struct Source {
    /// Stable identifier used on the command line.
    pub name: &'static str,
    /// Root directory, relative to the `sources/` dir.
    pub root_rel: &'static str,
    /// Human-readable description.
    pub description: &'static str,
}

/// Every corpus the CLI knows about. Add an entry here to register a new source.
pub const ALL_SOURCES: &[Source] = &[
    Source {
        name: "openjdk",
        root_rel: "openjdk",
        description: "OpenJDK (openjdk/jdk): every .java file in the repository",
    },
    Source {
        name: "langtools",
        root_rel: "openjdk/test/langtools",
        description: "OpenJDK langtools tests (includes intentionally invalid Java)",
    },
];

/// Look up a source by its command-line name.
pub fn source_by_name(name: &str) -> Option<&'static Source> {
    ALL_SOURCES.iter().find(|s| s.name == name)
}

/// The classification of a single file's parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No syntax errors and the tree round-trips to the input.
    Ok,
    /// The parser reported this many syntax errors (the tree still round-trips).
    SyntaxErrors(usize),
    /// The tree's text differs from the input: a losslessness violation.
    NonLossless,
    /// The parser panicked: a hard invariant violation.
    Panicked,
    /// The file could not be read as UTF-8 text.
    ReadError,
}

impl Outcome {
    /// Whether the file parsed cleanly.
    pub fn is_ok(self) -> bool {
        matches!(self, Outcome::Ok)
    }

    /// Whether this outcome breaks a hard parser invariant (panic / non-lossless).
    pub fn is_invariant_violation(self) -> bool {
        matches!(self, Outcome::Panicked | Outcome::NonLossless)
    }

    /// A short, stable label for display.
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::SyntaxErrors(_) => "syntax-error",
            Outcome::NonLossless => "non-lossless",
            Outcome::Panicked => "panicked",
            Outcome::ReadError => "read-error",
        }
    }
}

/// Parse a single file and classify the result.
///
/// Never panics: a panic inside the parser is caught and reported as
/// [`Outcome::Panicked`], since catching invariant violations is the whole point.
pub fn check_file(path: &Path) -> Outcome {
    let src = match std::fs::read_to_string(path) {
        Ok(src) => src,
        Err(_) => return Outcome::ReadError,
    };
    let parsed = panic::catch_unwind(AssertUnwindSafe(|| {
        let parse = jals_syntax::parse(&src);
        let lossless = parse.syntax().text().to_string() == src;
        (parse.errors().len(), lossless)
    }));
    match parsed {
        Err(_) => Outcome::Panicked,
        Ok((_, false)) => Outcome::NonLossless,
        Ok((0, true)) => Outcome::Ok,
        Ok((errors, true)) => Outcome::SyntaxErrors(errors),
    }
}

/// Re-parse `path` and format its first syntax error, for `--show-errors`.
///
/// Best-effort: returns `None` if the file is unreadable, panics, or has no errors.
pub fn first_error(path: &Path) -> Option<String> {
    let src = std::fs::read_to_string(path).ok()?;
    panic::catch_unwind(AssertUnwindSafe(|| {
        jals_syntax::parse(&src)
            .errors()
            .first()
            .map(|e| format!("{} @ {:?}", e.message(), e.range()))
    }))
    .ok()
    .flatten()
}

/// Recursively collect every `.java` file under `root`.
pub fn collect_java_files(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|path| path.extension().is_some_and(|ext| ext == "java"))
        .collect()
}

/// Aggregated outcomes for one source.
#[derive(Debug, Clone)]
pub struct SourceReport {
    /// Source name.
    pub name: String,
    /// Resolved root directory that was walked.
    pub root: PathBuf,
    /// Total `.java` files found.
    pub total: usize,
    /// Files that parsed cleanly.
    pub ok: usize,
    /// Files with one or more syntax errors.
    pub syntax_errors: usize,
    /// Files whose tree did not round-trip (invariant violation).
    pub non_lossless: usize,
    /// Files that made the parser panic (invariant violation).
    pub panicked: usize,
    /// Files that could not be read as UTF-8.
    pub read_errors: usize,
    /// Every non-`Ok` file, in walk order.
    pub failures: Vec<(PathBuf, Outcome)>,
}

impl SourceReport {
    /// Fraction of files (0.0–1.0) with at least one syntax error.
    pub fn syntax_error_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.syntax_errors as f64 / self.total as f64
        }
    }

    /// Whether any hard invariant (non-lossless / panic) was violated.
    pub fn has_invariant_violations(&self) -> bool {
        self.panicked > 0 || self.non_lossless > 0
    }
}

/// Walk `root`, parse every `.java` file in parallel, and aggregate the outcomes.
pub fn run_source(name: &str, root: &Path) -> SourceReport {
    let outcomes: Vec<(PathBuf, Outcome)> = collect_java_files(root)
        .into_par_iter()
        .map(|path| {
            let outcome = check_file(&path);
            (path, outcome)
        })
        .collect();

    let mut report = SourceReport {
        name: name.to_string(),
        root: root.to_path_buf(),
        total: outcomes.len(),
        ok: 0,
        syntax_errors: 0,
        non_lossless: 0,
        panicked: 0,
        read_errors: 0,
        failures: Vec::new(),
    };
    for (path, outcome) in outcomes {
        match outcome {
            Outcome::Ok => report.ok += 1,
            Outcome::SyntaxErrors(_) => report.syntax_errors += 1,
            Outcome::NonLossless => report.non_lossless += 1,
            Outcome::Panicked => report.panicked += 1,
            Outcome::ReadError => report.read_errors += 1,
        }
        if !outcome.is_ok() {
            report.failures.push((path, outcome));
        }
    }
    report
}

/// Render the reports as a GitHub-flavored Markdown summary: a parse-rate table,
/// suitable for a CI step summary or a pull-request comment.
pub fn markdown_report(reports: &[SourceReport]) -> String {
    let mut out = String::from("## jals parse soundness\n\n");
    out.push_str("Parse rate of `jals_syntax::parse` over real Java corpora.\n\n");
    out.push_str(
        "| source | files | ok | parse rate | syntax errors | non-lossless | panicked |\n",
    );
    out.push_str("| --- | --: | --: | --: | --: | --: | --: |\n");
    for r in reports {
        let rate = if r.total == 0 {
            0.0
        } else {
            r.ok as f64 * 100.0 / r.total as f64
        };
        out.push_str(&format!(
            "| {} | {} | {} | {:.2}% | {} | {} | {} |\n",
            r.name, r.total, r.ok, rate, r.syntax_errors, r.non_lossless, r.panicked
        ));
    }
    if reports.iter().any(SourceReport::has_invariant_violations) {
        out.push_str("\n⚠️ **Invariant violation**: `non-lossless` and `panicked` must be 0.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn check_file_classifies_outcomes() {
        let dir = tempdir().unwrap();

        let good = dir.path().join("Good.java");
        fs::write(&good, "class Good { void m() {} }").unwrap();
        assert_eq!(check_file(&good), Outcome::Ok);

        // Unclosed class body — the parser recovers but records a syntax error.
        let bad = dir.path().join("Bad.java");
        fs::write(&bad, "class Bad {").unwrap();
        assert!(
            matches!(check_file(&bad), Outcome::SyntaxErrors(_)),
            "expected syntax errors, got {:?}",
            check_file(&bad)
        );

        let missing = dir.path().join("Missing.java");
        assert_eq!(check_file(&missing), Outcome::ReadError);
    }

    #[test]
    fn run_source_walks_recursively_and_tallies() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("A.java"), "class A {}").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("B.java"), "class B {}").unwrap();
        fs::write(dir.path().join("sub").join("Bad.java"), "class C {").unwrap();
        // Non-.java files are ignored.
        fs::write(dir.path().join("notes.txt"), "class D {}").unwrap();

        let report = run_source("tmp", dir.path());
        assert_eq!(report.total, 3);
        assert_eq!(report.ok, 2);
        assert_eq!(report.syntax_errors, 1);
        assert_eq!(report.non_lossless, 0);
        assert_eq!(report.panicked, 0);
        assert_eq!(report.failures.len(), 1);
        assert!(!report.has_invariant_violations());
    }

    #[test]
    fn markdown_report_has_a_row_per_source() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("A.java"), "class A {}").unwrap();
        let report = run_source("openjdk", dir.path());
        let md = markdown_report(std::slice::from_ref(&report));
        assert!(md.contains("| source |"), "missing header:\n{md}");
        assert!(md.contains("openjdk"), "missing source row:\n{md}");
        assert!(md.contains("100.00%"), "missing parse rate:\n{md}");
    }
}
