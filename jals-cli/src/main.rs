//! `jals` command-line interface.

// The only `usize`/`u32` casts here build a `FileId` from a linted file's index — bounded by the
// set of files on the command line, never approaching 2³² — so they cannot truncate in practice.
#![allow(clippy::cast_possible_truncation)]

mod report;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use jals_build::build_script::{
    BuildScriptDiagnostic, BuildScriptEnvironment, BuildScriptLimits, BuildScriptSession,
};
use jals_build::{Compiler, ManifestExt, Runtime};
use jals_config::fmt::Config;
use jals_config::lint::Config as LintConfig;
use jals_config::{DiscoverableConfig, FeatureSet, Manifest, ResolvedBuildFeatures};
use jals_exec::Exec;
use jals_hir::{FileId, LoweredClasspath, ProjectIndex};
use jals_storage::{FileKey, NativeScope, NativeStorage, RelativePath};

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
    /// Remove a project's `classes-dir` and reserved build-script outputs.
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

/// Cargo-style build-feature selection, shared by `build` and `run`.
///
/// `[features]` declares the features; these flags choose which are active for one
/// invocation. Selection is additive — a feature never subtracts — so `--features client` keeps the
/// `default` list unless `--no-default-features` is also given.
#[derive(Args)]
struct FeatureArgs {
    /// Activate these `[features]` (comma separated, repeatable). A `<dependency>/<feature>` entry
    /// activates a feature in that dependency instead of this project.
    #[arg(long, value_name = "FEATURES", value_delimiter = ',')]
    features: Vec<String>,

    /// Activate every declared `[features]`. Takes precedence over `--no-default-features`.
    #[arg(long)]
    all_features: bool,

    /// Do not activate the `default` `[features]` list.
    #[arg(long)]
    no_default_features: bool,
}

impl FeatureArgs {
    /// The build features these flags select from `manifest`: the root project's own sorted set,
    /// plus what its `[features]` forwards to each dependency.
    fn resolve(&self, manifest: &Manifest) -> Result<ResolvedBuildFeatures> {
        manifest
            .resolve_build_features(&self.features, self.all_features, self.no_default_features)
            .map_err(|e| anyhow!("{e}"))
    }
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

    /// Resolve build-task artifacts only from the verified project cache.
    #[arg(long)]
    offline: bool,

    #[command(flatten)]
    features: FeatureArgs,
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

    /// Resolve build-task artifacts only from the verified project cache.
    #[arg(long)]
    offline: bool,

    #[command(flatten)]
    features: FeatureArgs,
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
    // One current-thread runtime + LocalSet for the whole invocation; every command runs async
    // on it, and `jals lsp` serves inside it rather than nesting a second runtime.
    let result = jals_exec::tokio_rt::run(|exec| async move {
        match cli.command {
            Commands::Fmt(args) => args.run(&exec).await,
            Commands::Lsp(_) => LspArgs::run(exec).await,
            Commands::Lint(args) => args.run(&exec).await,
            Commands::Build(args) => args.run(&exec).await,
            Commands::Run(args) => args.run(&exec).await,
            Commands::Clean(args) => args.run(&exec).await,
            Commands::Init(args) => args.run(&exec).await,
        }
    });
    match result {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => {
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
        Err(err) => {
            eprintln!("error: failed to start the runtime: {err}");
            ExitCode::from(1)
        }
    }
}

impl FmtArgs {
    async fn run(&self, exec: &Exec) -> Result<ExitCode> {
        let deny_warnings = self.deny.iter().any(|d| d == "warnings");
        let explicit_config = App::load_explicit::<Config>(self.config.as_deref())?;

        // `--check` and `--diff` both render a diff and write nothing; `--check` additionally
        // fails the run. With neither, stdin is echoed to stdout and files are rewritten in place.
        let show_diff = self.check || self.diff;

        let mut discovery = HostConfigs::new(explicit_config);
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
            let out = jals_fmt::FormatOutput::format_source(&src, &cfg).await;
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
            // Discover paths before opening storage, then snapshot exactly those files. Overlapping
            // targets are deduplicated and files sharing a root commit in one transaction.
            let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
            for target in &self.paths {
                let root = if target.is_dir() {
                    target.clone()
                } else {
                    target
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .to_path_buf()
                };
                groups
                    .entry(root)
                    .or_default()
                    .extend(App::collect_java_files(std::slice::from_ref(target))?);
            }
            for (root, mut paths) in groups {
                paths.sort();
                paths.dedup();
                let keyed: Vec<_> = paths
                    .into_iter()
                    .map(|path| {
                        let key = RelativePath::from_host_path(&root, &path)
                            .and_then(|relative| FileKey::new(relative).ok())
                            .ok_or_else(|| {
                                anyhow!(
                                    "source path is not addressable under {}: {}",
                                    root.display(),
                                    path.display()
                                )
                            })?;
                        Ok::<_, anyhow::Error>((path, key))
                    })
                    .collect::<Result<_>>()?;
                let scopes = keyed
                    .iter()
                    .map(|(_, key)| NativeScope::all(key.path().clone()));
                let mut storage =
                    NativeStorage::for_project_scoped(&root, scopes, exec.clone()).await?;
                let mut edits = Vec::new();
                for (path, key) in keyed {
                    let src = storage
                        .view()
                        .file(&key)?
                        .text()
                        .map_err(|_| anyhow!("source is not valid UTF-8: {}", path.display()))?
                        .to_owned();
                    let cfg = discovery.for_dir(path.parent().unwrap_or_else(|| Path::new(".")))?;
                    let out = jals_fmt::FormatOutput::format_source(&src, &cfg).await;
                    let changed = out.formatted != src;
                    any_changed |= changed;
                    any_warning |= out.has_warnings();
                    let label = path.display().to_string();
                    Reporter::report_format_warnings(&label, &src, &out);

                    if show_diff {
                        Reporter::print_diff(&label, &src, &out.formatted);
                    } else if changed {
                        edits.push((key, out.formatted.into_bytes()));
                    }
                }
                Self::commit_edits(&mut storage, edits).await?;
            }
        }

