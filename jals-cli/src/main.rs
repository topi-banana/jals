//! `jals` command-line interface.

// The only `usize`/`u32` casts here build a `FileId` from a linted file's index — bounded by the
// set of files on the command line, never approaching 2³² — so they cannot truncate in practice.
#![allow(clippy::cast_possible_truncation)]

mod report;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use jals_build::{ManifestExt, Toolchain};
use jals_config::fmt::Config;
use jals_config::lint::Config as LintConfig;
use jals_config::{FeatureSet, Manifest};
use jals_hir::{FileId, LoweredClasspath, ProjectIndex};

use report::Reporter;

#[derive(Parser)]
#[command(name = "jals", version, about = "JALS/Java tooling")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Format JALS/Java source files.
    Fmt(FmtArgs),
    /// Lint JALS/Java source files.
    Lint(LintArgs),
    /// Run the language server (LSP) over stdio.
    Lsp(LspArgs),
    /// Compile a JALS/Java project described by `jals.toml` with `javac`.
    Build(BuildArgs),
    /// Compile and run a JALS/Java project with `java`.
    Run(RunArgs),
    /// Remove a JALS/Java project's build output (the `classes-dir`).
    Clean(CleanArgs),
    /// Scaffold a new JALS/Java project (`jals.toml`, a starter `Main.java`, and `.gitignore`).
    Init(InitArgs),
}

#[derive(Args)]
struct FmtArgs {
    /// Files or directories to format. Directories are searched recursively for `.java`
    /// files. With no paths, source is read from stdin and written to stdout.
    paths: Vec<PathBuf>,

    /// Check mode: write nothing and print a diff of what would change; exit non-zero if
    /// any file would change.
    #[arg(long)]
    check: bool,

    /// Print a diff of what would change without writing, like `--check` but always exits zero.
    #[arg(long)]
    diff: bool,

    /// Deny lints (repeatable). Pass `-D warnings` to fail when any file has syntax
    /// warnings. Only `warnings` is recognized.
    #[arg(short = 'D', value_name = "LINT", action = clap::ArgAction::Append)]
    deny: Vec<String>,

    /// Use this config file instead of discovering `jalsfmt.toml`.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[derive(Args)]
struct LintArgs {
    /// Files or directories to lint. Directories are searched recursively for `.java` files.
    /// With no paths, source is read from stdin.
    paths: Vec<PathBuf>,

    /// Use this config file instead of discovering `jalslint.toml`.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[derive(Args)]
struct LspArgs {
    /// Accepted for editor compatibility; the stdio transport is always used.
    #[arg(long)]
    stdio: bool,
}

#[derive(Args)]
struct BuildArgs {
    /// Use this manifest instead of discovering `jals.toml` upward from the cwd.
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,

    /// Print the javac command that would run and exit, without compiling.
    #[arg(long)]
    dry_run: bool,

    /// Print the javac command before running it (like `cargo build -v` showing rustc).
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Override the output directory (`-d`); takes precedence over `classes-dir`.
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,

    /// Require that a `[[bin]]` with this name exists. Does not change what is compiled — `javac`
    /// always compiles all discovered sources — it only validates the name.
    #[arg(long, value_name = "NAME")]
    bin: Option<String>,
}

#[derive(Args)]
struct RunArgs {
    /// Use this manifest instead of discovering `jals.toml` upward from the cwd.
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,

    /// Print the javac/java commands that would run and exit, without compiling or running.
    #[arg(long)]
    dry_run: bool,

    /// Print the javac/java commands before running them.
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Run this fully-qualified main class instead of the resolved entry point.
    #[arg(long, value_name = "FQCN")]
    main_class: Option<String>,

    /// Run the `[[bin]]` with this name. Mutually exclusive with `--main-class`.
    #[arg(long, value_name = "NAME", conflicts_with = "main_class")]
    bin: Option<String>,

    /// Arguments passed to the program after `--`.
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Args)]
struct CleanArgs {
    /// Use this manifest instead of discovering `jals.toml` upward from the cwd.
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,

    /// Print the paths that would be removed and exit, without deleting anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct InitArgs {
    /// Directory to initialize. Created if it does not exist. Defaults to the current directory.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,

