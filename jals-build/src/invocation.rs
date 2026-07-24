//! Pure construction of `javac`/`java` command lines from a [`CompileRequest`] / [`RunRequest`].
//!
//! [`Invocation::build`] and [`Invocation::run`] turn a request — a manifest plus its
//! already-resolved inputs — into an [`Invocation`]: a program name and an argument vector, without
//! touching the filesystem or spawning a process. The classpath separator is passed in by the
//! backend that plans the command (it is a command-line encoding detail, not a request input), so
//! the result is deterministic and the functions stay unit-testable with no JDK installed.
//!
//! A planned command line can outgrow what a host can spawn — one `.java` argument per source adds
//! up, and every OS caps the total. [`Invocation::needs_argument_file`] is the pure policy for that
//! (does this exceed [`MAX_COMMAND_LINE_BYTES`]?), [`Invocation::argument_file`] renders the JDK
//! `@argfile` body, and [`Invocation::with_argument_file`] rewrites the invocation to point at one.
//! Writing the file is the host's job; see `SubprocessToolchain::spill_arguments`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::request::{CompileRequest, RunRequest};
use crate::toolchain::Tool;

/// The command-line budget an [`Invocation`] is planned against, in bytes.
///
/// This is the tightest host limit, applied on every platform: Windows caps a `CreateProcessW`
/// command line at 32767 characters, while Linux allows roughly `ulimit -s / 4` in total (2 MiB by
/// default) and 128 KiB per single argument. Planning against the tightest one keeps the rule
/// platform-independent, and the argument file it selects is understood by `javac`/`java`
/// everywhere — so the only cost of being conservative is spilling a command line that one host
/// could have taken directly.
pub const MAX_COMMAND_LINE_BYTES: usize = 32_000;

/// A resolved subprocess invocation. Pure data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The program to run, e.g. `"javac"` or `"java"`.
    pub program: String,
    /// The arguments, in the exact order they should be passed.
    pub args: Vec<String>,
    /// The subprocess working directory.
    pub working_dir: PathBuf,
    /// Explicit subprocess environment entries. Unspecified variables remain inherited.
    pub environment: BTreeMap<String, String>,
}

impl Invocation {
    /// Render a human-readable command string for `--dry-run`/`-v` output.
    ///
    /// Environment entries are shown in sorted order with an `env` prefix. Words containing
    /// whitespace are wrapped in double quotes. This is display only, not a shell-escaping or
    /// executable contract.
    pub fn display_command(&self) -> String {
        let mut words = Vec::new();
        if !self.environment.is_empty() {
            words.push("env".to_owned());
            words.extend(
                self.environment
                    .iter()
                    .map(|(name, value)| Self::quote(&format!("{name}={value}"))),
            );
        }
        words.push(Self::quote(&self.program));
        for arg in &self.args {
            words.push(Self::quote(arg));
        }
        words.join(" ")
    }

    /// Resolve a manifest path for a subprocess running in `root`.
    ///
    /// Absolute roots produce absolute arguments. With a relative root, the subprocess working
    /// directory already supplies that prefix, so the argument remains relative to avoid applying
    /// the root twice.
    fn resolved(root: &Path, rel: &str) -> String {
        let path = Path::new(rel);
        if root.is_absolute() || path.is_absolute() {
            root.join(path).to_string_lossy().into_owned()
        } else {
            path.to_string_lossy().into_owned()
        }
    }

