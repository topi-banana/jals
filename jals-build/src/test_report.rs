//! Reading what an external test target said happened.
//!
//! A generated harness reports one test per process, and a pass is the sentinel line that process
//! printed. A target that boots once and runs everything inside itself cannot do that: there is one
//! exit status for the whole run, and it says nothing about which of forty tests passed. So the
//! program writes a **report**, and this reads it.
//!
//! **The format is one record per line, tab-separated**, and that is a deliberate choice against
//! the two obvious alternatives. A JUnit-style XML report would need an XML parser, and the only
//! one in the workspace is behind `jals-fmt`'s `std` feature; JSON would need `serde_json`, which
//! this crate takes only under `rhai`. A line protocol needs neither, parses in a few dozen lines,
//! and is the shape this crate already reads twice — [`TestCase::parse`](crate::TestCase) reads the
//! harness's `--list` output, and its pass sentinel is a line. A person can also write one by
//! hand while bringing a target up, which an XML report does not offer.
//!
//! Written out with `→` standing in for the tab that actually separates the fields:
//!
//! ```text
//! com.example.e2e.TitleScreen#renders → ok
//! com.example.e2e.TitleScreen#renders → shot → title_screen → screenshots/title_screen.png
//! com.example.e2e.Command#suggests    → fail → expected 3 suggestions, saw 0
//! com.example.e2e.Command#history     → skip → needs a server
//! ```
//!
//! The first field is the test id, the second the verb. `ok` takes nothing further, `fail` and
//! `skip` take one reason, `shot` takes a name and a path, and `time` takes a whole number of
//! milliseconds. `time` is optional — a target that does not measure simply omits it, and the
//! reported duration is then zero rather than invented.
//!
//! **Nothing here decides a verdict.** A `shot` line names a file; whether that file matches its
//! reference image is the screenshot comparison's answer, not the program's. Keeping those apart is
//! what lets the golden images live outside the program that produced the screenshots.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::time::Duration;

/// The separator between a test's declaring class and its method, in the id the report and the
/// runner both spell. The same separator [`TestCase`](crate::TestCase) requires, because an
/// external target's ids travel through the same filters.
const ID_SEPARATOR: char = '#';

/// What a program said about one test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportedVerdict {
    /// The test ran and passed.
    Passed,
    /// The test ran and failed, with the program's own explanation.
    Failed(String),
    /// The program declined to run it, with its reason.
    Skipped(String),
}

/// One screenshot a test produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shot {
    /// The name it is compared under — the golden archive's member name, without an extension.
    pub name: String,
    /// Where the program wrote it, relative to the run directory.
    pub path: String,
}

/// Everything the report said about one test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportEntry {
    pub id: String,
    pub verdict: ReportedVerdict,
    /// The screenshots this test produced, in the order the report named them.
    pub shots: Vec<Shot>,
    /// How long the program says the test took, when it says.
    ///
    /// Optional because only the program can know: a target runs every test inside one process, so
    /// there is no per-test wall clock on this side to fall back to. Absent is reported as zero,
    /// never as a share of the run.
    pub duration: Option<Duration>,
}

/// A line the report should not have contained.
///
/// Collected rather than raised, because a report is most interesting exactly when the run went
/// wrong: a program that crashed halfway leaves a truncated file, and the tests it *did* report
/// are still the most useful thing in it. A host reports these alongside the entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportProblem {
    /// A line that is not blank and is not a record.
    Malformed {
        /// The 1-based line number, as a reader counts them.
        line: usize,
        /// What the line said, so the message can quote it.
        text: String,
    },
    /// Two verdict lines for one test.
    DuplicateVerdict { line: usize, id: String },
    /// A `shot` line for a test the report never gave a verdict for.
    ShotWithoutVerdict { id: String },
}

impl fmt::Display for ReportProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { line, text } => {
                write!(f, "line {line} is not a report record: `{text}`")
            }
            Self::DuplicateVerdict { line, id } => {
                write!(f, "line {line} reports `{id}` a second time")
            }
            Self::ShotWithoutVerdict { id } => write!(
                f,
                "the report names a screenshot for `{id}` but never says whether it passed"
            ),
        }
    }
}

impl core::error::Error for ReportProblem {}

/// A parsed report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestReport {
    entries: Vec<ReportEntry>,
    problems: Vec<ReportProblem>,
}

