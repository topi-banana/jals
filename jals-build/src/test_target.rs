//! Turning a `[[test-target]]` into something a process can be started from.
//!
//! A target's `args` are written before anything exists: the run directory has no name yet, and
//! the directories a build task's artifacts were materialized into are addressed by content, so
//! their paths are not knowable until after the task ran. **Placeholders are how a manifest names
//! them anyway**, and expanding them is this module's job.
//!
//! ```text
//! args     = ["--gameDir", "{run-dir}", "--assetsDir", "{dir:assets}"]
//! jvm-args = ["-Djava.library.path={dir:natives}"]
//! ```
//!
//! Two rules keep the vocabulary honest. An **unknown** `{dir:…}` is an error rather than an empty
//! string, because a program handed `--assetsDir ""` fails in a way that names the wrong cause. A
//! brace that opens neither placeholder is **left alone**, because `{` is an ordinary character in
//! a JVM argument and a manifest should not have to escape one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jals_config::ResourcePattern;
use jals_config::testing::{Placeholder, TestTarget};
use jals_storage::RelativePath;

/// A `[[test-target]]` with its placeholders expanded and its paths resolved against the host.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    name: String,
    main_class: String,
    args: Vec<String>,
    jvm_args: Vec<String>,
    list_argument: String,
    run_dir: PathBuf,
    report: PathBuf,
    seed: Option<PathBuf>,
    /// Compiled `artifacts` globs.
    ///
    /// `ResourcePattern` rather than a matcher of this crate's own: the pattern language a
    /// `[build.resources] template` entry is written in (`*`, `?`, `**`) is exactly the one an
    /// artifact glob needs, and a second implementation would be a second set of edge cases.
    artifacts: Vec<ResourcePattern>,
    timeout: Option<Duration>,
    screenshot_dir: Option<RelativePath>,
}

impl ResolvedTarget {
    /// The scratch directory one target run owns, under the project root.
    ///
    /// Named by this process's id, like the harness runner's, so a second `jals test` in the same
    /// checkout cannot delete the run directory the first one is still writing into. Under
    /// `target/jals/build`, which is what makes `jals clean` reap it.
    #[must_use]
    pub fn scratch(project_root: &Path, name: &str) -> PathBuf {
        project_root
            .join(crate::test_runner::TEST_RUN_DIR.replace('/', std::path::MAIN_SEPARATOR_STR))
            .join(std::process::id().to_string())
            .join(name)
    }

    /// Resolve `target` for a run in `run_dir`.
    ///
    /// `runtime_dirs` maps a name a build task published a directory under to where that directory
    /// was materialized.
    ///
    /// # Errors
    /// [`TargetError`] when a placeholder names a directory that was not published, or a declared
    /// path is not a portable project path.
    pub fn resolve(
        target: &TestTarget,
        project_root: &Path,
        run_dir: PathBuf,
        runtime_dirs: &BTreeMap<String, PathBuf>,
    ) -> Result<Self, TargetError> {
        let dirs: BTreeMap<&str, String> = runtime_dirs
            .iter()
            .map(|(name, path)| (name.as_str(), path.display().to_string()))
            .collect();
        let expander = Expander {
            target: &target.name,
            run_dir: run_dir.display().to_string(),
            runtime_dirs: &dirs,
        };
        let args = expander.expand_all(&target.args)?;
        let jvm_args = expander.expand_all(&target.jvm_args)?;

        let report = Self::project_path(&target.name, "report.file", &target.report.file)?
            .to_host_path(&run_dir);
        let seed = target
            .run_dir
            .seed
            .as_deref()
            .map(|seed| Self::project_path(&target.name, "run-dir.seed", seed))
            .transpose()?
            .map(|path| path.to_host_path(project_root));
        let screenshot_dir = if target.screenshots.dir.is_empty() {
            None
        } else {
            Some(Self::project_path(
                &target.name,
                "screenshots.dir",
                &target.screenshots.dir,
            )?)
        };
        let artifacts = target
            .artifacts
            .iter()
            .map(|pattern| {
                ResourcePattern::parse(pattern).map_err(|_| TargetError::Artifact {
                    target: target.name.clone(),
                    pattern: pattern.clone(),
                })
            })
            .collect::<Result<_, _>>()?;

        Ok(Self {
            name: target.name.clone(),
            main_class: target.main_class.clone(),
            args,
            jvm_args,
            list_argument: target.list_argument.clone(),
            run_dir,
            report,
            seed,
            artifacts,
            timeout: target.timeout.map(Duration::from_secs),
            screenshot_dir,
        })
    }

