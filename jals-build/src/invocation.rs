//! Pure construction of `javac`/`java` command lines from a [`Manifest`].
//!
//! [`build_invocation`] and [`run_invocation`] turn a manifest plus already-resolved inputs into an
//! [`Invocation`] — a program name and an argument vector — without touching the filesystem or
//! spawning a process. The path separator is injected so the result is deterministic and
//! host-independent, which keeps the functions unit-testable with no JDK installed.

use std::path::{Path, PathBuf};

use jals_config::Manifest;

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
            s.to_string()
        }
    }
}

impl Invocation {
    /// Build the `javac` invocation for `manifest`, resolving all paths against `project_root` and
    /// compiling exactly `sources` (already-discovered `.java` files, in the order to pass them).
    ///
    /// `extra_sources` are additional already-resolved absolute `.java` paths compiled alongside
    /// `sources` (appended after them, since source-file order is irrelevant to `javac`) — the host
    /// passes the `.java` it resolved from `git`/`path` source `[dependencies]` here, so a project that
    /// depends on a source dependency compiles against it. Their `.class` land in the same
    /// `[build] classes-dir` as the project's own output.
    ///
    /// `extra_classpath` are already-resolved absolute classpath entries appended after the manifest's
    /// `[build] classpath` — the host passes the local jars it resolved from `[dependencies]` here, so
    /// `javac` sees external library types. `path_sep` is the platform classpath separator (`':'` on
    /// Unix, `';'` on Windows); injecting it keeps the function pure and deterministic. The argument
    /// order is fixed so the result is stable.
    pub fn build(
        manifest: &Manifest,
        project_root: &Path,
        sources: &[PathBuf],
        extra_sources: &[PathBuf],
        extra_classpath: &[PathBuf],
        path_sep: char,
    ) -> Self {
        let build = &manifest.build;
        let mut args = Vec::new();

        // Output directory. `javac` creates it if needed, so no mkdir is required here.
        args.push("-d".to_string());
        args.push(Self::resolved(project_root, &build.classes_dir));

        // Java version: `--release` wins; otherwise emit whichever of `--source`/`--target` are set.
        if let Some(release) = build.release {
            args.push("--release".to_string());
            args.push(release.to_string());
        } else {
            if let Some(source) = build.source {
                args.push("--source".to_string());
                args.push(source.to_string());
            }
            if let Some(target) = build.target {
                args.push("--target".to_string());
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
            args.push("-classpath".to_string());
            args.push(Self::join_with(&classpath, path_sep));
        }

        // Source path: where `javac` looks for referenced-but-unlisted sources.
        if !build.source_dirs.is_empty() {
            let source_path: Vec<String> = build
                .source_dirs
                .iter()
                .map(|d| Self::resolved(project_root, d))
                .collect();
            args.push("-sourcepath".to_string());
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
            program: "javac".to_string(),
            args,
        }
    }

    /// Build the `java` invocation that runs `main_class` against the compiled classes.
    ///
    /// The classpath is the project's `classes_dir`, then the manifest's `classpath` entries (resolved
    /// against `project_root`), then `extra_classpath` (the host's already-resolved dependency jars),
    /// joined with `path_sep`. `program_args` are passed to the program after the main class.
    pub fn run(
        manifest: &Manifest,
        project_root: &Path,
        main_class: &str,
        program_args: &[String],
        extra_classpath: &[PathBuf],
        path_sep: char,
    ) -> Self {
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
            "-cp".to_string(),
            Self::join_with(&classpath, path_sep),
            main_class.to_string(),
        ];
        args.extend(program_args.iter().cloned());

        Self {
            program: "java".to_string(),
            args,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jals_config::Manifest;

    const ROOT: &str = "/proj";

    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn release_emits_release_and_omits_source_target() {
        let mut m = Manifest::default();
        m.build.release = Some(21);
        m.build.source = Some(8);
        m.build.target = Some(8);
        let inv = Invocation::build(&m, Path::new(ROOT), &[], &[], &[], ':');
        assert!(has_pair(&inv.args, "--release", "21"));
        assert!(!inv.args.iter().any(|a| a == "--source"));
        assert!(!inv.args.iter().any(|a| a == "--target"));
    }

    #[test]
    fn source_target_used_when_no_release() {
        let mut m = Manifest::default();
        m.build.source = Some(17);
        m.build.target = Some(11);
        let inv = Invocation::build(&m, Path::new(ROOT), &[], &[], &[], ':');
        assert!(!inv.args.iter().any(|a| a == "--release"));
        assert!(has_pair(&inv.args, "--source", "17"));
        assert!(has_pair(&inv.args, "--target", "11"));
    }

    #[test]
    fn classpath_joined_with_separator_and_omitted_when_empty() {
        let mut m = Manifest::default();
        let inv = Invocation::build(&m, Path::new(ROOT), &[], &[], &[], ':');
        assert!(!inv.args.iter().any(|a| a == "-classpath"));

        m.build.classpath = vec!["a.jar".to_string(), "b".to_string()];
        let inv = Invocation::build(&m, Path::new(ROOT), &[], &[], &[], ':');
        assert!(has_pair(&inv.args, "-classpath", "/proj/a.jar:/proj/b"));
    }

    #[test]
    fn sources_come_last_in_order() {
        let m = Manifest::default();
        let sources = vec![
            PathBuf::from("/proj/src/main/java/A.java"),
            PathBuf::from("/proj/src/main/java/B.java"),
        ];
        let inv = Invocation::build(&m, Path::new(ROOT), &sources, &[], &[], ':');
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
        let inv = Invocation::build(&m, Path::new(ROOT), &sources, &extra_sources, &[], ':');
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
        m.build.javac_flags = vec!["-Xlint:all".to_string(), "-g".to_string()];
        let sources = vec![PathBuf::from("/proj/src/main/java/A.java")];
        let inv = Invocation::build(&m, Path::new(ROOT), &sources, &[], &[], ':');
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
        m.build.classpath = vec!["libs/guava.jar".to_string(), "libs/extra".to_string()];
        m.build.javac_flags = vec!["-Xlint:all".to_string()];
        let sources = vec![
            PathBuf::from("/proj/src/main/java/com/example/A.java"),
            PathBuf::from("/proj/src/main/java/com/example/B.java"),
        ];
        let inv = Invocation::build(&m, Path::new(ROOT), &sources, &[], &[], ':');
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
        m.build.classpath = vec!["libs/x.jar".to_string()];
        let inv = Invocation::run(
            &m,
            Path::new(ROOT),
            "com.example.Main",
            &["arg1".to_string()],
            &[],
            ':',
        );
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
        m.build.classpath = vec!["libs/guava.jar".to_string()];
        let extra = vec![
            PathBuf::from("/proj/target/jals/deps/dep.jar"),
            PathBuf::from("/abs/other.jar"),
        ];

        // build: manifest classpath first, then the resolved extra jars (verbatim, not re-rooted).
        let inv = Invocation::build(&m, Path::new(ROOT), &[], &[], &extra, ':');
        assert!(has_pair(
            &inv.args,
            "-classpath",
            "/proj/libs/guava.jar:/proj/target/jals/deps/dep.jar:/abs/other.jar",
        ));

        // run: classes-dir, then manifest classpath, then the extra jars.
        let inv = Invocation::run(&m, Path::new(ROOT), "com.example.Main", &[], &extra, ':');
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
        let inv = Invocation::build(&m, Path::new(ROOT), &[], &[], &extra, ':');
        assert!(has_pair(&inv.args, "-classpath", "/abs/dep.jar"));
    }

    #[test]
    fn display_command_quotes_whitespace() {
        let inv = Invocation {
            program: "javac".to_string(),
            args: vec!["-d".to_string(), "/has space/out".to_string()],
        };
        assert_eq!(inv.display_command(), "javac -d \"/has space/out\"");
    }
}
