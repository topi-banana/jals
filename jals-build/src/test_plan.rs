//! Which tests a run executes, and in what order — decided before a single JVM starts.
//!
//! Pure planning over a list the harness reported, so the whole selection is testable without a
//! JDK: filters, `#[ignore]` handling, and `--partition` are arithmetic over strings. What the
//! host owns is running the result.
//!
//! The list arrives already sorted, and every step here preserves that order, because the order
//! tests are *reported* in is a promise (`jals test --list` must not shuffle between runs) even
//! though the order they *complete* in is not.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// The separator between a test's declaring class and its method, in the id both the harness and
/// the CLI spell.
const ID_SEPARATOR: char = '#';

/// One test the compiled harness reported.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TestCase {
    /// `com.example.MathTest#adds`.
    id: String,
    /// Declared `#[ignore]`: listed, and run only when asked for.
    ignore: bool,
    /// Declared `#[should_fail]`. The harness already inverts the verdict, so this is carried for
    /// reporting alone.
    should_fail: bool,
}

impl TestCase {
    /// Parse one line of the harness's `--list` output: `<id>` TAB `<flags>`, where `<flags>` is a
    /// possibly empty comma-separated set.
    ///
    /// An unrecognized flag is ignored rather than refused: the harness and this parser are
    /// generated and compiled from the same tree, so a flag reaching here that this build does not
    /// know is a jals version skew inside one project — impossible — and treating it as fatal
    /// would turn a future addition into a hard failure for no gain.
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches(['\r', '\n']);
        let (id, flags) = line.split_once('\t').unwrap_or((line, ""));
        if id.is_empty() || !id.contains(ID_SEPARATOR) {
            return None;
        }
        let mut case = Self {
            id: id.to_owned(),
            ignore: false,
            should_fail: false,
        };
        for flag in flags.split(',') {
            match flag.trim() {
                "ignore" => case.ignore = true,
                "should_fail" => case.should_fail = true,
                _ => {}
            }
        }
        Some(case)
    }

    /// The full id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The declaring class.
    pub fn class(&self) -> &str {
        self.id
            .split_once(ID_SEPARATOR)
            .map_or("", |(class, _)| class)
    }

    /// The method name.
    pub fn method(&self) -> &str {
        self.id
            .split_once(ID_SEPARATOR)
            .map_or(self.id.as_str(), |(_, method)| method)
    }

    /// Whether the test declared `#[ignore]`.
    pub const fn is_ignored(&self) -> bool {
        self.ignore
    }

    /// Whether the test declared `#[should_fail]`.
    pub const fn should_fail(&self) -> bool {
        self.should_fail
    }
}

/// What to do with the tests that declared `#[ignore]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunIgnored {
    /// Run the tests that are not ignored.
    #[default]
    Default,
    /// Run *only* the ignored ones — the way to check on a test that was parked.
    IgnoredOnly,
    /// Run everything.
    All,
}

/// One shard of a test run, for splitting a suite across machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    kind: PartitionKind,
    /// 1-based shard index.
    index: u64,
    /// Shard count.
    total: u64,
}

/// How a partition assigns a test to a shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartitionKind {
    /// Round-robin over the selected order: shard `m` takes positions `m-1, m-1+n, …`.
    ///
    /// Balanced by count, and stable only for a stable *selection* — adding a test shifts what
    /// follows it. That is the trade against `hash`, and it is the one that keeps shard sizes
    /// even.
    Count,
    /// By a hash of the test id: a test stays on its shard as the suite grows, at the cost of
    /// shards that are only approximately equal.
    Hash,
}

impl Partition {
    /// Parse `count:M/N` or `hash:M/N`.
    ///
    /// # Errors
    /// [`PartitionError`] when the shape, the kind, or the numbers are wrong. `M` outside
    /// `1..=N` is rejected rather than clamped: a CI matrix that mis-numbers a shard would
    /// otherwise run one twice and another never, and report success.
    pub fn parse(spec: &str) -> Result<Self, PartitionError> {
        let (kind, rest) = spec.split_once(':').ok_or(PartitionError::Shape)?;
        let kind = match kind {
            "count" => PartitionKind::Count,
            "hash" => PartitionKind::Hash,
            _ => return Err(PartitionError::Kind),
        };
        let (index, total) = rest.split_once('/').ok_or(PartitionError::Shape)?;
        let index: u64 = index.parse().map_err(|_| PartitionError::Shape)?;
        let total: u64 = total.parse().map_err(|_| PartitionError::Shape)?;
        if total == 0 || index == 0 || index > total {
            return Err(PartitionError::Bounds);
        }
        Ok(Self { kind, index, total })
    }