    /// Render an already-resolved path as a string for the command line.
    fn path_string(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// Join classpath-style entries with `sep`.
    fn join_with(entries: &[String], sep: char) -> String {
        entries.join(&sep.to_string())
    }

    /// Quote `s` for display when it contains whitespace.
    fn quote(s: &str) -> String {
        if s.chars().any(char::is_whitespace) {
            format!("\"{s}\"")
        } else {
            s.to_owned()
        }
    }

    /// Replace the program with a resolved path, keeping the args.
    ///
    /// [`build`](Invocation::build) / [`run`](Invocation::run) emit the *logical* program name
    /// (`"javac"` / `"java"`); the host toolchain resolves that to a concrete path (a discovered JDK,
    /// `$JAVA_HOME/bin`, …) and swaps it in with this before spawning or displaying the command.
    #[must_use]
    pub fn with_program(mut self, program: impl AsRef<Path>) -> Self {
        self.program = Self::path_string(program.as_ref());
        self
    }

    /// The byte length this invocation occupies as a host command line: the program, every
    /// argument, and one separator apiece (a space on Windows, a `NUL` terminator on Unix).
    fn command_line_len(&self) -> usize {
        self.args
            .iter()
            .fold(self.program.len() + 1, |total, arg| total + arg.len() + 1)
    }

    /// Whether this invocation is too long to spawn directly and must go through an argument file.
    ///
    /// Exceeding the host limit is not a soft failure: `execve` rejects the whole command with
    /// `E2BIG` (and `CreateProcessW` with `ERROR_INVALID_PARAMETER`), which reads exactly like a
    /// missing JDK. Detecting it here means the spawn never gets the chance.
    pub fn needs_argument_file(&self) -> bool {
        self.command_line_len() > MAX_COMMAND_LINE_BYTES
    }

    /// The `@argfile` body for this invocation's arguments — one quoted argument per line — or
    /// `None` when an argument cannot be represented in one.
    ///
    /// Every argument is double-quoted with `\` and `"` escaped, which is what the JDK's argument
    /// tokenizer round-trips (verified against `javac` 8/17/21/25): inside quotes it treats `\` as
    /// an escape, so doubling it is what carries a Windows path through intact, and whitespace and
    /// `'` lose their meaning entirely. A line terminator inside an argument is the one thing the
    /// format cannot express — the tokenizer ends a token there, quoted or not — so such an
    /// invocation reports `None` and keeps its literal command line rather than being corrupted.
    pub fn argument_file(&self) -> Option<String> {
        if self.args.iter().any(|arg| arg.contains(['\n', '\r'])) {
            return None;
        }
        let mut body = String::with_capacity(self.command_line_len());
        for arg in &self.args {
            body.push('"');
            for ch in arg.chars() {
                if ch == '\\' || ch == '"' {
                    body.push('\\');
                }
                body.push(ch);
            }
            body.push_str("\"\n");
        }
        Some(body)
    }

    /// Replace the arguments with the single `@path` reference that reads them back from an
    /// argument file the host has written from [`argument_file`](Invocation::argument_file).
    #[must_use]
    pub fn with_argument_file(mut self, path: &Path) -> Self {
        self.args = vec![format!("@{}", Self::path_string(path))];
        self
    }
}

impl Invocation {
    /// Build the `javac` invocation for a [`CompileRequest`]: the request's manifest paths are
    /// resolved against its `project_root`, exactly its `sources` are compiled (with the
    /// `extra_sources` — the `git`/`path` source dependencies' `.java` — appended after them), and
    /// the `extra_classpath` (the resolved dependency jars) follows the manifest's
    /// `[build] classpath`, joined with `path_sep`. See [`CompileRequest`] for each input's
    /// contract. The argument order is fixed so the result is stable.
    pub fn build(req: &CompileRequest<'_>, path_sep: char) -> Self {
        let &CompileRequest {
            manifest,
            project_root,
            sources,
            extra_sources,
            extra_classpath,
            extra_javac_args,
            compile_env,
        } = req;
        let build = &manifest.build;
        let mut args = Vec::new();

        // Output directory. `javac` creates it if needed, so no mkdir is required here.
        args.push("-d".to_owned());
        args.push(Self::resolved(project_root, &build.classes_dir));

        // Java version: `--release` wins; otherwise emit whichever of `--source`/`--target` are set.
        if let Some(release) = build.release {
            args.push("--release".to_owned());
            args.push(release.to_string());
        } else {
            if let Some(source) = build.source {
                args.push("--source".to_owned());
                args.push(source.to_string());
            }
            if let Some(target) = build.target {
                args.push("--target".to_owned());
                args.push(target.to_string());
            }
        }

        // Classpath: manifest entries (resolved against the root) followed by the host's already-resolved
        // extra entries (e.g. downloaded dependency jars). Emitted only when at least one is present.
        let mut classpath: Vec<String> = build
            .classpath
            .iter()
            .map(|e| Self::resolved(project_root, e))
            .collect();
        classpath.extend(extra_classpath.iter().map(|p| Self::path_string(p)));
        if !classpath.is_empty() {
            args.push("-classpath".to_owned());
            args.push(Self::join_with(&classpath, path_sep));
        }

        // Source path: where `javac` looks for referenced-but-unlisted sources.
        if !build.source_dirs.is_empty() {
            let source_path: Vec<String> = build
                .source_dirs
                .iter()
                .map(|d| Self::resolved(project_root, d))
                .collect();
            args.push("-sourcepath".to_owned());
            args.push(Self::join_with(&source_path, path_sep));
        }

        // Manifest flags precede accepted build-script flags; all flags remain before sources.
        args.extend(build.javac_flags.iter().cloned());
        args.extend(extra_javac_args.iter().cloned());

        // The source files, in the given order: the project's own sources, then any source-dependency
        // (`git`/`path`) `.java` compiled alongside them (order is irrelevant to `javac`).
        args.extend(
            sources
                .iter()
                .chain(extra_sources.iter())
                .map(|p| Self::path_string(p)),
        );

        Self {
            program: Tool::Javac.binary_name().to_owned(),
            args,
            working_dir: project_root.to_path_buf(),
            environment: BTreeMap::clone(compile_env),
        }
    }