    /// Project name written to `[package] name`. Defaults to the target directory's name.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Fmt(args) => args.run(),
        Commands::Lsp(_) => LspArgs::run(),
        Commands::Lint(args) => args.run(),
        Commands::Build(args) => args.run(),
        Commands::Run(args) => args.run(),
        Commands::Clean(args) => args.run(),
        Commands::Init(args) => args.run(),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
    }
}

impl FmtArgs {
    fn run(&self) -> Result<ExitCode> {
        let deny_warnings = self.deny.iter().any(|d| d == "warnings");
        let explicit_config = self
            .config
            .as_deref()
            .map(|p| {
                Config::from_file(&jals_fs::OsFileTree, App::path_str(p)?)
                    .context("loading --config")
            })
            .transpose()?;

        // `--check` and `--diff` both render a diff and write nothing; `--check` additionally
        // fails the run. With neither, stdin is echoed to stdout and files are rewritten in place.
        let show_diff = self.check || self.diff;

        let mut discovery = Discovery::new(explicit_config);
        let mut any_changed = false;
        let mut any_warning = false;

        if self.paths.is_empty() {
            // stdin -> stdout
            let mut src = String::new();
            std::io::stdin()
                .read_to_string(&mut src)
                .context("reading stdin")?;
            let cfg =
                discovery.for_dir(&std::env::current_dir().context("getting current dir")?)?;
            let out = jals_fmt::FormatOutput::format_source(&src, &cfg);
            let changed = out.formatted != src;
            any_changed |= changed;
            any_warning |= out.has_warnings();
            Reporter::report_format_warnings("<stdin>", &src, &out);
            if show_diff {
                Reporter::print_diff("<stdin>", &src, &out.formatted);
            } else {
                std::io::stdout()
                    .write_all(out.formatted.as_bytes())
                    .context("writing stdout")?;
            }
        } else {
            for path in App::collect_java_files(&self.paths)? {
                let src = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                let parent = path.parent().unwrap_or_else(|| Path::new("."));
                let cfg = discovery.for_dir(parent)?;
                let out = jals_fmt::FormatOutput::format_source(&src, &cfg);
                let changed = out.formatted != src;
                any_changed |= changed;
                any_warning |= out.has_warnings();
                let label = path.display().to_string();
                Reporter::report_format_warnings(&label, &src, &out);

                if show_diff {
                    Reporter::print_diff(&label, &src, &out.formatted);
                } else if changed {
                    std::fs::write(&path, out.formatted.as_bytes())
                        .with_context(|| format!("writing {}", path.display()))?;
                }
            }
        }

        let fail = (self.check && any_changed) || (deny_warnings && any_warning);
        Ok(if fail {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    }
}

impl LintArgs {
    fn run(&self) -> Result<ExitCode> {
        let explicit_config = self
            .config
            .as_deref()
            .map(|p| {
                LintConfig::from_file(&jals_fs::OsFileTree, App::path_str(p)?)
                    .context("loading --config")
            })
            .transpose()?;

        let mut discovery = LintDiscovery::new(explicit_config);
        let mut any_finding = false;

        if self.paths.is_empty() {
            // stdin: a one-file "project". Building a single-file index still lets `type-mismatch`
            // see in-file project subtyping (a `Sub`/`Base` confusion), matching the multi-file path
            // below.
            let mut src = String::new();
            std::io::stdin()
                .read_to_string(&mut src)
                .context("reading stdin")?;
            let cwd = std::env::current_dir().context("getting current dir")?;
            let mut cfg = discovery.for_dir(&cwd)?;
            let parse = jals_syntax::Parse::parse(&src);
            // Fold in the project discovered from the cwd (in a single manifest parse): its classpath
            // so `type-mismatch` sees external library types, and its feature set (`[package]
            // features`) so the feature-gated rules run — exactly as the multi-file path does.
            let ctx = ProjectLintContext::load(&cwd);
            cfg.features = ctx.feature_set;
            let index = ctx.build_index(&[(FileId(0), parse.syntax())]);
            let out = jals_lint::LintOutput::lint_parse_with_index(
                &parse,
                &cfg,
                Some((&index, FileId(0))),
            );
            any_finding |= Reporter::report_lint("<stdin>", &src, &out);
        } else {
            // Read and parse every file once, then build a project-wide symbol index from the parsed
            // trees so the `type-mismatch` rule resolves reference types across files (project
            // subtyping, cross-file call arguments) — the same checks the language server runs. The
            // host owns the I/O; `ProjectIndex` itself is pure. Holding every parse at once costs more
            // memory than the old file-at-a-time pass, but is bounded by the set of files being linted.
            let mut files = Vec::new();
            for path in App::collect_java_files(&self.paths)? {
                let src = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                let parse = jals_syntax::Parse::parse(&src);
                files.push((path, src, parse));
            }
            let inputs: Vec<_> = files
                .iter()
                .enumerate()
                .map(|(i, (_, _, parse))| (FileId(i as u32), parse.syntax()))
                .collect();
            // Anchor project discovery at the first linted file's directory (walked upward for the
            // `jals.toml` in `ProjectLintContext::load` below).
            let start_dir = files
                .first()
                .and_then(|(path, _, _)| path.parent())
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            // Discover the project once: its classpath (folded into the cross-file `type-mismatch`
            // index so a method whose argument type comes from a dependency jar resolves) and its
            // feature set (`[package] features`, shared across the project's files), from a single
            // manifest parse.
            let ctx = ProjectLintContext::load(&start_dir);
            let index = ctx.build_index(&inputs);

            for (i, (path, src, parse)) in files.iter().enumerate() {
                let parent = path.parent().unwrap_or_else(|| Path::new("."));
                let mut cfg = discovery.for_dir(parent)?;
                cfg.features = ctx.feature_set;
                let out = jals_lint::LintOutput::lint_parse_with_index(
                    parse,
                    &cfg,
                    Some((&index, FileId(i as u32))),
                );
                any_finding |= Reporter::report_lint(&path.display().to_string(), src, &out);
            }
        }

        Ok(if any_finding {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    }
}

impl LspArgs {
    /// Runs the language server over stdio until the client disconnects. The parsed `--stdio` flag is
    /// accepted for editor compatibility and ignored (the stdio transport is always used).
    fn run() -> Result<ExitCode> {
        jals_lsp::Server::run()?;
        Ok(ExitCode::SUCCESS)
    }
}

impl BuildArgs {
    /// Compiles the project: discovers the manifest and sources, builds the `javac` invocation, and
    /// either prints it (`--dry-run`) or spawns `javac` and maps its exit code.
    fn run(&self) -> Result<ExitCode> {
        let (mut manifest, root) = App::resolve_manifest(self.manifest_path.as_deref())?;
        if let Some(out) = &self.out_dir {
            manifest.build.classes_dir = out.to_string_lossy().into_owned();
        }
        // `--bin` does not narrow compilation (javac compiles all sources); it only asserts the bin
        // exists, so a typo fails fast before spawning the compiler.
        if let Some(name) = &self.bin {
            jals_build::RunTarget::resolve(&manifest, Some(name)).map_err(|e| anyhow!("{e}"))?;
        }
        let sources = App::discover_sources(&manifest, &root)?;
        // Assemble the compile inputs: the resolved `[dependencies]` jars for javac's classpath, and
        // the `git`/`path` source dependencies' `.java` compiled alongside the project's own sources so
        // a project that depends on a source dependency builds. Best-effort — a failed download/clone
        // is warned and skipped, never aborting the build.
        let inputs = jals_classpath::ProjectInputs::assemble_project_inputs(
            &manifest,
            &root,
            jals_classpath::ProjectInputOptions::Compile,
            |message| eprintln!("warning: {message}"),
        );
        let request = App::compile_request(&manifest, &root, &sources, &inputs);
        // The toolchain selects `javac` from `[toolchain] compiler` (env override → discovered JDK →
        // `$JAVA_HOME` → `PATH`) and spawns it.
        let toolchain = jals_build::SubprocessToolchain::from_manifest(&manifest);

        if self.dry_run || self.verbose {
            println!("{}", toolchain.describe_compile(&request));
        }
        if self.dry_run {
            return Ok(ExitCode::SUCCESS);
        }

        let outcome = toolchain.compile(&request).map_err(|e| anyhow!("{e}"))?;
        Ok(App::outcome_exit_code(outcome))
    }
}

impl RunArgs {
    /// Compiles the project, then runs its main class with `java`. Compilation must succeed before the
    /// run; `--dry-run` prints both commands without executing either.
    fn run(&self) -> Result<ExitCode> {
        let (manifest, root) = App::resolve_manifest(self.manifest_path.as_deref())?;
        // `--main-class` overrides all manifest-based selection; otherwise resolve the entry point
        // from `[[bin]]` / `[package] default-run` / `[run] main-class`.
        let main_class: String = match &self.main_class {
            Some(explicit) => explicit.clone(),
            None => jals_build::RunTarget::resolve(&manifest, self.bin.as_deref())
                .map_err(|e| anyhow!("{e}"))?
                .to_owned(),
        };
        let sources = App::discover_sources(&manifest, &root)?;
        // Assemble the compile inputs once: the resolved `[dependencies]` jars go on both the compile
        // and run classpaths, and the `git`/`path` source dependencies' `.java` compile alongside the
        // project's own sources (their `.class` land in the run classpath's `classes-dir`, so the run
        // invocation is unchanged). Best-effort — a failed download/clone is warned and skipped.
        let inputs = jals_classpath::ProjectInputs::assemble_project_inputs(
            &manifest,
            &root,
            jals_classpath::ProjectInputOptions::Compile,
            |message| eprintln!("warning: {message}"),
        );
        let compile_request = App::compile_request(&manifest, &root, &sources, &inputs);
        let run_request = jals_build::RunRequest {
            manifest: &manifest,
            project_root: &root,
            main_class: &main_class,
            program_args: &self.args,
            extra_classpath: &inputs.dependency_jars,
        };
        // One toolchain drives both steps: `javac` from `[toolchain] compiler`, `java` from
        // `[toolchain] runtime` (each: env override → discovered JDK → `$JAVA_HOME` → `PATH`).
        let toolchain = jals_build::SubprocessToolchain::from_manifest(&manifest);

        if self.dry_run || self.verbose {
            println!("{}", toolchain.describe_compile(&compile_request));
            println!("{}", toolchain.describe_run(&run_request));
        }
        if self.dry_run {
            return Ok(ExitCode::SUCCESS);
        }

        // Compile first; only run when compilation succeeds.
        let build_outcome = toolchain
            .compile(&compile_request)
            .map_err(|e| anyhow!("{e}"))?;
        if !build_outcome.success() {
            return Ok(App::outcome_exit_code(build_outcome));
        }
        let run_outcome = toolchain.run(&run_request).map_err(|e| anyhow!("{e}"))?;
        Ok(App::outcome_exit_code(run_outcome))
    }
}

impl CleanArgs {
    /// Removes the project's build output: discovers the manifest, resolves the artifact paths, and
    /// deletes each existing directory (a missing one is simply skipped, so cleaning a never-built
    /// project succeeds quietly). `--dry-run` prints the paths without deleting them.
    fn run(&self) -> Result<ExitCode> {
        let (manifest, root) = App::resolve_manifest(self.manifest_path.as_deref())?;
        let paths = jals_build::CleanTargets::paths(&manifest, &root);

        for path in &paths {
            if self.dry_run {
                println!("would remove {}", path.display());
                continue;
            }
            if !path.exists() {
                continue;
            }
            std::fs::remove_dir_all(path)
                .with_context(|| format!("removing {}", path.display()))?;
            println!("removed {}", path.display());
        }
        Ok(ExitCode::SUCCESS)
    }
}

impl InitArgs {
    /// Scaffolds a new project: resolves the target directory and name, then writes the files from
    /// [`jals_build::InitOptions::scaffold`]. Refuses to overwrite an existing `jals.toml`; any other
    /// pre-existing scaffold file (e.g. a hand-written `Main.java`) is left untouched.
    fn run(self) -> Result<ExitCode> {
        /// Infers a project name from a target directory's final component, canonicalizing first so a
        /// relative path or `.` resolves to the directory's real name rather than the literal `.`.
        fn project_name_from_dir(dir: &Path) -> Result<String> {
            let absolute = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
            absolute
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
                .ok_or_else(|| {
                    anyhow!(
                        "could not infer a project name from {}; pass --name",
                        dir.display()
                    )
                })
        }

        let dir = match self.path {
            Some(p) => p,
            None => std::env::current_dir().context("getting current dir")?,
        };
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        if dir.join("jals.toml").exists() {
            return Err(anyhow!("`jals.toml` already exists in {}", dir.display()));
        }

        let name = match self.name {
            Some(n) => n,
            None => project_name_from_dir(&dir)?,
        };

        let files = jals_build::InitOptions { name: name.clone() }.scaffold();
        for file in &files {
            let dest = dir.join(&file.path);
            if dest.exists() {
                println!("skipping {} (already exists)", dest.display());
                continue;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&dest, file.contents.as_bytes())
                .with_context(|| format!("writing {}", dest.display()))?;
        }

        println!("created JALS project `{name}` in {}", dir.display());
        Ok(ExitCode::SUCCESS)
    }
}

/// The project context the linter folds in for the `jals.toml` discovered upward from `start_dir`:
/// its lowered classpath (so the cross-file `type-mismatch` rule resolves external library types) and
/// the resolved language feature set from `[package] features` (so feature-gated rules like
/// `compact-source-file` run). Both come from a **single** best-effort assembly of the project's
/// analysis inputs; a missing or malformed manifest yields an empty classpath and an empty feature
/// set — a malformed manifest is `jals build`'s business, not lint's.
#[derive(Default)]
struct ProjectLintContext {
    classpath: LoweredClasspath,
    feature_set: FeatureSet,
}

impl ProjectLintContext {
    fn load(start_dir: &Path) -> Self {
        let Some(manifest_path) = Manifest::discover_path(start_dir) else {
            return Self::default();
        };
        let Ok(manifest) = Manifest::from_file(&manifest_path) else {
            // A malformed manifest is the business of `jals build`; lint stays best-effort.
            return Self::default();
        };
        let root = manifest_path.parent().unwrap_or_else(|| Path::new("."));
        // Assemble the project's analysis inputs (best-effort): the classpath `.class` from the
        // `[build] classpath` plus resolved `[dependencies]` jars (folded into the cross-file
        // `type-mismatch` index) and the `[package] features`. An unreadable entry / failed download
        // is reported on stderr and skipped, never an error.
        let inputs = jals_classpath::ProjectInputs::assemble_project_inputs(
            &manifest,
            root,
            jals_classpath::ProjectInputOptions::Analysis,
            |message| eprintln!("warning: {message}"),
        );
        Self {
            classpath: ProjectIndex::lower_classpath(&inputs.classpath_classes),
            feature_set: inputs.feature_set,
        }
    }

    /// Builds a lint-time [`ProjectIndex`] over `files`, folding in the embedded stdlib stubs and this
    /// context's lowered classpath so the index-aware `type-mismatch` rule resolves stdlib and
    /// external library types. Shared by the stdin and multi-file lint paths.
    fn build_index(&self, files: &[(FileId, jals_syntax::SyntaxNode)]) -> ProjectIndex {
        ProjectIndex::builder(files)
            .with_stdlib()
            .with_classpath(&self.classpath)
            .build()
    }
}

/// Host-side helper operations for the CLI commands with no more natural home: manifest/source
/// resolution, JDK tool discovery and spawning, exit-code mapping, and `.java` file collection. A
/// stateless namespace grouping these cross-command utilities.
struct App;

impl App {
    /// Resolves the manifest from an explicit path or by discovering `jals.toml` upward from the cwd,
    /// returning the parsed manifest and the project root (the manifest's parent directory). A missing
    /// manifest is an error, unlike the formatter/linter configs.
    fn resolve_manifest(explicit: Option<&Path>) -> Result<(Manifest, PathBuf)> {
        let manifest_path = if let Some(p) = explicit {
            p.to_path_buf()
        } else {
            let cwd = std::env::current_dir().context("getting current dir")?;
            Manifest::discover_path(&cwd)
                .ok_or_else(|| anyhow!("no `jals.toml` found in {} or any parent", cwd.display()))?
        };
        let manifest = Manifest::from_file(&manifest_path)
            .with_context(|| format!("loading {}", manifest_path.display()))?;
        let root = manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Ok((manifest, root))
    }

    /// Collects the `.java` files under the manifest's source directories (resolved against `root`).
    /// Each source directory must exist, and at least one source file must be found.
    fn discover_sources(manifest: &Manifest, root: &Path) -> Result<Vec<PathBuf>> {
        let source_roots = manifest.source_roots(root);
        for dir in &source_roots {
            if !dir.is_dir() {
                return Err(anyhow!("source directory {} does not exist", dir.display()));
            }
        }
        let sources = Self::collect_java_files(&source_roots)?;
        if sources.is_empty() {
            return Err(anyhow!(
                "no .java files found under {:?}",
                manifest.build.source_dirs
            ));
        }
        Ok(sources)
    }

    /// The compile inputs shared by `jals build` and `jals run`: the manifest plus its discovered
    /// sources, with the resolved dependency jars on the classpath and the `git`/`path` source
    /// dependencies' `.java` compiled alongside — one place that wires `ProjectInputs` into a
    /// [`CompileRequest`](jals_build::CompileRequest).
    fn compile_request<'a>(
        manifest: &'a Manifest,
        project_root: &'a Path,
        sources: &'a [PathBuf],
        inputs: &'a jals_classpath::ProjectInputs,
    ) -> jals_build::CompileRequest<'a> {
        jals_build::CompileRequest {
            manifest,
            project_root,
            sources,
            extra_sources: &inputs.source_dep_sources,
            extra_classpath: &inputs.dependency_jars,
        }
    }

    /// Maps a toolchain [`BuildOutcome`](jals_build::BuildOutcome) to a CLI [`ExitCode`]: 0 succeeds,
    /// any other code propagates, and a signal-terminated process (no code) fails with code 1.
    fn outcome_exit_code(outcome: jals_build::BuildOutcome) -> ExitCode {
        match outcome.code {
            Some(0) => ExitCode::SUCCESS,
            // A `u8` exit code passes through; anything out of range (Windows codes, a signal) fails
            // as 1.
            Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
            None => ExitCode::from(1),
        }
    }

    /// Collect the files to format: explicit files as-is, directories searched recursively
    /// for `.java` files.
    fn collect_java_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
        fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
                .map(|e| e.map(|e| e.path()))
                .collect::<Result<_, _>>()?;
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    collect_dir(&path, out)?;
                } else if path.extension().is_some_and(|ext| ext == "java") {
                    out.push(path);
                }
            }
            Ok(())
        }

        let mut out = Vec::new();
        for p in paths {
            if p.is_dir() {
                collect_dir(p, &mut out)
                    .with_context(|| format!("scanning directory {}", p.display()))?;
            } else {
                out.push(p.clone());
            }
        }
        Ok(out)
    }

    /// The UTF-8 form of `path`, as required by the `jals_fs::FileTree` config-loader API. Errors on
    /// the rare non-UTF-8 host path rather than lossily converting.
    fn path_str(path: &Path) -> Result<&str> {
        path.to_str()
            .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
    }
}