    /// Whether the test at `position` in the selected order belongs to this shard.
    fn holds(self, position: usize, id: &str) -> bool {
        let slot = match self.kind {
            PartitionKind::Count => position as u64 % self.total,
            PartitionKind::Hash => Self::hash(id) % self.total,
        };
        slot == self.index - 1
    }

    /// FNV-1a over the id.
    ///
    /// Written out rather than taken from a hasher trait so the assignment is **stable across
    /// releases and platforms**: a shard that moved between jals versions would silently skip a
    /// test in a CI matrix mid-upgrade, with every shard still reporting success.
    fn hash(id: &str) -> u64 {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = OFFSET;
        for byte in id.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash
    }
}

/// A `--partition` value that names no shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionError {
    /// Not `<kind>:<m>/<n>`.
    Shape,
    /// The kind is neither `count` nor `hash`.
    Kind,
    /// `n` is zero, or `m` is outside `1..=n`.
    Bounds,
}

impl fmt::Display for PartitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape => f.write_str("expected `count:<m>/<n>` or `hash:<m>/<n>`"),
            Self::Kind => f.write_str("expected the partition kind to be `count` or `hash`"),
            Self::Bounds => {
                f.write_str("expected `<n>` to be positive and `<m>` to be between 1 and `<n>`")
            }
        }
    }
}

/// Which of the reported tests a run executes.
#[derive(Debug, Clone, Default)]
pub struct TestFilter {
    /// Positional patterns. A test is kept when it matches **any** of them; an empty list keeps
    /// every test.
    patterns: Vec<String>,
    /// Patterns that exclude, applied after the positional ones.
    skip: Vec<String>,
    /// Match a pattern against the whole id rather than as a substring.
    exact: bool,
    run_ignored: RunIgnored,
    partition: Option<Partition>,
}

impl TestFilter {
    /// A filter that keeps everything not ignored.
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep only tests matching one of `patterns`.
    #[must_use]
    pub fn with_patterns(mut self, patterns: Vec<String>) -> Self {
        self.patterns = patterns;
        self
    }

    /// Drop tests matching one of `skip`.
    #[must_use]
    pub fn with_skip(mut self, skip: Vec<String>) -> Self {
        self.skip = skip;
        self
    }

    /// Match patterns against the whole id.
    #[must_use]
    pub const fn exact(mut self, exact: bool) -> Self {
        self.exact = exact;
        self
    }

    /// What to do with `#[ignore]`.
    #[must_use]
    pub const fn with_ignored(mut self, run_ignored: RunIgnored) -> Self {
        self.run_ignored = run_ignored;
        self
    }

    /// Restrict to one shard.
    #[must_use]
    pub const fn with_partition(mut self, partition: Option<Partition>) -> Self {
        self.partition = partition;
        self
    }

    /// Split `cases` into the tests this run executes and the tests it skips.
    ///
    /// Both halves are returned because a runner reports them: nextest's summary names the skipped
    /// count, and a filter that silently dropped tests would make "0 failed" mean less than it
    /// looks like it means.
    pub fn select(&self, cases: &[TestCase]) -> Selection {
        let mut selected = Vec::new();
        let mut skipped = Vec::new();
        // The partition applies to what the *filters* already chose, so its positions are counted
        // over that list and not over every test the harness knows.
        for case in cases {
            if self.admits(case) {
                selected.push(case.clone());
            } else {
                skipped.push(case.clone());
            }
        }
        if let Some(partition) = self.partition {
            let mut kept = Vec::new();
            for (position, case) in selected.into_iter().enumerate() {
                if partition.holds(position, case.id()) {
                    kept.push(case);
                } else {
                    skipped.push(case);
                }
            }
            selected = kept;
        }
        skipped.sort();
        Selection { selected, skipped }
    }

