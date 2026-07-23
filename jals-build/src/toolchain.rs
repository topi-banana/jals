//! The toolchain abstraction: how a project's `javac`/`java` are selected and driven.
//!
//! The seam is a pair of object-safe traits mirroring the manifest's two selectors: a [`Compiler`]
//! turns a [`CompileRequest`] into an actual build, a [`Runtime`] turns a [`RunRequest`] into an
//! actual run. The host's `SubprocessToolchain` (behind the `native` feature) implements both by
//! spawning `javac`/`java`, and the in-process `BuiltinToolchain` implements both without a
//! subprocess; matching a manifest's [`Compiler`](jals_config::Compiler) /
//! [`Runtime`](jals_config::Runtime) selector picks the backend, passed on as a `&dyn Compiler` /
//! `&dyn Runtime` — so `jals-cli`/the playground drive a build without knowing which backend
//! realizes each step.
//!
//! Selecting *which* JDK tool to spawn is split into a **pure policy** ([`ToolResolver`], here — no
//! filesystem or environment access) and the **host mechanism** (`native`: discovering installed
//! JDKs, reading env vars, probing which candidate exists, spawning). This keeps the crate's "never
//! touches the filesystem" invariant: the host feeds the environment in as plain data (the
//! discovered installs, `$JAVAC`/`$JAVA`, `$JAVA_HOME`, `$HOME`, the project root), the pure policy
//! computes the ordered [`Candidates`], and the host picks the first that exists.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use jals_config::ToolSpec;

use crate::request::{CompileRequest, RunRequest};

/// The boxed future shape of a toolchain step.
///
/// `!Send` (the workspace execution model) and object-safe, so a host keeps driving
/// `&dyn Compiler` / `&dyn Runtime` while the steps await subprocesses and storage.
pub type ToolchainFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BuildOutcome, ToolchainError>> + 'a>>;

/// Which JDK tool a request needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// The compiler, `javac`.
    Javac,
    /// The runtime, `java`.
    Java,
}

impl Tool {
    /// The bare executable name (`"javac"` / `"java"`), used both as the `bin/` leaf of a JDK home
    /// and as the ultimate `PATH`-resolved fallback.
    pub const fn binary_name(self) -> &'static str {
        match self {
            Self::Javac => "javac",
            Self::Java => "java",
        }
    }

    /// The environment variable that hard-overrides this tool's path (`$JAVAC` / `$JAVA`), honored
    /// above the `[toolchain]` selection for CI/back-compat (see [`ToolResolver::resolve`]).
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Javac => "JAVAC",
            Self::Java => "JAVA",
        }
    }

    /// Where this tool lives inside a JDK `home` (`<home>/bin/<tool>`) — the one place the JDK
    /// layout rule is encoded.
    pub fn path_in(self, home: &Path) -> PathBuf {
        home.join("bin").join(self.binary_name())
    }
}

/// One installed JDK the host discovered, described enough for [`ToolResolver::resolve`] to match
/// a [`ToolSpec::Distribution`] selector against it.
///
/// The `native` feature builds these by scanning common install locations and classifying each
/// directory name via [`from_install_name`](JdkInstall::from_install_name); the pure resolver only
/// reads the fields, so resolution stays unit-testable with injected installs and no JDK present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JdkInstall {
    /// The JDK home directory (the parent of `bin/`).
    pub home: PathBuf,
    /// The distribution / vendor parsed from the install (`temurin`, `openjdk`, …), if determinable.
    pub distribution: Option<String>,
    /// The major Java version parsed from the install (`21`), if determinable.
    pub version: Option<u32>,
}

impl JdkInstall {
    /// Describe the JDK at `home` from its install directory `name` — a best-effort
    /// `(distribution, version)` classification.
    ///
    /// Handles the layouts the common install roots produce: SDKMAN vendor-suffixed names
    /// (`21.0.2-tem`), distro-prefixed names (`temurin-21.0.2`, `java-17-openjdk-amd64`), and legacy
    /// `1.8`-style versions (`jdk1.8.0_292` → 8). The distribution is canonicalized to the same
    /// lowercase vocabulary [`matches`](Self::matches) compares against, so classification and
    /// matching stay one scheme; an unrecognized vendor or version is `None` (matched leniently).
    pub fn from_install_name(home: PathBuf, name: &str) -> Self {
        Self {
            home,
            distribution: Self::parse_distribution(name),
            version: Self::parse_major_version(name),
        }
    }