    /// Build the `java` invocation that runs a [`RunRequest`]'s `main_class` against the compiled
    /// classes.
    ///
    /// The classpath is the project's `classes_dir`, then the manifest's `classpath` entries
    /// (resolved against the request's `project_root`), then its `extra_classpath` (the
    /// already-resolved dependency jars), joined with `path_sep`. The request's `program_args` are
    /// passed to the program after the main class.
    pub fn run(req: &RunRequest<'_>, path_sep: char) -> Self {
        let &RunRequest {
            manifest,
            project_root,
            jvm_args,
            main_class,
            program_args,
            extra_classpath,
            run_env,
        } = req;
        let build = &manifest.build;

        let mut classpath = vec![Self::resolved(project_root, &build.classes_dir)];
        classpath.extend(
            build
                .classpath
                .iter()
                .map(|e| Self::resolved(project_root, e)),
        );
        classpath.extend(extra_classpath.iter().map(|p| Self::path_string(p)));

        let mut args = jvm_args.to_vec();
        args.extend([
            "-cp".to_owned(),
            Self::join_with(&classpath, path_sep),
            main_class.to_owned(),
        ]);
        args.extend(program_args.iter().cloned());

        Self {
            program: Tool::Java.binary_name().to_owned(),
            args,
            working_dir: project_root.to_path_buf(),
            environment: BTreeMap::clone(run_env),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use jals_config::Manifest;

    const ROOT: &str = "/proj";

    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    fn build(
        manifest: &Manifest,
        sources: &[PathBuf],
        extra_sources: &[PathBuf],
        extra_classpath: &[PathBuf],
    ) -> Invocation {
        Invocation::build(
            &CompileRequest {
                manifest,
                project_root: Path::new(ROOT),
                sources,
                extra_sources,
                extra_classpath,
                extra_javac_args: &[],
                compile_env: &BTreeMap::new(),
            },
            ':',
        )
    }

    fn run(
        manifest: &Manifest,
        main_class: &str,
        program_args: &[String],
        extra_classpath: &[PathBuf],
    ) -> Invocation {
        Invocation::run(
            &RunRequest {
                manifest,
                project_root: Path::new(ROOT),
                jvm_args: &[],
                main_class,
                program_args,
                extra_classpath,
                run_env: &BTreeMap::new(),
            },
            ':',
        )
    }

    #[test]
    fn release_emits_release_and_omits_source_target() {
        let mut m = Manifest::default();
        m.build.release = Some(21);
        m.build.source = Some(8);
        m.build.target = Some(8);
        let inv = build(&m, &[], &[], &[]);
        assert!(has_pair(&inv.args, "--release", "21"));
        assert!(!inv.args.iter().any(|a| a == "--source"));
        assert!(!inv.args.iter().any(|a| a == "--target"));
    }

    #[test]
    fn source_target_used_when_no_release() {
        let mut m = Manifest::default();
        m.build.source = Some(17);
        m.build.target = Some(11);
        let inv = build(&m, &[], &[], &[]);
        assert!(!inv.args.iter().any(|a| a == "--release"));
        assert!(has_pair(&inv.args, "--source", "17"));
        assert!(has_pair(&inv.args, "--target", "11"));
    }

    #[test]
    fn classpath_joined_with_separator_and_omitted_when_empty() {
        let mut m = Manifest::default();
        let inv = build(&m, &[], &[], &[]);
        assert!(!inv.args.iter().any(|a| a == "-classpath"));

        m.build.classpath = vec!["a.jar".to_owned(), "b".to_owned()];
        let inv = build(&m, &[], &[], &[]);
        assert!(has_pair(&inv.args, "-classpath", "/proj/a.jar:/proj/b"));
    }

    #[test]
    fn sources_come_last_in_order() {
        let m = Manifest::default();
        let sources = vec![
            PathBuf::from("/proj/src/main/java/A.java"),
            PathBuf::from("/proj/src/main/java/B.java"),
        ];
        let inv = build(&m, &sources, &[], &[]);
        let n = inv.args.len();
        assert_eq!(inv.args[n - 2], "/proj/src/main/java/A.java");
        assert_eq!(inv.args[n - 1], "/proj/src/main/java/B.java");
    }

    #[test]
    fn extra_sources_appended_after_project_sources() {
        // Source-dependency (`git`/`path`) `.java` are compiled alongside the project's own sources,
        // appended after them (source-file order is irrelevant to `javac`).
        let m = Manifest::default();
        let sources = vec![PathBuf::from("/proj/src/main/java/A.java")];
        let extra_sources = vec![
            PathBuf::from("/proj/target/jals/deps/git/mylib-abc/src/main/java/Lib.java"),
            PathBuf::from("/other/local-lib/src/Helper.java"),
        ];
        let inv = build(&m, &sources, &extra_sources, &[]);
        let n = inv.args.len();
        assert_eq!(inv.args[n - 3], "/proj/src/main/java/A.java");
        assert_eq!(
            inv.args[n - 2],
            "/proj/target/jals/deps/git/mylib-abc/src/main/java/Lib.java"
        );
        assert_eq!(inv.args[n - 1], "/other/local-lib/src/Helper.java");
    }

    #[test]
    fn javac_flags_passed_verbatim_before_sources() {
        let mut m = Manifest::default();
        m.build.javac_flags = vec!["-Xlint:all".to_owned(), "-g".to_owned()];
        let sources = vec![PathBuf::from("/proj/src/main/java/A.java")];
        let inv = build(&m, &sources, &[], &[]);
        let lint = inv.args.iter().position(|a| a == "-Xlint:all").unwrap();
        let src = inv
            .args
            .iter()
            .position(|a| a == "/proj/src/main/java/A.java")
            .unwrap();
        assert!(lint < src);
    }

    #[test]
    fn extra_javac_args_follow_manifest_flags_and_precede_sources() {
        let mut manifest = Manifest::default();
        manifest.build.javac_flags = vec!["-Xlint:all".to_owned()];
        let sources = vec![PathBuf::from("/proj/src/main/java/A.java")];
        let extra_javac_args = vec!["-Afirst=1".to_owned(), "-Asecond=2".to_owned()];
        let invocation = Invocation::build(
            &CompileRequest {
                manifest: &manifest,
                project_root: Path::new(ROOT),
                sources: &sources,
                extra_sources: &[],
                extra_classpath: &[],
                extra_javac_args: &extra_javac_args,
                compile_env: &BTreeMap::new(),
            },
            ':',
        );

        let tail = invocation
            .args
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert!(tail.ends_with(&[
            "-Xlint:all",
            "-Afirst=1",
            "-Asecond=2",
            "/proj/src/main/java/A.java",
        ]));
    }

    #[test]
    fn full_javac_command_snapshot() {
        let mut m = Manifest::default();
        m.build.release = Some(21);
        m.build.classpath = vec!["libs/guava.jar".to_owned(), "libs/extra".to_owned()];
        m.build.javac_flags = vec!["-Xlint:all".to_owned()];
        let sources = vec![
            PathBuf::from("/proj/src/main/java/com/example/A.java"),
            PathBuf::from("/proj/src/main/java/com/example/B.java"),
        ];
        let inv = build(&m, &sources, &[], &[]);
        assert_eq!(inv.program, "javac");
        assert_eq!(
            inv.args.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "-d",
                "/proj/target/classes",
                "--release",
                "21",
                "-classpath",
                "/proj/libs/guava.jar:/proj/libs/extra",
                "-sourcepath",
                "/proj/src/main/java",
                "-Xlint:all",
                "/proj/src/main/java/com/example/A.java",
                "/proj/src/main/java/com/example/B.java",
            ]
        );
    }

    #[test]
    fn run_invocation_prepends_classes_dir_to_classpath() {
        let mut m = Manifest::default();
        m.build.classpath = vec!["libs/x.jar".to_owned()];
        let inv = run(&m, "com.example.Main", &["arg1".to_owned()], &[]);
        assert_eq!(inv.program, "java");
        assert_eq!(
            inv.args.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "-cp",
                "/proj/target/classes:/proj/libs/x.jar",
                "com.example.Main",
                "arg1",
            ]
        );
    }

