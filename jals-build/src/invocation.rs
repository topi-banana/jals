//! Pure construction of `javac`/`java` command lines from a [`CompileRequest`] / [`RunRequest`].
//!
//! [`Invocation::build`] and [`Invocation::run`] turn a request — a manifest plus its
//! already-resolved inputs — into an [`Invocation`]: a program name and an argument vector, without
//! touching the filesystem or spawning a process. The classpath separator is passed in by the
//! backend that plans the command (it is a command-line encoding detail, not a request input), so
//! the result is deterministic and the functions stay unit-testable with no JDK installed.

use std::path::Path;

use crate::request::{CompileRequest, RunRequest};
use crate::toolchain::Tool;

/// A resolved command line: a program plus its argument vector. Pure data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The program to run, e.g. `"javac"` or `"java"`.
    pub program: String,
    /// The arguments, in the exact order they should be passed.
    pub args: Vec<String>,
}

impl Invocation {
    /// Render a human-readable, copy-pasteable command string for `--dry-run`/`-v` output.
    ///
    /// Arguments containing whitespace are wrapped in double quotes. This is for display only and
    /// is not intended to be re-parsed by a shell.
    pub fn display_command(&self) -> String {
        let mut out = Self::quote(&self.program);
        for arg in &self.args {
            out.push(' ');
            out.push_str(&Self::quote(arg));
        }
        out
    }

    /// Join `rel` onto `root` and render it as a string for the command line.
    fn resolved(root: &Path, rel: &str) -> String {
        root.join(rel).to_string_lossy().into_owned()
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

        // Escape hatch: user flags verbatim, before the source files (which must come last).
        args.extend(build.javac_flags.iter().cloned());

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
            main_class,
            program_args,
            extra_classpath,
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

        let mut args = vec![
            "-cp".to_owned(),
            Self::join_with(&classpath, path_sep),
            main_class.to_owned(),
        ];
        args.extend(program_args.iter().cloned());

        Self {
            program: Tool::Java.binary_name().to_owned(),
            args,
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
                main_class,
                program_args,
                extra_classpath,
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
    fn extra_classpath_alone_still_emits_classpath() {
        // No manifest classpath, but extra dependency jars must still produce `-classpath`.
        let m = Manifest::default();
        let extra = vec![PathBuf::from("/abs/dep.jar")];
        let inv = build(&m, &[], &[], &extra);
        assert!(has_pair(&inv.args, "-classpath", "/abs/dep.jar"));
    }

    #[test]
    fn display_command_quotes_whitespace() {
        let inv = Invocation {
            program: "javac".to_owned(),
            args: vec!["-d".to_owned(), "/has space/out".to_owned()],
        };
        assert_eq!(inv.display_command(), "javac -d \"/has space/out\"");
    }
}