impl TestReport {
    /// Parse a report's text.
    ///
    /// Entries come back in the order the report first named each test, which is the order the
    /// program ran them in — not the order the runner selected them, because a target that
    /// reorders its own work is allowed to.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut report = Self::default();
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let record = raw.trim_end_matches(['\r', '\n']);
            if record.trim().is_empty() {
                continue;
            }
            report.record(line, record);
        }
        // A `shot` line creates an entry so the report stays order-independent; one that never
        // received a verdict is the program forgetting to say what happened, and is reported.
        let missing: Vec<String> = report
            .entries
            .iter()
            .filter(|entry| matches!(entry.verdict, ReportedVerdict::Skipped(ref why) if why == MISSING_VERDICT))
            .map(|entry| entry.id.clone())
            .collect();
        for id in missing {
            report
                .problems
                .push(ReportProblem::ShotWithoutVerdict { id });
        }
        report
    }

    /// Read one line into the report.
    fn record(&mut self, line: usize, record: &str) {
        let mut fields = record.split('\t');
        let Some(id) = fields.next() else {
            return;
        };
        if id.is_empty() || !id.contains(ID_SEPARATOR) {
            self.malformed(line, record);
            return;
        }
        match fields.next() {
            Some("ok") => self.verdict(line, id, ReportedVerdict::Passed),
            Some("fail") => {
                let why = fields.next().unwrap_or_default().to_owned();
                self.verdict(line, id, ReportedVerdict::Failed(why));
            }
            Some("skip") => {
                let why = fields.next().unwrap_or_default().to_owned();
                self.verdict(line, id, ReportedVerdict::Skipped(why));
            }
            Some("time") => {
                let Some(millis) = fields.next().and_then(|f| f.parse::<u64>().ok()) else {
                    self.malformed(line, record);
                    return;
                };
                let at = self.entry(id);
                self.entries[at].duration = Some(Duration::from_millis(millis));
            }
            Some("shot") => {
                let (Some(name), Some(path)) = (fields.next(), fields.next()) else {
                    self.malformed(line, record);
                    return;
                };
                if name.is_empty() || path.is_empty() {
                    self.malformed(line, record);
                    return;
                }
                let at = self.entry(id);
                self.entries[at].shots.push(Shot {
                    name: name.to_owned(),
                    path: path.to_owned(),
                });
            }
            // An unrecognized verb is ignored rather than refused, for the reason `TestCase::parse`
            // gives: the report is written by a program compiled from this same project, so a verb
            // this build does not know cannot arrive from a version skew — and treating it as fatal
            // would turn a future addition into a hard failure for no gain.
            Some(_) => {}
            None => self.malformed(line, record),
        }
    }

    /// Record a verdict, or a duplicate if one is already there.
    fn verdict(&mut self, line: usize, id: &str, verdict: ReportedVerdict) {
        let at = self.entry(id);
        if self.entries[at].verdict == ReportedVerdict::Skipped(MISSING_VERDICT.to_owned()) {
            self.entries[at].verdict = verdict;
            return;
        }
        self.problems.push(ReportProblem::DuplicateVerdict {
            line,
            id: id.to_owned(),
        });
    }

    /// The index of `id`'s entry, creating it if this is the first line naming it.
    fn entry(&mut self, id: &str) -> usize {
        if let Some(at) = self.entries.iter().position(|entry| entry.id == id) {
            return at;
        }
        self.entries.push(ReportEntry {
            id: id.to_owned(),
            // Replaced by the first verdict line. A sentinel rather than an `Option` so the
            // entry is always a complete value; the parse tail turns any left over into a problem.
            verdict: ReportedVerdict::Skipped(MISSING_VERDICT.to_owned()),
            shots: Vec::new(),
            duration: None,
        });
        self.entries.len() - 1
    }

    fn malformed(&mut self, line: usize, text: &str) {
        self.problems.push(ReportProblem::Malformed {
            line,
            text: text.to_owned(),
        });
    }

    /// Every test the report named, in first-mention order.
    #[must_use]
    pub fn entries(&self) -> &[ReportEntry] {
        &self.entries
    }

    /// What the report got wrong.
    #[must_use]
    pub fn problems(&self) -> &[ReportProblem] {
        &self.problems
    }

    /// The entry for `id`, if the report named it.
    #[must_use]
    pub fn entry_for(&self, id: &str) -> Option<&ReportEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

/// The placeholder reason an entry carries between being created by a `shot` line and receiving its
/// verdict. Distinctive enough that a program writing `skip` with this exact text — which would be
/// indistinguishable — is not something anyone writes by accident.
const MISSING_VERDICT: &str = "\u{0}no verdict";

#[cfg(test)]
mod tests {
    use super::{ReportProblem, ReportedVerdict, TestReport};
    use alloc::borrow::ToOwned;