    #[test]
    fn jvm_args_precede_classpath_and_keep_their_order() {
        let manifest = Manifest::default();
        let jvm_args = vec!["-ea".to_owned(), "-Dmode=test".to_owned()];
        let invocation = Invocation::run(
            &RunRequest {
                manifest: &manifest,
                project_root: Path::new(ROOT),
                jvm_args: &jvm_args,
                main_class: "com.example.Main",
                program_args: &[],
                extra_classpath: &[],
                run_env: &BTreeMap::new(),
            },
            ':',
        );

        assert_eq!(
            invocation
                .args
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "-ea",
                "-Dmode=test",
                "-cp",
                "/proj/target/classes",
                "com.example.Main",
            ]
        );
    }

    #[test]
    fn invocation_carries_project_root_and_phase_environment() {
        let manifest = Manifest::default();
        let compile_env = BTreeMap::from([
            ("LANG".to_owned(), "C".to_owned()),
            ("MODE".to_owned(), "compile".to_owned()),
        ]);
        let run_env = BTreeMap::from([("MODE".to_owned(), "run".to_owned())]);

        let compile = Invocation::build(
            &CompileRequest {
                manifest: &manifest,
                project_root: Path::new(ROOT),
                sources: &[],
                extra_sources: &[],
                extra_classpath: &[],
                extra_javac_args: &[],
                compile_env: &compile_env,
            },
            ':',
        );
        let run = Invocation::run(
            &RunRequest {
                manifest: &manifest,
                project_root: Path::new(ROOT),
                jvm_args: &[],
                main_class: "Main",
                program_args: &[],
                extra_classpath: &[],
                run_env: &run_env,
            },
            ':',
        );

        assert_eq!(compile.working_dir, PathBuf::from(ROOT));
        assert_eq!(compile.environment, compile_env);
        assert_eq!(run.working_dir, PathBuf::from(ROOT));
        assert_eq!(run.environment, run_env);
    }