    /// Whether this install satisfies a `distribution`/`version` selector. A `None` half of the
    /// selector matches anything; a named distribution matches case-insensitively as a substring
    /// (so `openjdk` matches an `openjdk-21` install directory), and a named version matches exactly.
    fn matches(&self, distribution: Option<&str>, version: Option<u32>) -> bool {
        let dist_ok = distribution.is_none_or(|want| {
            self.distribution.as_deref().is_some_and(|have| {
                have.to_ascii_lowercase()
                    .contains(&want.to_ascii_lowercase())
            })
        });
        let version_ok = version.is_none_or(|want| self.version == Some(want));
        dist_ok && version_ok
    }

    /// The major Java version embedded in an install name, or `None`.
    fn parse_major_version(name: &str) -> Option<u32> {
        // The first run of digits-and-dots is the version (`21.0.2`, `17`, `1.8.0`).
        let start = name.find(|c: char| c.is_ascii_digit())?;
        let run: String = name[start..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let mut parts = run.split('.').filter(|p| !p.is_empty());
        let first: u32 = parts.next()?.parse().ok()?;
        // Legacy `1.N` naming: the real major is the second component.
        if first == 1
            && let Some(second) = parts.next().and_then(|p| p.parse().ok())
        {
            return Some(second);
        }
        Some(first)
    }

    /// The distribution/vendor guessed from an install name, or `None`. Recognizes full vendor names
    /// and SDKMAN's short vendor codes, returning a canonical distribution string.
    fn parse_distribution(name: &str) -> Option<String> {
        // (needle, canonical). Ordered so longer/more-specific needles win over generic ones.
        const VENDORS: &[(&str, &str)] = &[
            ("temurin", "temurin"),
            ("adoptopenjdk", "temurin"),
            ("adopt", "temurin"),
            ("-tem", "temurin"),
            ("corretto", "corretto"),
            ("-amzn", "corretto"),
            ("graalvm", "graalvm"),
            ("-graal", "graalvm"),
            ("-grl", "graalvm"),
            ("zulu", "zulu"),
            ("liberica", "liberica"),
            ("-librca", "liberica"),
            ("semeru", "semeru"),
            ("-sem", "semeru"),
            ("sapmachine", "sapmachine"),
            ("-sapmchn", "sapmachine"),
            ("microsoft", "microsoft"),
            ("-ms", "microsoft"),
            ("dragonwell", "dragonwell"),
            ("oracle", "oracle"),
            ("-oracle", "oracle"),
            ("openjdk", "openjdk"),
            ("-open", "openjdk"),
        ];
        let lower = name.to_ascii_lowercase();
        VENDORS
            .iter()
            .find(|(needle, _)| lower.contains(needle))
            .map(|(_, canonical)| (*canonical).to_owned())
    }
}

/// The ordered result of a [`ToolResolver::resolve`]: candidate program paths to probe, plus the
/// guaranteed fallback.
///
/// Encoding the fallback structurally (rather than as "the last list entry") means a host cannot
/// invent its own fallback or disagree with the policy about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    /// The candidate program paths to probe for existence, most-preferred first.
    pub preferred: Vec<PathBuf>,
    /// The path used when no preferred candidate exists — the selector's natural default. Used
    /// *without* probing, so an explicit-but-wrong selection (a bad `[toolchain]` path, a bogus
    /// `$JAVAC`) surfaces as a spawn error naming it rather than silently reverting to `PATH`.
    pub fallback: PathBuf,
}

impl Candidates {
    /// Pick the program: the first `preferred` candidate that `exists`, else the `fallback`.
    ///
    /// The existence probe is injected so the policy stays pure and the host owns the one
    /// filesystem touch (`Path::is_file`).
    pub fn pick(self, exists: impl Fn(&Path) -> bool) -> PathBuf {
        self.preferred
            .into_iter()
            .find(|candidate| exists(candidate))
            .unwrap_or(self.fallback)
    }

    /// [`pick`](Self::pick) with an async existence probe, so the host can keep its filesystem
    /// touches off the executor. Same rule, one place: first existing preferred, else fallback.
    pub async fn pick_async(self, exists: impl AsyncFn(&Path) -> bool) -> PathBuf {
        for candidate in self.preferred {
            if exists(&candidate).await {
                return candidate;
            }
        }
        self.fallback
    }
}