        let fail = (self.check && any_changed) || (deny_warnings && any_warning);
        Ok(if fail {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    }

    /// Commit the staged rewrites against one aggregate in a single transaction (a no-op when
    /// nothing changed), so a sweep publishes one revision and a failure writes nothing.
    async fn commit_edits(
        storage: &mut NativeStorage,
        edits: Vec<(FileKey, Vec<u8>)>,
    ) -> Result<()> {
        if edits.is_empty() {
            return Ok(());
        }
        let mut transaction = storage.transaction(storage.revision())?;
        for (key, bytes) in edits {
            transaction.replace_file(key, bytes)?;
        }
        transaction.commit().await?;
        Ok(())
    }
}

impl LintArgs {
    async fn run(&self, exec: &Exec) -> Result<ExitCode> {
        let explicit_config = App::load_explicit::<LintConfig>(self.config.as_deref())?;

        let mut discovery = HostConfigs::new(explicit_config);
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
            let parse = jals_syntax::Parse::parse(&src).await;
            // Fold in the project discovered from the cwd (in a single manifest parse): its classpath
            // so `type-mismatch` sees external library types, and its feature set (`[package]
            // features`) so the feature-gated rules run — exactly as the multi-file path does.
            let ctx = ProjectLintContext::load(&cwd, exec).await;
            cfg.features = ctx.feature_set;
            let index = ctx.build_index(&[(FileId(0), parse.syntax())]).await;
            let out = jals_lint::LintOutput::lint_parse_with_index(
                &parse,
                &cfg,
                Some((&index, FileId(0))),
            )
            .await;
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
                let parse = jals_syntax::Parse::parse(&src).await;
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
            let ctx = ProjectLintContext::load(&start_dir, exec).await;
            let index = ctx.build_index(&inputs).await;

            for (i, (path, src, parse)) in files.iter().enumerate() {
                let parent = path.parent().unwrap_or_else(|| Path::new("."));
                let mut cfg = discovery.for_dir(parent)?;
                cfg.features = ctx.feature_set;
                let out = jals_lint::LintOutput::lint_parse_with_index(
                    parse,
                    &cfg,
                    Some((&index, FileId(i as u32))),
                )
                .await;
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
    /// accepted for editor compatibility and ignored (the stdio transport is always used). Serves
    /// inside the CLI's own runtime — no nested runtime.
    async fn run(exec: Exec) -> Result<ExitCode> {
        jals_lsp::Server::serve(exec).await?;
        Ok(ExitCode::SUCCESS)
    }
}

impl BuildArgs {
    /// Compiles the project: discovers the manifest and sources, builds the `javac` invocation, and
    /// either prints it (`--dry-run`) or spawns `javac` and maps its exit code.
    async fn run(&self, exec: &Exec) -> Result<ExitCode> {
        let (mut manifest, root) = App::resolve_manifest(self.manifest_path.as_deref()).await?;
        let features = self.features.resolve(&manifest)?;
        if let Some(out) = &self.out_dir {
            manifest.build.classes_dir = out.to_string_lossy().into_owned();
        }
        // `--bin` does not narrow compilation (javac compiles all sources); it only asserts the bin
        // exists, so a typo fails fast before spawning the compiler.
        if let Some(name) = &self.bin {
            jals_build::RunTarget::resolve(&manifest, Some(name)).map_err(|e| anyhow!("{e}"))?;
        }
        // Assemble the root script outputs and complete transitive dependency graph. Structural graph
        // and dependency-script failures abort before javac; lower-level classpath misses remain
        // warnings so the resolver can report all deterministic diagnostics.
        let (sources, inputs) = App::prepare_compile_inputs(
            &mut manifest,
            &root,
            exec,
            &features,
            self.offline,
            if self.dry_run {
                jals_project::SourcePublication::Skip
            } else {
                jals_project::SourcePublication::Apply
            },
        )
        .await?;
        let request = App::compile_request(&manifest, &root, sources.sources(), &inputs);
        // Select the backend `[toolchain] compiler` names: `"builtin"` is the in-process dummy;
        // anything else spawns `javac` (env override → discovered JDK → `$JAVA_HOME` → `PATH`).
        let compiler = <dyn Compiler>::select(&manifest, exec).await;

        if self.dry_run || self.verbose {
            println!("{}", compiler.describe_compile(&request));
        }
        if self.dry_run {
            return Ok(ExitCode::SUCCESS);
        }

        let outcome = compiler
            .compile(&request)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(App::outcome_exit_code(outcome))
    }
}

impl RunArgs {
    /// Compiles the project, then runs its main class with `java`. Compilation must succeed before the
    /// run; `--dry-run` prints both commands without executing either.
    async fn run(&self, exec: &Exec) -> Result<ExitCode> {
        let (mut manifest, root) = App::resolve_manifest(self.manifest_path.as_deref()).await?;
        let features = self.features.resolve(&manifest)?;
        // `--main-class` overrides all manifest-based selection; otherwise resolve the entry point
        // from `[[bin]]` / `[package] default-run` / `[run] main-class`.
        let main_class: String = match &self.main_class {
            Some(explicit) => explicit.clone(),
            None => jals_build::RunTarget::resolve(&manifest, self.bin.as_deref())
                .map_err(|e| anyhow!("{e}"))?
                .to_owned(),
        };
        // Assemble the compile inputs once. Transitive sources compile into `classes-dir`, while every
        // verified graph classpath artifact is shared by the javac and java requests.
        let (sources, inputs) = App::prepare_compile_inputs(
            &mut manifest,
            &root,
            exec,
            &features,
            self.offline,
            if self.dry_run {
                jals_project::SourcePublication::Skip
            } else {
                jals_project::SourcePublication::Apply
            },
        )
        .await?;
        let compile_request = App::compile_request(&manifest, &root, sources.sources(), &inputs);
        let run_request = jals_build::RunRequest {
            manifest: &manifest,
            project_root: &root,
            jvm_args: &inputs.jvm_args,
            main_class: &main_class,
            program_args: &self.args,
            extra_classpath: &inputs.extra_classpath,
            run_env: &inputs.run_env,
        };
        // Each step's backend is selected independently from its own `[toolchain]` enum:
        // `"builtin"` is the in-process dummy; anything else spawns `javac`/`java` per
        // `compiler`/`runtime` (each: env override → discovered JDK → `$JAVA_HOME` → `PATH`).
        let compiler = <dyn Compiler>::select(&manifest, exec).await;
        let runtime = <dyn Runtime>::select(&manifest, exec).await;

        if self.dry_run || self.verbose {
            println!("{}", compiler.describe_compile(&compile_request));
            println!("{}", runtime.describe_run(&run_request));
        }
        if self.dry_run {
            return Ok(ExitCode::SUCCESS);
        }

        // Compile first; only run when compilation succeeds.
        let build_outcome = compiler
            .compile(&compile_request)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        if !build_outcome.success() {
            return Ok(App::outcome_exit_code(build_outcome));
        }
        let run_outcome = runtime
            .run(&run_request)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(App::outcome_exit_code(run_outcome))
    }
}

impl CleanArgs {
    /// Removes the project's build output: discovers the manifest, resolves the artifact paths, and
    /// deletes each existing directory (a missing one is simply skipped, so cleaning a never-built
    /// project succeeds quietly). `--dry-run` prints the paths without deleting them.
    async fn run(&self, exec: &Exec) -> Result<ExitCode> {
        let (manifest, root) = App::resolve_manifest(self.manifest_path.as_deref()).await?;
        let storage = NativeStorage::for_project_scoped(
            &root,
            [NativeScope::all(RelativePath::ROOT)],
            exec.clone(),
        )
        .await
        .context("opening project storage for build-task cleanup")?;
        // Only portable source roots can own a publication root inside the project; one pointing
        // outside (`../shared/src`) simply has nothing here to clean.
        let source_roots: Vec<_> = manifest
            .build
            .source_dirs
            .iter()
            .filter_map(|root| jals_storage::DirKey::parse(root).ok())
            .collect();
        let mut keys = jals_project::BuildTaskExecutor::owned_publication_roots(
            &storage.view(),
            &source_roots,
        )
        .map_err(|error| anyhow!(error))?;
        keys.extend(
            jals_build::CleanTargets::keys(&manifest)
                .map_err(|error| anyhow!("invalid classes-dir: {error:?}"))?,
        );
        let mut seen = HashSet::new();
        keys.retain(|key| seen.insert(key.clone()));

        for key in keys {
            // The typed key confines the target under the project root; removal itself is a host
            // operation owned by the CLI (see `jals_build::clean`), so deleting build output does
            // not require snapshotting the project's bytes first.
            let path = key.path().to_host_path(&root);
            if self.dry_run {
                println!("would remove {}", path.display());
                continue;
            }
            if !path.is_dir() {
                continue;
            }
            std::fs::remove_dir_all(&path)
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
    async fn run(self, exec: &Exec) -> Result<ExitCode> {
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
        let name = match self.name {
            Some(n) => n,
            None => project_name_from_dir(&dir)?,
        };

        let files = jals_build::InitOptions { name: name.clone() }.scaffold();
        let scopes = files
            .iter()
            .map(|file| NativeScope::all(file.path.path().clone()));
        let mut storage = NativeStorage::for_project_scoped(&dir, scopes, exec.clone()).await?;
        let manifest_key = FileKey::parse("jals.toml").expect("static key is valid");
        if storage.view().tree().lookup_file(&manifest_key).is_some() {
            return Err(anyhow!("`jals.toml` already exists in {}", dir.display()));
        }
        for file in &files {
            let dest = dir.join(file.path.to_string());
            if storage.view().tree().lookup_file(&file.path).is_some() {
                println!("skipping {} (already exists)", dest.display());
                continue;
            }
            let mut transaction = storage.transaction(storage.revision())?;
            transaction.create_file(file.path.clone(), file.contents.as_bytes().to_vec())?;
            transaction.commit().await?;
        }

        println!("created JALS project `{name}` in {}", dir.display());
        Ok(ExitCode::SUCCESS)
    }
}

/// The project context the linter folds in for the `jals.toml` discovered upward from `start_dir`:
/// its lowered classpath (so the cross-file `type-mismatch` rule resolves external library types) and
/// the resolved language feature set from `[package] features` (so feature-gated rules like
/// `compact-source-file` run). Both come from a **single** best-effort assembly of the project's
/// analysis inputs. A missing manifest defaults silently; malformed manifests and graph failures
/// emit a warning before falling back to an empty classpath and feature set.
#[derive(Default)]
struct ProjectLintContext {
    classpath: LoweredClasspath,
    feature_set: FeatureSet,
}

impl ProjectLintContext {
    async fn load(start_dir: &Path, exec: &Exec) -> Self {
        let Some(manifest_path) = Manifest::discover_path(start_dir).await else {
            return Self::default();
        };
        let manifest = match Manifest::from_file(&manifest_path).await {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("warning: project analysis inputs unavailable: {error}");
                return Self::default();
            }
        };
        // `Path::new("jals.toml").parent()` is `Some("")`, not `None`, so the fallback below only
        // fires for a path with no parent at all. An empty root then fails to canonicalize and the
        // whole project context — classpath and feature set — is silently dropped, weakening lint
        // results whenever the manifest was discovered in the current directory.
        let root = match manifest_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        // Assemble the project's analysis inputs (best-effort): the classpath `.class` from the
        // `[build] classpath` plus resolved `[dependencies]` jars (folded into the cross-file
        // `type-mismatch` index) and the `[package] features`. Any strict graph failure is reported and
        // converted into the default lint context below.
        // The lint / editor path takes no `--features` flags, so build scripts see the manifest's
        // default selection. With nothing selected, resolution cannot fail.
        let features = manifest
            .resolve_build_features(&[], false, false)
            .unwrap_or_default();
        let environment = App::build_script_environment(&manifest, &features);
        let inputs = match App::project_inputs(
            &manifest,
            root,
            jals_classpath::ProjectInputOptions::Analysis,
            exec,
            HostBuildScript::default(),
            &RootScriptInputs {
                environment: &environment,
                features: &features,
            },
            // Lint analyses what is already here; it does not acquire dependencies.
            jals_classpath::NetworkPolicy::Offline,
        )
        .await
        {
            Ok(inputs) => inputs,
            Err(error) => {
                eprintln!("warning: project analysis inputs unavailable: {error:#}");
                return Self::default();
            }
        };
        Self {
            classpath: ProjectIndex::lower_classpath(&inputs.classpath_classes).await,
            feature_set: inputs.feature_set,
        }
    }

    /// Builds a lint-time [`ProjectIndex`] over `files`, folding in the embedded stdlib stubs and this
    /// context's lowered classpath so the index-aware `type-mismatch` rule resolves stdlib and
    /// external library types. Shared by the stdin and multi-file lint paths.
    async fn build_index(&self, files: &[(FileId, jals_syntax::SyntaxNode)]) -> ProjectIndex {
        ProjectIndex::builder(files)
            .with_stdlib()
            .with_classpath(&self.classpath)
            .build()
            .await
    }
}

/// Host-side helper operations for the CLI commands with no more natural home: manifest/source
/// resolution, JDK tool discovery and spawning, exit-code mapping, and `.java` file collection. A
/// stateless namespace grouping these cross-command utilities.
struct App;

#[derive(Default)]
struct HostProjectInputs {
    extra_classpath: Vec<PathBuf>,
    classpath_classes: Vec<jals_classfile::ClassFile>,
    extra_sources: Vec<PathBuf>,
    feature_set: FeatureSet,
    javac_args: Vec<String>,
    jvm_args: Vec<String>,
    compile_env: BTreeMap<String, String>,
    run_env: BTreeMap<String, String>,
}

impl HostProjectInputs {
    /// Keep authored sources and manifest classpath entries first, then retain each extra input's
    /// first occurrence without disturbing the order supplied by scripts and dependency resolution.
    fn deduplicate(&mut self, manifest: &mut Manifest, root: &Path, sources: &[PathBuf]) {
        let mut seen_sources: HashSet<PathBuf> = sources.iter().cloned().collect();
        self.extra_sources
            .retain(|source| seen_sources.insert(source.clone()));

        let mut seen_classpath = HashSet::new();
        manifest
            .build
            .classpath
            .retain(|entry| seen_classpath.insert(root.join(entry)));
        self.extra_classpath
            .retain(|entry| seen_classpath.insert(entry.clone()));
    }
}

#[derive(Default)]
struct HostBuildScript {
    generated_sources: Vec<PathBuf>,
    additional_classpath: Vec<PathBuf>,
    javac_args: Vec<String>,
    jvm_args: Vec<String>,
    compile_env: BTreeMap<String, String>,
    run_env: BTreeMap<String, String>,
}

impl From<HostBuildScript> for HostProjectInputs {
    fn from(script: HostBuildScript) -> Self {
        Self {
            extra_classpath: script.additional_classpath,
            extra_sources: script.generated_sources,
            javac_args: script.javac_args,
            jvm_args: script.jvm_args,
            compile_env: script.compile_env,
            run_env: script.run_env,
            ..Self::default()
        }
    }
}

/// The root project's build-script inputs, borrowed as one value.
///
/// The two are meaningless apart: `environment` is already scoped to the root and carries the
/// queryable half of `features`, whose other half is what the root's `[features]` forwards into the
/// dependency graph. Passing them together is what keeps a caller from installing one and
/// forgetting the other.
struct RootScriptInputs<'a> {
    environment: &'a BuildScriptEnvironment,
    features: &'a ResolvedBuildFeatures,
}

impl App {
    /// Discover and preprocess the complete dependency graph, then project it together with the
    /// root manifest over one immutable project revision and its verified native artifact cache.
    async fn project_inputs(
        manifest: &Manifest,
        root: &Path,
        options: jals_classpath::ProjectInputOptions,
        exec: &Exec,
        script: HostBuildScript,
        scripts: &RootScriptInputs<'_>,
        network: jals_classpath::NetworkPolicy,
    ) -> Result<HostProjectInputs> {
        let mut result = HostProjectInputs::from(script);
        let graph = jals_project::NativeProjectGraph::discover(manifest, root, exec, network)
            .await
            .context("discovering project dependency graph")?;
        let discovery_warning_count = graph.warnings().len();
        for warning in graph.warnings() {
            Self::print_graph_warning(warning);
        }

        let scopes = jals_classpath::NativeProjectPlan::snapshot_scopes(manifest, root);
        let mut storage = NativeStorage::for_project_scoped(root, scopes, exec.clone())
            .await
            .context("opening project storage")?;
        let graph = graph
            .preprocess(
                storage.artifacts_mut(),
                scripts.environment,
                scripts.features,
                &BuildScriptLimits::default(),
            )
            .await
            .context("preprocessing project dependency graph")?;
        let assembly = graph
            .assemble_native(manifest, root, &mut storage, options)
            .await;

        for warning in assembly.warnings.iter().skip(discovery_warning_count) {
            Self::print_graph_warning(warning);
        }
        for warning in &assembly.inputs.warnings {
            eprintln!("warning: {}", warning.message);
        }
        if !assembly.errors.is_empty() {
            let messages = assembly
                .errors
                .iter()
                .map(|error| {
                    error.path.as_ref().map_or_else(
                        || format!("{}: {}", error.node, error.message),
                        |path| format!("{} `{path}`: {}", error.node, error.message),
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(anyhow!("project dependency assembly failed: {messages}"));
        }

        for entry in &assembly.compile_classpath {
            let path = match entry {
                jals_project::CompileClasspathEntry::File(file) => storage
                    .artifacts()
                    .materialize_file(&file.key, &file.path)
                    .await
                    .map_err(|error| {
                        anyhow!(
                            "materializing dependency classpath `{}` failed: {error:?}",
                            file.path
                        )
                    })?,
                jals_project::CompileClasspathEntry::Tree(tree) => storage
                    .artifacts()
                    .materialize_tree(
                        tree.members
                            .iter()
                            .map(|member| (&member.path, &member.key)),
                    )
                    .await
                    .map_err(|error| {
                        anyhow!(
                            "materializing dependency classpath directory `{}` failed: {error:?}",
                            tree.path
                        )
                    })?,
            };
            result.extra_classpath.push(path);
        }
        for source in &assembly.inputs.source_dep_sources {
            match source {
                jals_classpath::SourceFile::Project(key) => {
                    result.extra_sources.push(key.path().to_host_path(root));
                }
                jals_classpath::SourceFile::Artifact(source) => {
                    match storage
                        .artifacts()
                        .materialize_file(&source.key, &source.path)
                        .await
                    {
                        Ok(path) => result.extra_sources.push(path),
                        Err(error) => {
                            eprintln!("warning: materializing git source failed: {error:?}");
                        }
                    }
                }
            }
        }
        result.classpath_classes = assembly.inputs.classpath_classes;
        result.feature_set = assembly.inputs.feature_set;
        Ok(result)
    }

    /// Prepare the root and transitive compile inputs shared by `build` and `run`.
    async fn prepare_compile_inputs(
        manifest: &mut Manifest,
        root: &Path,
        exec: &Exec,
        features: &ResolvedBuildFeatures,
        offline: bool,
        publications: jals_project::SourcePublication,
    ) -> Result<(jals_build::StagedTree, HostProjectInputs)> {
        let environment = Self::build_script_environment(manifest, features);
        let script =
            Self::run_build_script(manifest, root, exec, &environment, offline, publications)
                .await?;
        let sources = Self::discover_sources(manifest, root, !script.generated_sources.is_empty())?;
        // The root build script's output is root project source, so it goes through the root
        // frontend alongside the authored files. Dependency-contributed sources, which land in
        // `extra_sources` further down, deliberately do not: a dependency is lowered under its
        // own manifest's frontend, never re-expanded by whoever consumes it.
        let generated = script.generated_sources.clone();
        let mut inputs = Self::project_inputs(
            manifest,
            root,
            jals_classpath::ProjectInputOptions::Compile,
            exec,
            script,
            &RootScriptInputs {
                environment: &environment,
                features,
            },
            if offline {
                jals_classpath::NetworkPolicy::Offline
            } else {
                jals_classpath::NetworkPolicy::Online
            },
        )
        .await?;
        inputs.deduplicate(manifest, root, &sources);
        // Deduplication compares against the *authored* paths, so it must happen before lowering
        // replaces them with staged ones.
        // A build script may register a file that is *also* an authored source (`add_source` on
        // an existing project file is legal), so the union has to be deduplicated — a tree with
        // two entries at one path is rejected, correctly, by the frontend.
        let mut to_lower = sources;
        let mut seen: HashSet<PathBuf> = to_lower.iter().cloned().collect();
        for path in &generated {
            if seen.insert(path.clone()) {
                to_lower.push(path.clone());
            }
        }
        let staged = Self::lower_sources(manifest, root, &to_lower).await?;
        // Whatever was lowered is now represented by its staged copy; leaving the original in
        // `extra_sources` would hand javac the pre-frontend file as well.
        inputs
            .extra_sources
            .retain(|path| !generated.contains(path));
        // Replace `-sourcepath` with the staged tree so the authored source dirs leave it
        // entirely. Without this the compiler could resolve a type from the authored source it was
        // never given on the command line, silently reading around the frontend — harmless while
        // the frontend is the identity, and a correctness hole the moment one rewrites anything.
        //
        // This only *excludes* the authored roots; it does not repoint resolution at the staged
        // copies. The staging root is not a package root — staged files keep their full
        // project-relative path beneath it (`<root>/src/main/java/com/example/Main.java`), so
        // implicit lookup of `com.example.Foo` probes `<root>/com/example/Foo.java` and always
        // misses. That is fine today because every source is passed to javac explicitly; a future
        // rewriting frontend that relies on implicit resolution would have to stage under the
        // original source-dir prefix instead.
        manifest.build.source_dirs = Self::staged_source_dirs(root, &staged);
        Ok((staged, inputs))
    }

    fn print_graph_warning(warning: &jals_project::GraphWarning) {
        if let Some(dependency) = &warning.dependency {
            eprintln!(
                "warning: project dependency `{dependency}`: {}",
                warning.message
            );
        } else if let Some(node) = &warning.node {
            eprintln!("warning: project dependency {node}: {}", warning.message);
        } else {
            eprintln!("warning: project dependency graph: {}", warning.message);
        }
    }

    /// Construct the explicit environment visible to both root and dependency build scripts.
    ///
    /// Only `JALS_`-prefixed host variables cross the boundary. The rest of the host environment
    /// stays out: a build script can forward anything it reads into a task fetch URL, so
    /// inheriting wholesale would expose every credential on the machine to an unreviewed
    /// `build.rhai` — including a dependency's. See [`BuildScriptEnvironment::HOST_PREFIX`].
    ///
    /// Only the **root project's** own queryable half of `features` is installed here. A dependency
    /// node's script is given its own resolved set by the graph's preprocessing pass, from the
    /// `[dependencies]` entries aimed at it and whatever a `[features]` entry forwarded — the
    /// [`dependencies`](ResolvedBuildFeatures::dependencies) half, which never lands in an
    /// environment the root's script can read.
    fn build_script_environment(
        manifest: &Manifest,
        features: &ResolvedBuildFeatures,
    ) -> BuildScriptEnvironment {
        BuildScriptEnvironment::from_host(std::env::vars_os().filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?))
        }))
        .for_project(manifest, features.features().clone())
    }

    /// Execute the manifest's optional Rhai pre-build phase against a project snapshot. The host
    /// supplies environment values as plain data; scripts only read and publish through typed
    /// `jals-storage` keys.
    async fn run_build_script(
        manifest: &Manifest,
        root: &Path,
        exec: &Exec,
        environment: &BuildScriptEnvironment,
        offline: bool,
        publications: jals_project::SourcePublication,
    ) -> Result<HostBuildScript> {
        let mut storage = NativeStorage::for_project_scoped(
            root,
            [NativeScope::all(RelativePath::ROOT)],
            exec.clone(),
        )
        .await
        .context("opening project storage for the build script")?;
        let mut session = BuildScriptSession::new();
        let fetcher = jals_classpath::ReqwestFetcher::for_project(root.to_path_buf());
        let network = if offline {
            jals_classpath::NetworkPolicy::Offline
        } else {
            jals_classpath::NetworkPolicy::Online
        };
        let root_output = jals_project::BuildTaskExecutor::execute_root(
            exec,
            &fetcher,
            &mut storage,
            &mut session,
            jals_project::RootBuildScriptOptions {
                manifest,
                environment,
                limits: &BuildScriptLimits::default(),
                network,
                host: jals_project::BuildTaskHost::Project,
                blocked_files: &[],
                publications,
            },
        )
        .await
        .map_err(|error| match error {
            jals_project::RootBuildScriptError::BuildScript(
                jals_build::build_script::BuildScriptError::ReportedErrors(diagnostics),
            ) => anyhow!(
                "build script reported errors: {}",
                diagnostics
                    .iter()
                    .filter_map(|diagnostic| match diagnostic {
                        BuildScriptDiagnostic::Error(message) => Some(message.as_str()),
                        BuildScriptDiagnostic::Warning(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            other => anyhow!(other),
        })?;
        let mut task_classpath = Vec::new();
        for (index, key) in root_output.task_classpath.iter().enumerate() {
            let logical = RelativePath::parse(&format!("build-task/{index}.jar"))
                .expect("build-task materialization path is portable");
            task_classpath.push(
                storage
                    .artifacts()
                    .materialize_file(key, &logical)
                    .await
                    .map_err(|error| {
                        anyhow!("materializing build-task classpath failed: {error:?}")
                    })?,
            );
        }
        let Some(output) = root_output.script else {
            return Ok(HostBuildScript::default());
        };
        for diagnostic in &output.diagnostics {
            if let BuildScriptDiagnostic::Warning(message) = diagnostic {
                eprintln!("warning: build script: {message}");
            }
        }
        let mut additional_classpath: Vec<_> = output
            .additional_classpath
            .iter()
            .map(|key| key.path().to_host_path(root))
            .collect();
        additional_classpath.extend(task_classpath);
        Ok(HostBuildScript {
            generated_sources: output
                .generated_sources
                .iter()
                .map(|key| key.path().to_host_path(root))
                .collect(),
            additional_classpath,
            javac_args: output.javac_args,
            jvm_args: output.jvm_args,
            compile_env: output.compile_env,
            run_env: output.run_env,
        })
    }

    /// Resolves the manifest from an explicit path or by discovering `jals.toml` upward from the cwd,
    /// returning the parsed manifest and the project root (the manifest's parent directory). A missing
    /// manifest is an error, unlike the formatter/linter configs.
    async fn resolve_manifest(explicit: Option<&Path>) -> Result<(Manifest, PathBuf)> {
        let manifest_path = if let Some(p) = explicit {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir()
                    .context("getting current dir")?
                    .join(p)
            }
        } else {
            let cwd = std::env::current_dir().context("getting current dir")?;
            Manifest::discover_path(&cwd)
                .await
                .ok_or_else(|| anyhow!("no `jals.toml` found in {} or any parent", cwd.display()))?
        };
        let manifest = Manifest::from_file(&manifest_path)
            .await
            .with_context(|| format!("loading {}", manifest_path.display()))?;
        let root = manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Ok((manifest, root))
    }

    /// Collects the `.java` files under the manifest's source directories (resolved against `root`).
    /// Each source directory must exist, and at least one source file must be found.
    fn discover_sources(
        manifest: &Manifest,
        root: &Path,
        has_generated_sources: bool,
    ) -> Result<Vec<PathBuf>> {
        let source_roots = manifest.source_roots(root);
        for dir in &source_roots {
            if !dir.is_dir() && !has_generated_sources {
                return Err(anyhow!("source directory {} does not exist", dir.display()));
            }
        }
        let existing_roots: Vec<PathBuf> = source_roots
            .into_iter()
            .filter(|root| root.is_dir())
            .collect();
        let sources = Self::collect_java_files(&existing_roots)?;
        if sources.is_empty() && !has_generated_sources {
            return Err(anyhow!(
                "no .java files found under {:?}",
                manifest.build.source_dirs
            ));
        }
        Ok(sources)
    }

    /// Run the project's frontend over the discovered sources and stage the result on disk.
    ///
    /// This is the frontend/backend seam. The compiler is handed the returned paths and never the
    /// paths that went in, so whatever `javac` compiles is by construction something a frontend
    /// emitted. With the default vanilla frontend the bytes are identical to the authored
    /// sources, so the observable build is unchanged — the point of this release is that the
    /// *path* now goes through the seam, not that the output differs.
    ///
    /// The staged tree lives under `target/jals/build/frontend`, which `jals clean` already
    /// removes and which the build-script fingerprint rules already refuse to treat as a rerun
    /// input.
    async fn lower_sources(
        manifest: &Manifest,
        root: &Path,
        sources: &[PathBuf],
    ) -> Result<jals_build::StagedTree> {
        let frontend: &dyn jals_frontend::Frontend = match manifest.build.frontend {
            jals_config::FrontendKind::Vanilla {} => &jals_frontend::VanillaFrontend,
        };

        let mut files = Vec::with_capacity(sources.len());
        for path in sources {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading source {}", path.display()))?;
            // Logical, project-relative identity. Sorting on this rather than on the filesystem
            // walk order is what keeps cache keys identical across machines.
            let relative = RelativePath::from_host_path(root, path)
                .ok_or_else(|| anyhow!("source {} is outside the project root", path.display()))?;
            files.push(jals_frontend::IrFile::new(relative, bytes.into()));
        }
        jals_frontend::FrontendKey::canonical_order(&mut files);

        // Only the artifact cache is needed, so open it directly rather than taking a whole
        // project snapshot: lowering reads its inputs from `files`, never from a `ProjectView`.
        let mut cache = jals_storage::ArtifactCache::new(jals_storage::NativeCache::new(
            root.join(NativeStorage::PROJECT_CACHE_DIR),
        ));

        let lowered = jals_frontend::Driver::lower(frontend, &mut cache, &files)
            .await
            .map_err(|error| anyhow!("frontend `{}` failed: {error}", frontend.caps().id))?;

        let tree: Vec<_> = lowered
            .tree
            .files()
            .iter()
            .map(|file| (file.path.clone(), file.key.clone()))
            .collect();

        jals_build::StagedTree::write(&cache, &tree, root.join(jals_build::FRONTEND_OUT_DIR))
            .await
            .map_err(|error| anyhow!("staging frontend output failed: {error}"))
    }

    /// The staged tree expressed as manifest `source-dirs`, relative to the project root when
    /// possible so the rendered `javac` command stays readable.
    ///
    /// This is the staging *root*, not a package root: staged files keep their full
    /// project-relative path beneath it, so setting `-sourcepath` to it resolves nothing
    /// implicitly. It is retained only to replace — and thereby exclude — the authored source
    /// dirs; every source is passed to javac explicitly.
    fn staged_source_dirs(root: &Path, staged: &jals_build::StagedTree) -> Vec<String> {
        let path = staged
            .root()
            .strip_prefix(root)
            .unwrap_or_else(|_| staged.root());
        vec![path.to_string_lossy().into_owned()]
    }

    /// The compile inputs shared by `jals build` and `jals run`: the manifest plus its discovered
    /// sources, with the resolved dependency jars on the classpath and the `git`/`path` source
    /// dependencies' `.java` compiled alongside — one place that wires `ProjectInputs` into a
    /// [`CompileRequest`](jals_build::CompileRequest).
    fn compile_request<'a>(
        manifest: &'a Manifest,
        project_root: &'a Path,
        sources: &'a [PathBuf],
        inputs: &'a HostProjectInputs,
    ) -> jals_build::CompileRequest<'a> {
        jals_build::CompileRequest {
            manifest,
            project_root,
            sources,
            extra_sources: &inputs.extra_sources,
            extra_classpath: &inputs.extra_classpath,
            extra_javac_args: &inputs.javac_args,
            compile_env: &inputs.compile_env,
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
                } else if path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("java"))
                {
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

    /// Read and parse the single config file at `path` — no project snapshot is taken for it.
    fn load_config<C: DiscoverableConfig>(path: &Path) -> Result<C> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("config filename is not valid UTF-8: {}", path.display()))?;
        let key = FileKey::parse(name)
            .map_err(|error| anyhow!("invalid config filename `{name}`: {error:?}"))?;
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        C::from_text(&key, &text).map_err(Into::into)
    }

    /// The config an explicit `--config` path names, when one was given.
    fn load_explicit<C: DiscoverableConfig>(explicit: Option<&Path>) -> Result<Option<C>> {
        explicit
            .map(|p| Self::load_config::<C>(p).context("loading --config"))
            .transpose()
    }
}

/// Host-side memoized config discovery for one run: the explicit `--config` override answers
/// every directory; otherwise the governing config root is found by walking `dir`'s ancestors on
/// the host filesystem, and its file is read and parsed once per root.
struct HostConfigs<C> {
    explicit: Option<C>,
    by_root: HashMap<PathBuf, C>,
}

impl<C: DiscoverableConfig + Clone + Default> HostConfigs<C> {
    fn new(explicit: Option<C>) -> Self {
        Self {
            explicit,
            by_root: HashMap::new(),
        }
    }