    #[test]
    fn extra_classpath_appended_after_manifest_classpath() {
        let mut m = Manifest::default();
        m.build.classpath = vec!["libs/guava.jar".to_owned()];
        let extra = vec![
            PathBuf::from("/proj/target/jals/deps/dep.jar"),
            PathBuf::from("/abs/other.jar"),
        ];

        // build: manifest classpath first, then the resolved extra jars (verbatim, not re-rooted).
        let inv = build(&m, &[], &[], &extra);
        assert!(has_pair(
            &inv.args,
            "-classpath",
            "/proj/libs/guava.jar:/proj/target/jals/deps/dep.jar:/abs/other.jar",
        ));

        // run: classes-dir, then manifest classpath, then the extra jars.
        let inv = run(&m, "com.example.Main", &[], &extra);
        assert!(has_pair(
            &inv.args,
            "-cp",
            "/proj/target/classes:/proj/libs/guava.jar:/proj/target/jals/deps/dep.jar:/abs/other.jar",
        ));
    }

    #[test]
    fn relative_project_root_is_not_resolved_twice_under_working_directory() {
        let mut manifest = Manifest::default();
        manifest.build.classpath = vec!["libs/api.jar".to_owned()];
        let source = PathBuf::from("/workspace/project/src/main/java/Main.java");
        let compile = Invocation::build(
            &CompileRequest {
                manifest: &manifest,
                project_root: Path::new("project"),
                sources: std::slice::from_ref(&source),
                extra_sources: &[],
                extra_classpath: &[],
                extra_javac_args: &[],
                compile_env: &BTreeMap::new(),
            },
            ':',
        );

        assert_eq!(compile.working_dir, PathBuf::from("project"));
        assert!(has_pair(&compile.args, "-d", "target/classes"));
        assert!(has_pair(&compile.args, "-classpath", "libs/api.jar"));
        assert!(has_pair(&compile.args, "-sourcepath", "src/main/java"));
        assert_eq!(
            compile.args.last().map(String::as_str),
            Some("/workspace/project/src/main/java/Main.java")
        );

        let run = Invocation::run(
            &RunRequest {
                manifest: &manifest,
                project_root: Path::new("project"),
                jvm_args: &[],
                main_class: "Main",
                program_args: &[],
                extra_classpath: &[],
                run_env: &BTreeMap::new(),
            },
            ':',
        );
        assert_eq!(run.working_dir, PathBuf::from("project"));
        assert!(has_pair(&run.args, "-cp", "target/classes:libs/api.jar"));
    }