/// The pure policy that turns a [`ToolSpec`] view (see `jals_config::Compiler::spec` /
/// `jals_config::Runtime::spec`) into the ordered [`Candidates`] for a tool.
///
/// Holds the host environment as plain data, so the *complete* selection precedence — including
/// the `$JAVAC`/`$JAVA` override and `~` expansion — lives (and is tested) here; the host only
/// reads env vars, probes existence, and spawns.
pub struct ToolResolver<'a> {
    /// The installed JDKs the host discovered, matched by a [`ToolSpec::Distribution`].
    pub installs: &'a [JdkInstall],
    /// `$JAVA_HOME`, when set: the preferred system-tool location.
    pub java_home: Option<&'a Path>,
    /// `$HOME`, when known: expands a `~/`-anchored [`ToolSpec::Path`].
    pub home: Option<&'a Path>,
    /// The project root, against which a relative [`ToolSpec::Path`] is resolved.
    pub project_root: &'a Path,
}

impl ToolResolver<'_> {
    /// Compute the [`Candidates`] for `tool`, most-preferred first.
    ///
    /// Precedence:
    ///
    /// - `env_override` (the host-read `$JAVAC`/`$JAVA`, see [`Tool::env_var`]): wins
    ///   unconditionally and is used verbatim (no candidates to probe, no fallback past it) —
    ///   CI/back-compat.
    /// - [`Path`](ToolSpec::Path): the given location — the tool binary itself when the path
    ///   already ends in the tool name (used verbatim), else probed as the binary with the JDK-home
    ///   reading (`<path>/bin/<tool>`) as the fallback; `~/` is expanded against `home` and a
    ///   relative path is resolved against `project_root`. **No system fallback** — an explicit
    ///   path is an explicit instruction, so a non-existent one is used verbatim (the spawn then
    ///   fails naming it).
    /// - [`Distribution`](ToolSpec::Distribution): every matching `install`'s `bin/<tool>`, then the
    ///   system locations (an un-discovered distribution reverts to the system tools).
    /// - [`System`](ToolSpec::System) or `None`: `<java_home>/bin/<tool>` (when `java_home` is
    ///   set), falling back to the bare name on `PATH`.
    pub fn resolve(
        &self,
        tool: Tool,
        spec: Option<ToolSpec<'_>>,
        env_override: Option<PathBuf>,
    ) -> Candidates {
        if let Some(explicit) = env_override {
            return Candidates {
                preferred: Vec::new(),
                fallback: explicit,
            };
        }

        let bin = tool.binary_name();
        match spec {
            Some(ToolSpec::Path(raw)) => {
                let base = self.resolve_path(raw);
                // A path already ending in the tool name is the binary itself, used verbatim (like
                // an env override); anything else is probed as the binary, falling back to the JDK
                // home reading (`<path>/bin/<tool>`) so a bad explicit path surfaces as an error.
                if base.file_name().and_then(|n| n.to_str()) == Some(bin) {
                    Candidates {
                        preferred: Vec::new(),
                        fallback: base,
                    }
                } else {
                    let home_bin = tool.path_in(&base);
                    Candidates {
                        preferred: vec![base],
                        fallback: home_bin,
                    }
                }
            }
            Some(ToolSpec::Distribution { name, version }) => {
                let mut preferred: Vec<PathBuf> = self
                    .installs
                    .iter()
                    .filter(|install| install.matches(name, version))
                    .map(|install| tool.path_in(&install.home))
                    .collect();
                preferred.extend(self.java_home_bin(tool));
                Candidates {
                    preferred,
                    fallback: PathBuf::from(bin),
                }
            }
            // `None` is a caller with no program selector — the in-process `builtin` backend
            // (which never resolves a program and is routed away by the selection factories
            // before any resolver runs), or a host with no selection at all. The system reading
            // is the conservative answer for both.
            Some(ToolSpec::System) | None => Candidates {
                preferred: self.java_home_bin(tool).into_iter().collect(),
                fallback: PathBuf::from(bin),
            },
        }
    }

    /// Resolve a `[toolchain]` path selector: `~/…` expands against `home` (verbatim when no home
    /// is known), a relative path resolves against `project_root`, an absolute path is verbatim.
    fn resolve_path(&self, raw: &str) -> PathBuf {
        if let Some(rest) = raw.strip_prefix("~/")
            && let Some(home) = self.home
        {
            return home.join(rest);
        }
        let path = Path::new(raw);
        if path.is_relative() && !raw.starts_with('~') {
            self.project_root.join(path)
        } else {
            path.to_path_buf()
        }
    }

    /// The `<java_home>/bin/<tool>` system candidate, when a `JAVA_HOME` is known.
    fn java_home_bin(&self, tool: Tool) -> Option<PathBuf> {
        self.java_home.map(|home| tool.path_in(home))
    }
}