    /// Parse one declared path, naming the field it came from when it is not one.
    fn project_path(
        target: &str,
        field: &'static str,
        value: &str,
    ) -> Result<RelativePath, TargetError> {
        RelativePath::parse(value).map_err(|_| TargetError::Path {
            target: target.to_owned(),
            field,
            value: value.to_owned(),
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn main_class(&self) -> &str {
        &self.main_class
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    #[must_use]
    pub fn jvm_args(&self) -> &[String] {
        &self.jvm_args
    }

    #[must_use]
    pub fn list_argument(&self) -> &str {
        &self.list_argument
    }

    #[must_use]
    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    #[must_use]
    pub fn report(&self) -> &Path {
        &self.report
    }

    /// The directory copied into the run directory before the process starts.
    #[must_use]
    pub fn seed(&self) -> Option<&Path> {
        self.seed.as_deref()
    }

    #[must_use]
    pub const fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Where this target's screenshots land, relative to the run directory.
    #[must_use]
    pub const fn screenshot_dir(&self) -> Option<&RelativePath> {
        self.screenshot_dir.as_ref()
    }

    /// Whether `path`, relative to the run directory, is something the target declared worth
    /// keeping.
    #[must_use]
    pub fn is_artifact(&self, path: &RelativePath) -> bool {
        self.artifacts.iter().any(|pattern| pattern.matches(path))
    }

    /// Whether the target declared any artifacts at all.
    #[must_use]
    pub const fn collects_artifacts(&self) -> bool {
        !self.artifacts.is_empty()
    }
}

/// Expands a target's placeholders.
struct Expander<'a> {
    target: &'a str,
    run_dir: String,
    runtime_dirs: &'a BTreeMap<&'a str, String>,
}

impl Expander<'_> {
    fn expand_all(&self, values: &[String]) -> Result<Vec<String>, TargetError> {
        values.iter().map(|value| self.expand(value)).collect()
    }

    /// Expand every placeholder in one argument.
    fn expand(&self, value: &str) -> Result<String, TargetError> {
        let mut out = String::with_capacity(value.len());
        let mut rest = value;
        while let Some(at) = rest.find('{') {
            out.push_str(&rest[..at]);
            let tail = &rest[at..];
            if let Some(after) = tail.strip_prefix(Placeholder::RUN_DIR) {
                out.push_str(&self.run_dir);
                rest = after;
                continue;
            }
            if let Some(after) = tail.strip_prefix(Placeholder::RUNTIME_DIR_PREFIX) {
                let Some(end) = after.find(Placeholder::CLOSE) else {
                    // An unterminated `{dir:` is a typo, not a literal: nothing else in a command
                    // line opens with it.
                    return Err(TargetError::Unterminated {
                        target: self.target.to_owned(),
                        value: value.to_owned(),
                    });
                };
                let name = &after[..end];
                let Some(path) = self.runtime_dirs.get(name) else {
                    return Err(TargetError::UnknownDir {
                        target: self.target.to_owned(),
                        name: name.to_owned(),
                        known: self.runtime_dirs.keys().map(|k| (*k).to_owned()).collect(),
                    });
                };
                out.push_str(path);
                rest = &after[end + 1..];
                continue;
            }
            // A brace that opens neither placeholder is an ordinary character.
            out.push('{');
            rest = &tail[1..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// Why a target could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// A `{dir:…}` naming a directory no build task published.
    UnknownDir {
        target: String,
        name: String,
        /// The names that *are* published, so the message can say what was available.
        known: Vec<String>,
    },
    /// A `{dir:` with no closing brace.
    Unterminated { target: String, value: String },
    /// A declared path that is not a relative path inside the project.
    Path {
        target: String,
        field: &'static str,
        value: String,
    },
    /// An `artifacts` entry that is not a well-formed glob.
    Artifact { target: String, pattern: String },
}

impl core::fmt::Display for TargetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownDir {
                target,
                name,
                known,
            } if known.is_empty() => write!(
                f,
                "test target `{target}` names the runtime directory `{name}`, but the build script \
                 published none at all"
            ),
            Self::UnknownDir {
                target,
                name,
                known,
            } => write!(
                f,
                "test target `{target}` names the runtime directory `{name}`, which no build task \
                 published; available: {}",
                known.join(", ")
            ),
            Self::Unterminated { target, value } => write!(
                f,
                "test target `{target}` has an unterminated `{{dir:` in `{value}`"
            ),
            Self::Path {
                target,
                field,
                value,
            } => write!(
                f,
                "test target `{target}` has `{field} = \"{value}\"`, which is not a relative path \
                 inside the project"
            ),
            Self::Artifact { target, pattern } => write!(
                f,
                "test target `{target}` has an `artifacts` entry `{pattern}` that is not a valid \
                 glob"
            ),
        }
    }
}

impl core::error::Error for TargetError {}

#[cfg(test)]
mod tests {
    use super::{Expander, ResolvedTarget, TargetError};
    use jals_config::testing::TestTarget;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// `-Djava.library.path={dir:natives}`, assembled rather than written as one literal: a bare
    /// `{…}` in source reads to clippy as a formatting argument, and the placeholder vocabulary
    /// this module implements is spelled with braces.
    const NATIVES_ARG: &str = concat!("-Djava.library.path=", "{", "dir:natives", "}");

    fn expander<'a>(dirs: &'a BTreeMap<&'a str, String>) -> Expander<'a> {
        Expander {
            target: "e2e",
            run_dir: "/tmp/run".to_owned(),
            runtime_dirs: dirs,
        }
    }

    #[test]
    fn a_run_dir_placeholder_expands_wherever_it_appears() {
        let dirs = BTreeMap::new();
        let expander = expander(&dirs);
        assert_eq!(expander.expand("{run-dir}").expect("expands"), "/tmp/run");
        assert_eq!(
            expander
                .expand("--gameDir={run-dir}/world")
                .expect("expands"),
            "--gameDir=/tmp/run/world"
        );
        assert_eq!(
            expander.expand("{run-dir}{run-dir}").expect("expands"),
            "/tmp/run/tmp/run"
        );
    }

    #[test]
    fn a_runtime_dir_placeholder_expands_by_name() {
        let dirs: BTreeMap<&str, String> = [
            ("assets", "/cache/tree-view/aa".to_owned()),
            ("natives", "/cache/tree-view/bb".to_owned()),
        ]
        .into_iter()
        .collect();
        let expander = expander(&dirs);
        assert_eq!(
            expander.expand(NATIVES_ARG).expect("expands"),
            "-Djava.library.path=/cache/tree-view/bb"
        );
        assert_eq!(
            expander.expand("{dir:assets}").expect("expands"),
            "/cache/tree-view/aa"
        );
    }

    #[test]
    fn an_unpublished_directory_is_an_error_that_says_what_was_available() {
        let dirs: BTreeMap<&str, String> = core::iter::once(("assets", "/a".to_owned())).collect();
        let error = expander(&dirs)
            .expand("{dir:natives}")
            .expect_err("natives was not published");
        let TargetError::UnknownDir { name, known, .. } = &error else {
            panic!("expected an unknown directory, got {error:?}");
        };
        assert_eq!(name, "natives");
        assert_eq!(known, &["assets".to_owned()]);
        assert!(error.to_string().contains("available: assets"));
    }

    #[test]
    fn an_empty_publication_set_says_so_rather_than_listing_nothing() {
        let dirs = BTreeMap::new();
        let error = expander(&dirs).expand("{dir:assets}").expect_err("none");
        assert!(
            error.to_string().contains("published none at all"),
            "{error}"
        );
    }

    #[test]
    fn an_unterminated_placeholder_is_refused_rather_than_copied() {
        let dirs = BTreeMap::new();
        assert!(matches!(
            expander(&dirs).expand("{dir:natives"),
            Err(TargetError::Unterminated { .. })
        ));
    }

    #[test]
    fn a_brace_that_opens_no_placeholder_is_an_ordinary_character() {
        let dirs = BTreeMap::new();
        let expander = expander(&dirs);
        assert_eq!(
            expander.expand("-Dpattern={a,b}").expect("expands"),
            "-Dpattern={a,b}"
        );
        assert_eq!(expander.expand("{").expect("expands"), "{");
        assert_eq!(
            expander.expand("{run-dir} {x} {run-dir}").expect("expands"),
            "/tmp/run {x} /tmp/run"
        );
    }

    #[test]
    fn a_resolved_target_places_its_report_and_seed() {
        let target = TestTarget {
            name: "e2e".to_owned(),
            main_class: "com.example.Driver".to_owned(),
            args: vec!["--gameDir".to_owned(), "{run-dir}".to_owned()],
            artifacts: vec!["logs/**".to_owned(), "*.png".to_owned()],
            run_dir: jals_config::testing::RunDir {
                seed: Some("fixtures/run".to_owned()),
            },
            ..TestTarget::default()
        };
        let resolved = ResolvedTarget::resolve(
            &target,
            Path::new("/project"),
            PathBuf::from("/scratch/e2e"),
            &BTreeMap::new(),
        )
        .expect("resolves");

        assert_eq!(resolved.args(), ["--gameDir", "/scratch/e2e"]);
        assert_eq!(resolved.report(), Path::new("/scratch/e2e/report.tsv"));
        assert_eq!(resolved.seed(), Some(Path::new("/project/fixtures/run")));
        assert_eq!(resolved.list_argument(), "--list");

        let logs = jals_storage::RelativePath::parse("logs/latest.log").expect("a path");
        let shot = jals_storage::RelativePath::parse("title.png").expect("a path");
        let other = jals_storage::RelativePath::parse("world/level.dat").expect("a path");
        assert!(resolved.is_artifact(&logs));
        assert!(resolved.is_artifact(&shot));
        assert!(!resolved.is_artifact(&other));
    }

    #[test]
    fn a_target_naming_an_unpublished_directory_does_not_resolve() {
        let target = TestTarget {
            name: "e2e".to_owned(),
            main_class: "com.example.Driver".to_owned(),
            jvm_args: vec![NATIVES_ARG.to_owned()],
            ..TestTarget::default()
        };
        assert!(matches!(
            ResolvedTarget::resolve(
                &target,
                Path::new("/project"),
                PathBuf::from("/scratch/e2e"),
                &BTreeMap::new()
            ),
            Err(TargetError::UnknownDir { .. })
        ));
    }
}