/// Resolves the config for a directory, either from an explicit `--config` (used for all
/// files) or by discovering `jalsfmt.toml`, memoized per directory.
struct Discovery {
    explicit: Option<Config>,
    cache: HashMap<PathBuf, Config>,
}

impl Discovery {
    fn new(explicit: Option<Config>) -> Self {
        Self {
            explicit,
            cache: HashMap::new(),
        }
    }

    fn for_dir(&mut self, dir: &Path) -> Result<Config> {
        if let Some(cfg) = &self.explicit {
            return Ok(cfg.clone());
        }
        if let Some(cfg) = self.cache.get(dir) {
            return Ok(cfg.clone());
        }
        let cfg = Config::discover(&jals_fs::OsFileTree, App::path_str(dir)?)
            .with_context(|| format!("discovering config from {}", dir.display()))?;
        self.cache.insert(dir.to_path_buf(), cfg.clone());
        Ok(cfg)
    }
}

/// Resolves the lint config for a directory, mirroring [`Discovery`] for [`jals_lint::Config`]:
/// either from an explicit `--config` (used for all files) or by discovering `jalslint.toml`,
/// memoized per directory.
struct LintDiscovery {
    explicit: Option<LintConfig>,
    cache: HashMap<PathBuf, LintConfig>,
}

impl LintDiscovery {
    fn new(explicit: Option<LintConfig>) -> Self {
        Self {
            explicit,
            cache: HashMap::new(),
        }
    }

    fn for_dir(&mut self, dir: &Path) -> Result<LintConfig> {
        if let Some(cfg) = &self.explicit {
            return Ok(cfg.clone());
        }
        if let Some(cfg) = self.cache.get(dir) {
            return Ok(cfg.clone());
        }
        let cfg = LintConfig::discover(&jals_fs::OsFileTree, App::path_str(dir)?)
            .with_context(|| format!("discovering config from {}", dir.display()))?;
        self.cache.insert(dir.to_path_buf(), cfg.clone());
        Ok(cfg)
    }
}