/// The result of a compile or run: the process exit code (`None` when the process was terminated
/// by a signal), with [`success`](BuildOutcome::success) derived from it.
///
/// Tool-agnostic — it does not leak `std::process::ExitStatus`, so a non-subprocess backend can
/// report an outcome too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildOutcome {
    /// The process exit code, or `None` if terminated by a signal.
    pub code: Option<i32>,
}

impl BuildOutcome {
    /// Whether the step succeeded (exit code `0`).
    pub const fn success(self) -> bool {
        matches!(self.code, Some(0))
    }
}

/// A toolchain failure: the tool could not be launched, or the backend does not support the step.
#[derive(Debug)]
pub enum ToolchainError {
    /// Spawning the program failed (missing JDK, permission, etc.).
    Spawn {
        /// The program that failed to launch.
        program: String,
        /// The underlying OS error.
        source: std::io::Error,
    },
    /// Writing the argument file an over-long command line is spilled into failed.
    ArgumentFile {
        /// The argument file that could not be written.
        path: String,
        /// The underlying OS error.
        source: std::io::Error,
    },
    /// This backend does not support the requested step (e.g. a wasm compiler asked to *run*).
    Unsupported(&'static str),
    /// A project-storage step of an in-process backend failed (for example reading a source or
    /// committing generated output).
    Fs(jals_storage::Error),
}

impl std::fmt::Display for ToolchainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The OS error is part of the message, not just the `source` chain: a caller that
            // flattens this to its `Display` (`jals-cli` does) would otherwise report a permission
            // error, an over-long command line, and a genuinely absent JDK identically. Only a
            // `NotFound` earns the "is a JDK installed" hint.
            Self::Spawn { program, source } => {
                write!(f, "failed to spawn `{program}`: {source}")?;
                if source.kind() == std::io::ErrorKind::NotFound {
                    write!(f, " (is a JDK installed and on PATH?)")?;
                }
                Ok(())
            }
            Self::ArgumentFile { path, source } => {
                write!(f, "failed to write the argument file `{path}`: {source}")
            }
            Self::Unsupported(what) => write!(f, "toolchain does not support {what}"),
            Self::Fs(source) => write!(f, "builtin toolchain I/O failed: {source}"),
        }
    }
}

impl std::error::Error for ToolchainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } | Self::ArgumentFile { source, .. } => Some(source),
            Self::Unsupported(_) => None,
            Self::Fs(source) => Some(source),
        }
    }
}

/// A backend that can compile a project — the trait behind `[toolchain] compiler`.
///
/// The native implementation (`SubprocessToolchain`) spawns `javac`; the in-process
/// [`BuiltinToolchain`](crate::BuiltinToolchain) is today a dummy stand-in and the seam a real
/// embedded compiler fills. [`compile`](Compiler::compile) performs the work and reports a
/// [`BuildOutcome`]; [`describe_compile`](Compiler::describe_compile) renders the planned action
/// for `--dry-run`/`-v` output without performing it. A backend that cannot compile returns
/// [`ToolchainError::Unsupported`]. The trait is object-safe: a host matches the manifest's
/// [`Compiler`](jals_config::Compiler) selector to a backend and drives it as a `&dyn Compiler`
/// (see `<dyn Compiler>::select` under the `native` feature).
pub trait Compiler {
    /// Compile the project described by `req`.
    fn compile<'a>(&'a self, req: &'a CompileRequest<'_>) -> ToolchainFuture<'a>;

    /// A human-readable description of what [`compile`](Compiler::compile) would do (for
    /// `--dry-run`/`-v`). Stays synchronous: rendering a plan is display-only and bounded.
    fn describe_compile(&self, req: &CompileRequest<'_>) -> String;
}

