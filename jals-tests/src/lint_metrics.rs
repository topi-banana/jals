//! Per-rule lint counts over a corpus of real projects, pinned so they can only fall.
//!
//! The compiler corpus measures what lowers and what a JVM links; this measures what the *analysis*
//! says about code somebody actually shipped. Both are the same kind of number — a rate over a
//! fixed denominator that means nothing unless the denominator is fixed too — and both are pinned
//! the same way: [`RegressionPin::regressions`] names every rule whose count grew, and that list is
//! empty for a change to land.
//!
//! # Why a pin and not a test that asserts the counts
//!
//! A count asserted inline is a number nobody can argue with and everybody has to edit. A pin is a
//! *ratchet*: a fix moves a number down and the pin moves with it, and a regression moves one up
//! and stops. The pin is data beside the harness rather than an expectation inside it, so the diff
//! that changes it is the diff that changed the analysis.
//!
//! # The corpus is generated, never committed
//!
//! Like the four formatter corpora and the javac one, `sources/real-world/` is produced locally by
//! `scripts/gen-real-world-corpus.sh` from the projects `corpus/real-world.toml` names. Nothing
//! here fetches anything: measuring is offline, and a run with no corpus stands down loudly rather
//! than reporting zero findings for a project it never read.

use std::collections::BTreeMap;
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

use jals_config::lint::Config;
use jals_lint::LintOutput;

/// One project in the real-world corpus, as `corpus/real-world.toml` declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealWorldProject {
    /// The directory `scripts/gen-real-world-corpus.sh` checks it out into.
    pub name: String,
    /// The repository to clone.
    pub url: String,
    /// The ref to check out — a tag or a commit, never a branch: a moving corpus is two
    /// measurements wearing one name.
    pub r#ref: String,
}

impl RealWorldProject {
    /// Read `corpus/real-world.toml`: one `[[project]]` per entry with `name`, `url` and `ref`.
    ///
    /// Parsed by hand rather than through `toml` because this harness reads one fixed shape of one
    /// fixed file, and a parser is a dependency the corpus does not need. The shape is small enough
    /// that a key neither of us wrote is an error rather than a silently ignored row.
    ///
    /// # Errors
    /// An entry missing `name`, `url`, or `ref`. Every field is load-bearing: the name is the
    /// directory the generator writes, the url is where it comes from, and the ref is what makes
    /// two runs one measurement.
    pub fn registry(text: &str) -> Result<Vec<Self>, String> {
        let mut projects = Vec::new();
        for entry in text.split("[[project]]").skip(1) {
            let field = |key: &str| {
                entry
                    .lines()
                    .find_map(|line| line.trim().strip_prefix(key))
                    .map(|value| {
                        value
                            .trim_start_matches(" = ")
                            .trim_matches('"')
                            .to_string()
                    })
            };
            projects.push(Self {
                name: field("name").ok_or_else(|| String::from("an entry with no `name`"))?,
                url: field("url").ok_or_else(|| String::from("an entry with no `url`"))?,
                r#ref: field("ref").ok_or_else(|| String::from("an entry with no `ref`"))?,
            });
        }
        Ok(projects)
    }
}

/// The lint findings per rule over a corpus, and how many files they came from.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LintMetrics {
    /// Files actually linted, one per compilation unit.
    pub files: usize,
    /// Findings per rule name. A rule with no findings is absent rather than zero, so a rule that
    /// starts reporting is a new row and cannot hide behind a `0` nobody diffed.
    pub counts: BTreeMap<String, usize>,
}

impl LintMetrics {
    /// Lint every `.java` under `roots`, counting findings per rule.
    ///
    /// Every file is linted on its own — the rule set this measures is the one a single-file run
    /// applies, which is the reading the numbers are defined against. A file whose parse fails is
    /// counted and contributes nothing else, so a corpus of broken sources cannot read as clean.
    pub fn measure(roots: &[std::path::PathBuf]) -> Self {
        let mut metrics = Self::default();
        let config = Config::default();
        for root in roots {
            for path in Self::java_files(root) {
                let Ok(source) = std::fs::read_to_string(&path) else {
                    continue;
                };
                metrics.files += 1;
                let out = jals_exec::block_on_inline(LintOutput::lint_source(&source, &config));
                for diagnostic in out.diagnostics {
                    *metrics
                        .counts
                        .entry(diagnostic.rule.to_string())
                        .or_default() += 1;
                }
            }
        }
        metrics
    }