    #[test]
    fn a_report_reads_verdicts_and_shots_in_first_mention_order() {
        let report = TestReport::parse(concat!(
            "com.example.A#one\tok\n",
            "com.example.A#one\tshot\ttitle\tscreenshots/title.png\n",
            "com.example.B#two\tfail\texpected 3 suggestions, saw 0\n",
            "com.example.C#three\tskip\tneeds a server\n",
        ));
        assert!(report.problems().is_empty(), "{:?}", report.problems());
        let ids: alloc::vec::Vec<_> = report.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "com.example.A#one",
                "com.example.B#two",
                "com.example.C#three"
            ]
        );

        let first = report.entry_for("com.example.A#one").expect("named");
        assert_eq!(first.verdict, ReportedVerdict::Passed);
        assert_eq!(first.shots.len(), 1);
        assert_eq!(first.shots[0].name, "title");
        assert_eq!(first.shots[0].path, "screenshots/title.png");

        assert_eq!(
            report
                .entry_for("com.example.B#two")
                .expect("named")
                .verdict,
            ReportedVerdict::Failed("expected 3 suggestions, saw 0".to_owned())
        );
        assert_eq!(
            report
                .entry_for("com.example.C#three")
                .expect("named")
                .verdict,
            ReportedVerdict::Skipped("needs a server".to_owned())
        );
    }

    #[test]
    fn a_shot_may_precede_its_verdict() {
        let report = TestReport::parse(concat!(
            "com.example.A#one\tshot\ttitle\tshots/title.png\n",
            "com.example.A#one\tok\n",
        ));
        assert!(report.problems().is_empty());
        let entry = report.entry_for("com.example.A#one").expect("named");
        assert_eq!(entry.verdict, ReportedVerdict::Passed);
        assert_eq!(entry.shots.len(), 1);
    }

    #[test]
    fn a_shot_with_no_verdict_is_reported_rather_than_assumed() {
        let report = TestReport::parse("com.example.A#one\tshot\ttitle\tshots/title.png\n");
        assert_eq!(
            report.problems(),
            [ReportProblem::ShotWithoutVerdict {
                id: "com.example.A#one".to_owned()
            }]
        );
    }

    #[test]
    fn a_second_verdict_for_one_test_is_a_problem_and_the_first_one_stands() {
        let report = TestReport::parse(concat!(
            "com.example.A#one\tok\n",
            "com.example.A#one\tfail\tand again\n",
        ));
        assert_eq!(
            report
                .entry_for("com.example.A#one")
                .expect("named")
                .verdict,
            ReportedVerdict::Passed
        );
        assert!(matches!(
            report.problems(),
            [ReportProblem::DuplicateVerdict { line: 2, .. }]
        ));
    }

    #[test]
    fn a_truncated_report_still_yields_what_it_did_say() {
        // A run that crashed after two tests: the third line is half-written.
        let report = TestReport::parse(concat!(
            "com.example.A#one\tok\n",
            "com.example.B#two\tok\n",
            "com.example.C#unfinis",
        ));
        assert_eq!(report.entries().len(), 2);
        assert!(matches!(
            report.problems(),
            [ReportProblem::Malformed { line: 3, .. }]
        ));
    }

    #[test]
    fn blank_lines_and_carriage_returns_are_not_records() {
        let report = TestReport::parse("com.example.A#one\tok\r\n\n   \n");
        assert!(report.problems().is_empty());
        assert_eq!(report.entries().len(), 1);
        assert_eq!(
            report
                .entry_for("com.example.A#one")
                .expect("named")
                .verdict,
            ReportedVerdict::Passed
        );
    }

    #[test]
    fn a_line_whose_id_is_not_an_id_is_malformed() {
        let report = TestReport::parse("not-an-id\tok\nalso not one\n");
        assert_eq!(report.entries().len(), 0);
        assert_eq!(report.problems().len(), 2);
    }

    #[test]
    fn an_unknown_verb_is_ignored_rather_than_refused() {
        let report = TestReport::parse(concat!(
            "com.example.A#one\tok\n",
            "com.example.A#one\tmeasured\t42ms\n",
        ));
        assert!(report.problems().is_empty());
        assert_eq!(report.entries().len(), 1);
    }

    #[test]
    fn a_shot_missing_a_field_is_malformed_rather_than_half_recorded() {
        let report = TestReport::parse(concat!(
            "com.example.A#one\tok\n",
            "com.example.A#one\tshot\ttitle\n",
            "com.example.A#one\tshot\t\tshots/x.png\n",
        ));
        assert_eq!(
            report
                .entry_for("com.example.A#one")
                .expect("named")
                .shots
                .len(),
            0
        );
        assert_eq!(report.problems().len(), 2);
    }

    #[test]
    fn a_time_line_is_read_and_a_missing_one_is_not_invented() {
        let report = TestReport::parse(concat!(
            "com.example.A#one\tok\n",
            "com.example.A#one\ttime\t1234\n",
            "com.example.B#two\tok\n",
        ));
        assert!(report.problems().is_empty());
        assert_eq!(
            report
                .entry_for("com.example.A#one")
                .expect("named")
                .duration,
            Some(core::time::Duration::from_millis(1234))
        );
        assert_eq!(
            report
                .entry_for("com.example.B#two")
                .expect("named")
                .duration,
            None
        );
    }

    #[test]
    fn a_time_that_is_not_a_number_is_malformed() {
        let report = TestReport::parse(concat!(
            "com.example.A#one\tok\n",
            "com.example.A#one\ttime\tsoon\n",
        ));
        assert_eq!(report.problems().len(), 1);
        assert_eq!(
            report
                .entry_for("com.example.A#one")
                .expect("named")
                .duration,
            None
        );
    }

    #[test]
    fn a_fail_with_no_message_still_fails() {
        let report = TestReport::parse("com.example.A#one\tfail\n");
        assert_eq!(
            report
                .entry_for("com.example.A#one")
                .expect("named")
                .verdict,
            ReportedVerdict::Failed(alloc::string::String::new())
        );
    }
}