    /// Whether the filters — everything but the partition — keep `case`.
    fn admits(&self, case: &TestCase) -> bool {
        let ignored_ok = match self.run_ignored {
            RunIgnored::Default => !case.is_ignored(),
            RunIgnored::IgnoredOnly => case.is_ignored(),
            RunIgnored::All => true,
        };
        if !ignored_ok {
            return false;
        }
        if !self.patterns.is_empty()
            && !self
                .patterns
                .iter()
                .any(|pattern| Self::matches(case.id(), pattern, self.exact))
        {
            return false;
        }
        // `--skip` is a substring even under `--exact`: the flag says how a *selection* is
        // spelled, and an exclusion that had to name a test in full could not exclude a class.
        !self
            .skip
            .iter()
            .any(|pattern| Self::matches(case.id(), pattern, false))
    }

    /// Whether `id` matches `pattern`.
    fn matches(id: &str, pattern: &str, exact: bool) -> bool {
        if exact {
            id == pattern
        } else {
            id.contains(pattern)
        }
    }
}

/// What a filter chose, and what it left out.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    selected: Vec<TestCase>,
    skipped: Vec<TestCase>,
}

impl Selection {
    /// The tests to run, in report order.
    pub fn selected(&self) -> &[TestCase] {
        &self.selected
    }

    /// The tests the filters left out, in report order.
    pub fn skipped(&self) -> &[TestCase] {
        &self.skipped
    }
}

#[cfg(test)]
mod tests {
    use super::{Partition, PartitionError, RunIgnored, TestCase, TestFilter};
    use alloc::borrow::ToOwned;
    use alloc::vec;
    use alloc::vec::Vec;

    fn cases() -> Vec<TestCase> {
        [
            "com.example.MathTest#adds\t",
            "com.example.MathTest#divides\tshould_fail",
            "com.example.MathTest#slow\tignore",
            "com.example.TextTest#trims\t",
        ]
        .iter()
        .map(|line| TestCase::parse(line).unwrap())
        .collect()
    }

    fn ids(cases: &[TestCase]) -> Vec<&str> {
        cases.iter().map(TestCase::id).collect()
    }

    #[test]
    fn a_list_line_carries_the_id_and_its_flags() {
        let case = TestCase::parse("com.example.MathTest#adds\t").unwrap();
        assert_eq!(case.class(), "com.example.MathTest");
        assert_eq!(case.method(), "adds");
        assert!(!case.is_ignored() && !case.should_fail());

        let both = TestCase::parse("a.B#c\tignore,should_fail").unwrap();
        assert!(both.is_ignored() && both.should_fail());
        // A trailing newline from a line-oriented read is not part of the flags.
        assert!(TestCase::parse("a.B#c\tignore\r\n").unwrap().is_ignored());
        // An unknown flag is carried past rather than refused.
        assert!(TestCase::parse("a.B#c\tfuture").is_some());
        // Something that is not a test id at all.
        assert!(TestCase::parse("").is_none());
        assert!(TestCase::parse("no-separator").is_none());
    }

    #[test]
    fn an_empty_filter_runs_everything_but_the_ignored() {
        let selection = TestFilter::new().select(&cases());
        assert_eq!(
            ids(selection.selected()),
            [
                "com.example.MathTest#adds",
                "com.example.MathTest#divides",
                "com.example.TextTest#trims"
            ]
        );
        // The skipped half is reported, not discarded.
        assert_eq!(ids(selection.skipped()), ["com.example.MathTest#slow"]);
    }