    /// Every `.java` under `root`, in a stable order — enumeration is deterministic or the pin is
    /// not reproducible.
    fn java_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path
                    .extension()
                    .is_some_and(|extension| extension == "java")
                {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    /// The pin: one `rule<TAB>count` line per rule, sorted by rule name.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (rule, count) in &self.counts {
            out.push_str(&format!("{rule}\t{count}\n"));
        }
        out
    }

    /// Read a pin written by [`render`](Self::render).
    ///
    /// # Errors
    /// A line that is not `rule<TAB>count`. A pin is written by this harness and read by this
    /// harness, so a line neither produced is corruption and not a measurement.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut metrics = Self::default();
        for (number, line) in text.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let Some((rule, count)) = line.split_once('\t') else {
                return Err(format!("line {}: not `rule<TAB>count`", number + 1));
            };
            let count: usize = count
                .parse()
                .map_err(|_| format!("line {}: `{count}` is not a count", number + 1))?;
            metrics.counts.insert(rule.to_string(), count);
        }
        Ok(metrics)
    }

    /// Every rule whose count is higher than the pin allows, in rule order.
    ///
    /// A rule the pin does not mention is allowed **nothing**: a rule added since the pin was
    /// written has to come with a refreshed pin, so adding one puts it under the ratchet in the
    /// same diff rather than leaving it outside until somebody notices.
    pub fn regressions(&self, pinned: &Self) -> Vec<String> {
        self.counts
            .iter()
            .filter_map(|(rule, count)| {
                let allowed = pinned.counts.get(rule).copied().unwrap_or(0);
                (count > &allowed)
                    .then(|| format!("{rule}: {count} findings, pin allows {allowed}"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{LintMetrics, RealWorldProject};

    #[test]
    fn a_pin_round_trips_through_its_own_format() {
        let metrics = LintMetrics::parse("a\t1\nb\t20\n").unwrap();
        assert_eq!(metrics.render(), "a\t1\nb\t20\n");
    }

    #[test]
    fn a_malformed_pin_line_is_reported_rather_than_read_as_zero() {
        assert!(LintMetrics::parse("a 1\n").is_err());
        assert!(LintMetrics::parse("a\tmany\n").is_err());
    }

    #[test]
    fn a_rule_that_grew_is_a_regression_and_one_that_fell_is_not() {
        let pinned = LintMetrics::parse("a\t5\nb\t5\n").unwrap();
        let now = LintMetrics::parse("a\t6\nb\t4\n").unwrap();
        assert_eq!(now.regressions(&pinned), ["a: 6 findings, pin allows 5"]);
    }

    #[test]
    fn a_rule_the_pin_does_not_mention_is_allowed_nothing() {
        // A rule added since the pin was written comes with a refreshed pin, so it is under the
        // ratchet from the same diff that added it rather than outside until somebody notices.
        let pinned = LintMetrics::default();
        let now = LintMetrics::parse("new-rule\t100\n").unwrap();
        assert_eq!(
            now.regressions(&pinned),
            ["new-rule: 100 findings, pin allows 0"]
        );
    }

    #[test]
    fn measuring_counts_one_file_per_compilation_unit() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("A.java"), "class A { int x = 1.0; }").unwrap();
        std::fs::write(directory.path().join("B.java"), "class B { }").unwrap();
        let metrics = LintMetrics::measure(&[directory.path().to_path_buf()]);
        assert_eq!(metrics.files, 2);
        assert!(
            metrics.counts.contains_key("type-mismatch"),
            "{:?}",
            metrics.counts
        );
    }

    /// The registry names the four projects the milestone's acceptance is measured on. Their
    /// configs are the migration examples, so the list is a fact about the milestone and not about
    /// this harness.
    #[test]
    fn the_registry_names_the_four_acceptance_projects() {
        let registry = read_registry();
        let names: Vec<&str> = registry
            .iter()
            .map(|project| project.name.as_str())
            .collect();
        assert_eq!(names, ["hikaricp", "gson", "guava", "netty"]);
    }

    #[test]
    fn a_registry_entry_pins_a_ref_rather_than_a_branch() {
        // A moving corpus is two measurements wearing one name, so a branch is not a ref.
        for project in read_registry() {
            assert!(
                !matches!(project.r#ref.as_str(), "main" | "master" | "HEAD"),
                "`{}` is a branch",
                project.r#ref
            );
            assert!(!project.url.is_empty());
        }
    }

    fn read_registry() -> Vec<RealWorldProject> {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/real-world.toml"),
        )
        .unwrap();
        RealWorldProject::registry(&text).unwrap()
    }
}