    #[test]
    fn extra_classpath_alone_still_emits_classpath() {
        // No manifest classpath, but extra dependency jars must still produce `-classpath`.
        let m = Manifest::default();
        let extra = vec![PathBuf::from("/abs/dep.jar")];
        let inv = build(&m, &[], &[], &extra);
        assert!(has_pair(&inv.args, "-classpath", "/abs/dep.jar"));
    }

    #[test]
    fn argument_file_needed_only_past_the_command_line_budget() {
        let mut invocation = Invocation {
            program: "javac".to_owned(),
            args: Vec::new(),
            working_dir: PathBuf::from(ROOT),
            environment: BTreeMap::new(),
        };
        assert!(!invocation.needs_argument_file());

        // Straddle the budget with one argument, so the boundary itself is exercised rather than
        // some comfortably-over case.
        let mut arg = "a".repeat(MAX_COMMAND_LINE_BYTES - invocation.command_line_len() - 1);
        invocation.args = vec![arg.clone()];
        assert_eq!(invocation.command_line_len(), MAX_COMMAND_LINE_BYTES);
        assert!(!invocation.needs_argument_file());

        arg.push('a');
        invocation.args = vec![arg];
        assert!(invocation.needs_argument_file());
    }

    #[test]
    fn argument_file_quotes_every_argument_and_escapes_backslashes() {
        // The escaping the JDK tokenizer round-trips: whitespace and `'` are neutralized by the
        // quotes, `\` and `"` are escaped. A Windows path is the case a Unix-only test would miss.
        let invocation = Invocation {
            program: "javac".to_owned(),
            args: vec![
                "-d".to_owned(),
                r"C:\Users\dev\My Project\target\classes".to_owned(),
                "quote'dir/A.java".to_owned(),
                r#"say "hi".java"#.to_owned(),
            ],
            working_dir: PathBuf::from(ROOT),
            environment: BTreeMap::new(),
        };

        assert_eq!(
            invocation.argument_file().unwrap(),
            concat!(
                "\"-d\"\n",
                r#""C:\\Users\\dev\\My Project\\target\\classes""#,
                "\n",
                "\"quote'dir/A.java\"\n",
                r#""say \"hi\".java""#,
                "\n",
            )
        );
    }

    #[test]
    fn argument_containing_a_line_terminator_has_no_argument_file() {
        // The format ends a token at a newline even inside quotes, so there is no encoding of this
        // argument — reporting it keeps the host on the literal command line instead of silently
        // compiling something else.
        let invocation = Invocation {
            program: "javac".to_owned(),
            args: vec!["/proj/src/two\nlines.java".to_owned()],
            working_dir: PathBuf::from(ROOT),
            environment: BTreeMap::new(),
        };
        assert!(invocation.argument_file().is_none());
    }

    #[test]
    fn with_argument_file_replaces_every_argument() {
        let invocation = Invocation {
            program: "javac".to_owned(),
            args: vec!["-d".to_owned(), "target/classes".to_owned()],
            working_dir: PathBuf::from(ROOT),
            environment: BTreeMap::from([("LANG".to_owned(), "C".to_owned())]),
        };

        let referenced = invocation
            .clone()
            .with_argument_file(Path::new("/proj/target/jals/build/javac-args"));
        assert_eq!(referenced.args, vec!["@/proj/target/jals/build/javac-args"]);
        // Only the arguments move: the program, working directory, and environment still decide
        // what runs and where.
        assert_eq!(referenced.program, invocation.program);
        assert_eq!(referenced.working_dir, invocation.working_dir);
        assert_eq!(referenced.environment, invocation.environment);
    }

    #[test]
    fn display_command_quotes_whitespace() {
        let inv = Invocation {
            program: "javac".to_owned(),
            args: vec!["-d".to_owned(), "/has space/out".to_owned()],
            working_dir: PathBuf::from(ROOT),
            environment: BTreeMap::from([
                ("Z_LAST".to_owned(), "value".to_owned()),
                ("A_FIRST".to_owned(), "has space".to_owned()),
            ]),
        };
        assert_eq!(
            inv.display_command(),
            "env \"A_FIRST=has space\" Z_LAST=value javac -d \"/has space/out\""
        );
    }
}