    #[test]
    fn patterns_select_and_skip_excludes() {
        let filter = TestFilter::new().with_patterns(vec!["MathTest".to_owned()]);
        assert_eq!(
            ids(filter.select(&cases()).selected()),
            ["com.example.MathTest#adds", "com.example.MathTest#divides"]
        );

        // Two patterns are a union.
        let filter = TestFilter::new().with_patterns(vec!["adds".to_owned(), "trims".to_owned()]);
        assert_eq!(
            ids(filter.select(&cases()).selected()),
            ["com.example.MathTest#adds", "com.example.TextTest#trims"]
        );

        // `--exact` matches the whole id; the same string as a substring would have matched.
        let filter = TestFilter::new()
            .with_patterns(vec!["com.example.MathTest#adds".to_owned()])
            .exact(true);
        assert_eq!(
            ids(filter.select(&cases()).selected()),
            ["com.example.MathTest#adds"]
        );
        let filter = TestFilter::new()
            .with_patterns(vec!["MathTest".to_owned()])
            .exact(true);
        assert!(filter.select(&cases()).selected().is_empty());

        // `--skip` applies after the positional patterns, and stays a substring under `--exact`
        // so that it can name a class.
        let filter = TestFilter::new()
            .with_patterns(vec!["com.example".to_owned()])
            .with_skip(vec!["MathTest".to_owned()]);
        assert_eq!(
            ids(filter.select(&cases()).selected()),
            ["com.example.TextTest#trims"]
        );
    }

    #[test]
    fn run_ignored_selects_the_parked_tests() {
        let only = TestFilter::new().with_ignored(RunIgnored::IgnoredOnly);
        assert_eq!(
            ids(only.select(&cases()).selected()),
            ["com.example.MathTest#slow"]
        );
        let all = TestFilter::new().with_ignored(RunIgnored::All);
        assert_eq!(all.select(&cases()).selected().len(), 4);
        assert!(all.select(&cases()).skipped().is_empty());
    }

    #[test]
    fn a_partition_covers_every_test_exactly_once() {
        for kind in ["count", "hash"] {
            let all = cases();
            let mut seen: Vec<alloc::string::String> = Vec::new();
            for shard in 1..=3 {
                let spec = alloc::format!("{kind}:{shard}/3");
                let filter = TestFilter::new()
                    .with_ignored(RunIgnored::All)
                    .with_partition(Some(Partition::parse(&spec).unwrap()));
                let selection = filter.select(&all);
                seen.extend(selection.selected().iter().map(|case| case.id().to_owned()));
                // Whatever this shard did not take is reported as skipped, so a shard's own
                // summary still accounts for every test.
                assert_eq!(
                    selection.selected().len() + selection.skipped().len(),
                    all.len()
                );
            }
            seen.sort_unstable();
            let mut expected: Vec<alloc::string::String> =
                all.iter().map(|case| case.id().to_owned()).collect();
            expected.sort_unstable();
            assert_eq!(
                seen, expected,
                "`{kind}` must cover every test exactly once"
            );
        }
    }

    #[test]
    fn the_hash_partition_is_pinned_so_a_shard_never_moves() {
        // A shard assignment that changed between releases would silently skip a test in a CI
        // matrix mid-upgrade, with every shard still reporting success. These are the values the
        // pinned FNV-1a produces.
        let assign = |id: &str| {
            (1..=4)
                .find(|shard| {
                    Partition::parse(&alloc::format!("hash:{shard}/4"))
                        .unwrap()
                        .holds(0, id)
                })
                .unwrap()
        };
        assert_eq!(assign("com.example.MathTest#adds"), 4);
        assert_eq!(assign("com.example.MathTest#divides"), 4);
        assert_eq!(assign("com.example.TextTest#trims"), 2);
    }

    #[test]
    fn a_partition_spec_names_a_shard_or_is_refused() {
        assert!(Partition::parse("count:1/2").is_ok());
        assert!(Partition::parse("hash:2/2").is_ok());
        assert_eq!(Partition::parse("1/2"), Err(PartitionError::Shape));
        assert_eq!(Partition::parse("count:1"), Err(PartitionError::Shape));
        assert_eq!(Partition::parse("nope:1/2"), Err(PartitionError::Kind));
        // Out of range rather than clamped: a mis-numbered CI shard must fail, not run twice.
        assert_eq!(Partition::parse("count:0/2"), Err(PartitionError::Bounds));
        assert_eq!(Partition::parse("count:3/2"), Err(PartitionError::Bounds));
        assert_eq!(Partition::parse("count:1/0"), Err(PartitionError::Bounds));
    }
}