/// A backend that can run a project's main class — the trait behind `[toolchain] runtime`.
///
/// The exact mirror of [`Compiler`] for the run step, selected independently of it (the manifest's
/// two selectors may name *different* backends — a builtin dummy compile checked with the real
/// `java`, or a real `javac` compile whose run is stubbed out — and each `select` factory matches
/// its own enum, so no routing composite is needed). Implemented by the same two backends; driven
/// as a `&dyn Runtime` (see `<dyn Runtime>::select` under the `native` feature).
pub trait Runtime {
    /// Run the project's main class described by `req`.
    fn run<'a>(&'a self, req: &'a RunRequest<'_>) -> ToolchainFuture<'a>;

    /// A human-readable description of what [`run`](Runtime::run) would do (for `--dry-run`/`-v`).
    fn describe_run(&self, req: &RunRequest<'_>) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(home: &str, dist: Option<&str>, version: Option<u32>) -> JdkInstall {
        JdkInstall {
            home: PathBuf::from(home),
            distribution: dist.map(str::to_owned),
            version,
        }
    }

    fn resolver<'a>(installs: &'a [JdkInstall], java_home: Option<&'a Path>) -> ToolResolver<'a> {
        ToolResolver {
            installs,
            java_home,
            home: None,
            project_root: Path::new("/proj"),
        }
    }

    #[test]
    fn env_override_wins_unconditionally() {
        let installs = [install("/jvm/temurin-21", Some("temurin"), Some(21))];
        let out = resolver(&installs, Some(Path::new("/sys/jdk"))).resolve(
            Tool::Javac,
            Some(ToolSpec::System),
            Some(PathBuf::from("/ci/javac")),
        );
        // Used verbatim: nothing to probe, nothing to fall past.
        assert!(out.preferred.is_empty());
        assert_eq!(out.fallback, PathBuf::from("/ci/javac"));
    }

    #[test]
    fn system_spec_uses_java_home_then_bare_name() {
        let out = resolver(&[], Some(Path::new("/opt/java-home"))).resolve(
            Tool::Javac,
            Some(ToolSpec::System),
            None,
        );
        assert_eq!(
            out.preferred,
            vec![PathBuf::from("/opt/java-home/bin/javac")]
        );
        assert_eq!(out.fallback, PathBuf::from("javac"));
    }

    #[test]
    fn none_spec_matches_system() {
        let out = resolver(&[], None).resolve(Tool::Java, None, None);
        assert!(out.preferred.is_empty());
        assert_eq!(out.fallback, PathBuf::from("java"));
    }

    #[test]
    fn home_path_tries_bin_and_has_no_system_fallback() {
        // A path not ending in the tool name is read as a JDK home; the home's `bin/<tool>` is the
        // fallback candidate — never the bare name, so a bogus home errors instead of silently
        // reverting to PATH.
        let out = resolver(&[], Some(Path::new("/sys/jdk"))).resolve(
            Tool::Javac,
            Some(ToolSpec::Path("/opt/jdk-21")),
            None,
        );
        assert_eq!(out.preferred, vec![PathBuf::from("/opt/jdk-21")]);
        assert_eq!(out.fallback, PathBuf::from("/opt/jdk-21/bin/javac"));
    }

    #[test]
    fn binary_path_is_used_verbatim() {
        // A path ending in the tool name is the binary itself: nothing to probe, so a non-existent
        // explicit binary is spawned verbatim (and fails naming it).
        let out = resolver(&[], None).resolve(
            Tool::Javac,
            Some(ToolSpec::Path("/opt/jdk/bin/javac")),
            None,
        );
        assert!(out.preferred.is_empty());
        assert_eq!(out.fallback, PathBuf::from("/opt/jdk/bin/javac"));
    }

    #[test]
    fn relative_path_resolves_against_project_root() {
        // "jdk/bin/javac" ends in the tool name → the binary itself, resolved against the root.
        let out =
            resolver(&[], None).resolve(Tool::Javac, Some(ToolSpec::Path("jdk/bin/javac")), None);
        assert!(out.preferred.is_empty());
        assert_eq!(out.fallback, PathBuf::from("/proj/jdk/bin/javac"));
    }

    #[test]
    fn tilde_path_expands_against_home() {
        let installs = [];
        let resolver = ToolResolver {
            installs: &installs,
            java_home: None,
            home: Some(Path::new("/home/dev")),
            project_root: Path::new("/proj"),
        };
        let out = resolver.resolve(Tool::Javac, Some(ToolSpec::Path("~/jdks/21")), None);
        assert_eq!(out.preferred, vec![PathBuf::from("/home/dev/jdks/21")]);
        assert_eq!(out.fallback, PathBuf::from("/home/dev/jdks/21/bin/javac"));
    }

    #[test]
    fn tilde_path_without_home_is_verbatim() {
        // No `$HOME` known: the `~` path is used verbatim (never joined onto the project root).
        let out = resolver(&[], None).resolve(Tool::Javac, Some(ToolSpec::Path("~/jdks/21")), None);
        assert_eq!(out.preferred, vec![PathBuf::from("~/jdks/21")]);
    }

    #[test]
    fn distribution_matches_by_name_and_version() {
        let installs = vec![
            install("/jvm/openjdk-17", Some("openjdk"), Some(17)),
            install("/jvm/temurin-21", Some("temurin"), Some(21)),
        ];
        let out = resolver(&installs, None).resolve(
            Tool::Javac,
            Some(ToolSpec::Distribution {
                name: Some("temurin"),
                version: Some(21),
            }),
            None,
        );
        // Only the temurin-21 install matches; bare name is the fallback.
        assert_eq!(
            out.preferred,
            vec![PathBuf::from("/jvm/temurin-21/bin/javac")]
        );
        assert_eq!(out.fallback, PathBuf::from("javac"));
    }

    #[test]
    fn bare_version_matches_any_distribution() {
        let installs = vec![
            install("/jvm/openjdk-17", Some("openjdk"), Some(17)),
            install("/jvm/temurin-17", Some("temurin"), Some(17)),
        ];
        let out = resolver(&installs, None).resolve(
            Tool::Java,
            Some(ToolSpec::Distribution {
                name: None,
                version: Some(17),
            }),
            None,
        );
        assert_eq!(
            out.preferred,
            vec![
                PathBuf::from("/jvm/openjdk-17/bin/java"),
                PathBuf::from("/jvm/temurin-17/bin/java"),
            ]
        );
        assert_eq!(out.fallback, PathBuf::from("java"));
    }

    #[test]
    fn unmatched_distribution_falls_back_to_system() {
        let installs = vec![install("/jvm/openjdk-17", Some("openjdk"), Some(17))];
        let out = resolver(&installs, Some(Path::new("/sys/jdk"))).resolve(
            Tool::Javac,
            Some(ToolSpec::Distribution {
                name: Some("temurin"),
                version: Some(21),
            }),
            None,
        );
        // No install matches, so only the system + bare-name fallbacks remain.
        assert_eq!(out.preferred, vec![PathBuf::from("/sys/jdk/bin/javac")]);
        assert_eq!(out.fallback, PathBuf::from("javac"));
    }

    #[test]
    fn pick_probes_preferred_then_falls_back() {
        let candidates = Candidates {
            preferred: vec![PathBuf::from("/a/javac"), PathBuf::from("/b/javac")],
            fallback: PathBuf::from("javac"),
        };
        assert_eq!(
            candidates.clone().pick(|p| p == Path::new("/b/javac")),
            PathBuf::from("/b/javac")
        );
        assert_eq!(candidates.pick(|_| false), PathBuf::from("javac"));
    }

    #[test]
    fn outcome_success_derives_from_code() {
        assert!(BuildOutcome { code: Some(0) }.success());
        assert!(!BuildOutcome { code: Some(1) }.success());
        assert!(!BuildOutcome { code: None }.success());
    }

    #[test]
    fn classifies_major_version_forms() {
        let version = |name| JdkInstall::from_install_name(PathBuf::new(), name).version;
        assert_eq!(version("21.0.2-tem"), Some(21));
        assert_eq!(version("temurin-17.0.9"), Some(17));
        assert_eq!(version("java-11-openjdk-amd64"), Some(11));
        assert_eq!(version("jdk1.8.0_292"), Some(8));
        assert_eq!(version("graalvm-21"), Some(21));
        assert_eq!(version("no-digits"), None);
    }

    #[test]
    fn classifies_distribution_forms() {
        let dist = |name| JdkInstall::from_install_name(PathBuf::new(), name).distribution;
        assert_eq!(dist("21.0.2-tem"), Some("temurin".to_owned()));
        assert_eq!(dist("java-17-openjdk-amd64"), Some("openjdk".to_owned()));
        assert_eq!(dist("17.0.9-amzn"), Some("corretto".to_owned()));
        assert_eq!(dist("graalvm-community-21"), Some("graalvm".to_owned()));
        assert_eq!(dist("jdk-21.0.1"), None);
    }
}