    /// The config governing `dir`: the explicit override, the memoized config of the discovered
    /// root, or the default when no ancestor carries `C::FILE_NAME`.
    fn for_dir(&mut self, dir: &Path) -> Result<C> {
        if let Some(config) = &self.explicit {
            return Ok(config.clone());
        }
        let Some(root) = dir
            .ancestors()
            .find(|candidate| candidate.join(C::FILE_NAME).is_file())
        else {
            return Ok(C::default());
        };
        if let Some(config) = self.by_root.get(root) {
            return Ok(config.clone());
        }
        let config: C = App::load_config(&root.join(C::FILE_NAME))
            .with_context(|| format!("discovering config from {}", dir.display()))?;
        self.by_root.insert(root.to_path_buf(), config.clone());
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_inputs_are_stably_deduplicated_against_authored_inputs() {
        let root = Path::new("/project");
        let authored = vec![root.join("src/A.java"), root.join("src/B.java")];
        let mut manifest = Manifest::default();
        manifest.build.classpath = vec!["libs/base.jar".to_owned(), "libs/base.jar".to_owned()];
        let mut inputs = HostProjectInputs {
            extra_sources: vec![
                authored[1].clone(),
                root.join("generated/Z.java"),
                authored[0].clone(),
                root.join("generated/A.java"),
                root.join("generated/Z.java"),
            ],
            extra_classpath: vec![
                root.join("libs/z.jar"),
                root.join("libs/base.jar"),
                root.join("libs/a.jar"),
                root.join("libs/z.jar"),
            ],
            ..HostProjectInputs::default()
        };

        inputs.deduplicate(&mut manifest, root, &authored);

        assert_eq!(
            inputs.extra_sources,
            vec![root.join("generated/Z.java"), root.join("generated/A.java")]
        );
        assert_eq!(
            inputs.extra_classpath,
            vec![root.join("libs/z.jar"), root.join("libs/a.jar")]
        );
        assert_eq!(manifest.build.classpath, vec!["libs/base.jar"]);
    }
}
