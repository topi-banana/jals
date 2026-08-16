//! `jals` command-line interface.

mod migrate;
mod report;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use jals_build::build_script::{BuildScriptEnvironment, BuildScriptLimits, BuildScriptSession};
use jals_build::{ManifestExt, Runtime};
use jals_config::fmt::Config;
use jals_config::lint::Config as LintConfig;
use jals_config::{DiscoverableConfig, FeatureSet, Manifest, ResolvedBuildFeatures};
use jals_exec::Exec;
use jals_storage::{DirKey, FileKey, Name, NativeScope, NativeStorage, RelativePath};

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

    /// Do not generate a `jalsfmt.toml` from a detected Eclipse / IntelliJ / EditorConfig
    /// formatter config. The detected settings are still used for this run.
    // Product names, and this doc line is also the `--help` text, so it stays unquoted.
    #[allow(clippy::doc_markdown)]
    #[arg(long)]
    no_migrate: bool,
}

#[derive(Args)]
struct LintArgs {
    /// Files or directories to lint. Directories are searched recursively for `.java` files.
    /// With no paths, source is read from stdin.
    paths: Vec<PathBuf>,

    /// Use this config file instead of discovering `jalslint.toml`.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Build-feature selection for `#[cfg(feature = "…")]` analysis (with the `attributes`
    /// dialect on) and build scripts — the same flags `build`/`run` take. Defaults to the
    /// manifest's `default` list.
    #[command(flatten)]
    features: FeatureArgs,
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
        let mut features = HostFeatures::default();
        let mut any_changed = false;
        let mut any_warning = false;
        // A file the fail-safe refused is byte-identical to a file that was already formatted, so
        // `any_changed` cannot see it and `--check` would call it clean. Tracked separately.
        let mut any_fallback = false;

        if self.paths.is_empty() {
            // stdin -> stdout
            let mut src = String::new();
            std::io::stdin()
                .read_to_string(&mut src)
                .context("reading stdin")?;
            let cwd = std::env::current_dir().context("getting current dir")?;
            // Migrating a native config still applies to a piped source, so stdin and a file get
            // the same output — but nothing is written: a pipe should not make a file appear in
            // the working directory.
            self.migrate(std::slice::from_ref(&cwd), false, &mut discovery, exec)
                .await?;
            let cfg = discovery.for_dir(&cwd)?;
            let out =
                jals_fmt::FormatOutput::format_source(&src, &cfg, features.for_dir(&cwd).await)
                    .await;
            let changed = out.formatted != src;
            any_changed |= changed;
            any_warning |= out.has_warnings();
            any_fallback |= out.fell_back();
            Reporter::report_format_warnings("<stdin>", &src, &out);
            Reporter::report_format_fallback("<stdin>", &out);
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
            // Resolve — and, in write mode, emit — the migrated config before any source is
            // rewritten, so a run can never format against a config it then fails to record.
            let anchors: Vec<PathBuf> = groups.keys().cloned().collect();
            self.migrate(&anchors, !show_diff, &mut discovery, exec)
                .await?;
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
                    let dir = path.parent().unwrap_or_else(|| Path::new("."));
                    let cfg = discovery.for_dir(dir)?;
                    let out = jals_fmt::FormatOutput::format_source(
                        &src,
                        &cfg,
                        features.for_dir(dir).await,
                    )
                    .await;
                    let changed = out.formatted != src;
                    any_changed |= changed;
                    any_warning |= out.has_warnings();
                    any_fallback |= out.fell_back();
                    let label = path.display().to_string();
                    Reporter::report_format_warnings(&label, &src, &out);
                    Reporter::report_format_fallback(&label, &out);

                    if show_diff {
                        Reporter::print_diff(&label, &src, &out.formatted);
                    } else if changed {
                        edits.push((key, out.formatted.into_bytes()));
                    }
                }
                Self::commit_edits(&mut storage, edits).await?;
            }
        }

        // A fallback fails `--check` even though nothing changed: `--check` answers "is every file
        // formatted", and a file the formatter refused to touch is not a file it formatted. Reporting
        // it clean is what let the defect this guards against go unnoticed.
        //
        // It fails `-D warnings` too, whatever the mode. `report_format_fallback` announces it on the
        // one line every other file-less diagnostic uses — prefixed `warning:` — so leaving it out of
        // the flag that denies warnings would have the prefix promise something the flag does not
        // honor, and `jals fmt -D warnings` would exit 0 on a file it just declined to format.
        let fail = (self.check && any_changed)
            || (any_fallback && (self.check || deny_warnings))
            || (deny_warnings && any_warning);
        Ok(if fail {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    }

    /// Detect a native formatter config for each anchor directory and fold it into `discovery`,
    /// writing it out as a `jalsfmt.toml` when `may_write` allows.
    ///
    /// Detection resolves to a *project root*, so several anchors inside one project collapse to
    /// one migration and one generated file. An explicit `--config` wins over every directory, so
    /// there is nothing to detect in that case.
    ///
    /// `may_write` is the caller's mode; `--no-migrate` turns the write off while leaving the
    /// detected config in play for this run.
    async fn migrate(
        &self,
        anchors: &[PathBuf],
        may_write: bool,
        discovery: &mut HostConfigs<Config>,
        exec: &Exec,
    ) -> Result<()> {
        if self.config.is_some() {
            return Ok(());
        }
        let mut seen = HashSet::new();
        for anchor in anchors {
            let Some(migration) =
                migrate::Migration::detect(anchor, migrate::Walk::Ancestors, exec).await?
            else {
                continue;
            };
            if !seen.insert(migration.root.clone()) {
                continue;
            }
            Reporter::report_migration(&migration);
            if may_write && !self.no_migrate {
                match migration.write(exec).await? {
                    Some(path) => println!("created {}", path.display()),
                    None => eprintln!(
                        "note: {} already exists",
                        migration.root.join("jalsfmt.toml").display()
                    ),
                }
            }
            discovery.seed(&migration.root, migration.config.clone());
        }
        Ok(())
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

/// A file named on the command line, or stdin, before it has a workspace key.
struct NamedSource {
    /// How the file is named in output: its path as the caller spelled it, or `<stdin>`.
    label: String,
    /// The directory whose `jalslint.toml` governs it — its parent, or the cwd for stdin. Each
    /// file is looked up separately, so one run can span directories with different configs.
    config_dir: PathBuf,
    /// Where the file lives, or `None` for stdin.
    path: Option<PathBuf>,
    /// stdin's text. A named file carries none: its bytes come from the project snapshot when that
    /// holds them, and are read at mount time when it does not.
    text: Option<String>,
}

/// One file this run reports on, resolved to the identity the workspace answers by.
struct LintTarget {
    label: String,
    config_dir: PathBuf,
    key: FileKey,
}

impl LintArgs {
    async fn run(&self, exec: &Exec) -> Result<ExitCode> {
        let explicit_config = App::load_explicit::<LintConfig>(self.config.as_deref())?;
        let mut discovery = HostConfigs::new(explicit_config);

        // What this run reports on, and where project discovery starts from.
        let named = Self::named_sources(&self.paths)?;
        let anchor = named
            .first()
            .map_or_else(|| PathBuf::from("."), |file| file.config_dir.clone());
        let mut project = LintProject::open(&anchor, exec, &self.features).await?;

        // Reported ⊆ indexed. The workspace indexes the source-root walk ∪ `project_sources`,
        // because diagnostics assembly reports every type name that resolves to nothing and a
        // sibling the caller did not name is not an unresolved name — it is a file the index was
        // never given. This only decides which keys are *reported* on.
        let targets = Self::resolve_targets(&mut project, named)?;
        let workspace = jals_editor::Workspace::load(project.storage, project.layout).await;

        let mut any_finding = false;
        for target in &targets {
            // A named file must be analysable: the caller asked for it by name. The workspace
            // silently skips a file it cannot read or decode, so this is where both failures
            // surface — otherwise an unreadable file would report nothing and read as clean.
            let Some(id) = workspace.file_id(&target.key) else {
                bail!("{}: could not be read for analysis", target.label);
            };
            let doc = workspace
                .document(id)
                .expect("an indexed file has a parsed document");
            // Looked up per file, so one run can span directories with different `jalslint.toml`.
            // The feature set is deliberately not set here: the workspace folds in the project's
            // own, exactly as it does for the language server and the playground.
            let config = discovery.for_dir(&target.config_dir)?;
            // The one policy — syntax errors, `cfg` hints, and the rule engine, in one order —
            // reached through the same seam the other two hosts reach it through. What a broken
            // tree suppresses is decided inside the engine, so no host restates it.
            let diagnostics = workspace.diagnostics(&target.key, &config).await;
            any_finding |= Reporter::report_lint(&target.label, &doc.text, &diagnostics);
        }

        Ok(if any_finding {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    }

    /// What this run reports on, each file once: stdin as one detached file when no paths were
    /// given, otherwise every `.java` the paths name or contain.
    ///
    /// One file reached two ways — named directly and again through a directory that contains it,
    /// or spelled with a `./` or `..` detour — is one reported file, keeping the first spelling
    /// because that is the label the caller will recognize. Deduplicating here rather than after
    /// the keys are known is what keeps it a single rule: a file the project snapshot holds and one
    /// mounted into it arrive at their keys by different routes, and only one of those routes makes
    /// two spellings converge on their own.
    ///
    /// Only stdin is read here. A named file's bytes come from the project snapshot, or — when that
    /// does not hold it — are read at mount time in [`resolve_targets`](Self::resolve_targets),
    /// where it is already known which of the two applies.
    fn named_sources(paths: &[PathBuf]) -> Result<Vec<NamedSource>> {
        if paths.is_empty() {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("reading stdin")?;
            return Ok(vec![NamedSource {
                label: "<stdin>".to_owned(),
                config_dir: std::env::current_dir().context("getting current dir")?,
                path: None,
                text: Some(text),
            }]);
        }
        let mut seen = HashSet::new();
        Ok(App::collect_java_files(paths)?
            .into_iter()
            .filter(|path| seen.insert(App::canonical_path(path)))
            .map(|path| NamedSource {
                label: path.display().to_string(),
                config_dir: path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf(),
                path: Some(path),
                text: None,
            })
            .collect())
    }

    /// Give every named source the identity the workspace answers by, reading and mounting only
    /// what the project snapshot does not already hold.
    ///
    /// A path under the project root that the snapshot captured needs nothing read: the workspace
    /// reads it, and its key is the very one the source-root walk produces — which is what makes
    /// naming a file and naming the root that contains it one entry instead of two declarations of
    /// the same type. Anything else — a file outside every snapshot scope, a path outside the
    /// project root, stdin — is read here and mounted.
    ///
    /// Unlike a project source folded in beside them, an unreadable file here *is* an error: the
    /// caller asked for it by name.
    fn resolve_targets(
        project: &mut LintProject,
        named: Vec<NamedSource>,
    ) -> Result<Vec<LintTarget>> {
        let mut targets = Vec::with_capacity(named.len());
        for (index, source) in named.into_iter().enumerate() {
            let captured = source
                .path
                .as_deref()
                .and_then(|path| project.key_of(path))
                .filter(|key| project.holds(key));
            let key = if let Some(key) = captured {
                // Already addressable. Registering it as a project source is what indexes a named
                // file lying under no source root; for one the source-root walk already reaches,
                // the workspace deduplicates it away.
                project.layout.project_sources.push(key.clone());
                key
            } else {
                let bytes = if let Some(text) = source.text {
                    text.into_bytes()
                } else {
                    let path = source.path.as_deref().unwrap_or_else(|| Path::new("."));
                    std::fs::read(path).with_context(|| format!("reading {}", path.display()))?
                };
                project.mount(index, source.path.as_deref(), bytes)?
            };
            targets.push(LintTarget {
                label: source.label,
                config_dir: source.config_dir,
                key,
            });
        }
        Ok(targets)
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
        // One fetch capability for the whole command. It carries `--offline`, so every phase that
        // can fetch is handed the same answer instead of being told it separately.
        let fetcher = jals_classpath::ReqwestFetcher::for_project(
            root.clone(),
            jals_classpath::NetworkPolicy::when_offline(self.offline),
        );
        let (sources, tree, inputs) = App::prepare_compile_inputs(
            &mut manifest,
            &root,
            exec,
            &features,
            &fetcher,
            if self.dry_run {
                jals_project::SourcePublication::Skip
            } else {
                jals_project::SourcePublication::Apply
            },
        )
        .await?;
        // `[build] backend` picks *what* compiles the lowered tree, and the selection owns that
        // decision — this host can spawn a process, so every backend kind is available to it.
        let plan = CompilePlan::prepare(&manifest, &root, &sources, tree, &inputs, exec).await?;
        let request = plan.request();

        if self.dry_run || self.verbose {
            println!("{}", plan.backend.describe(&request));
        }
        if self.dry_run {
            return Ok(ExitCode::SUCCESS);
        }

        let outcome = plan
            .backend
            .compile(&request)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        App::finish_compile(&manifest, &root, &outcome)?;
        App::finish_package(
            &manifest, &root, exec, &features, &fetcher, &outcome, &inputs,
        )
        .await?;
        Ok(App::outcome_exit_code(outcome.code()))
    }
}

impl RunArgs {
    /// Compiles the project, then runs its main class with `java`. Compilation must succeed before the
    /// run; `--dry-run` prints both commands without executing either.
    async fn run(&self, exec: &Exec) -> Result<ExitCode> {
        let (mut manifest, root) = App::resolve_manifest(self.manifest_path.as_deref()).await?;
        // `jals run` is `java`, and a WebAssembly module is not something `java` can be handed. The
        // check is here rather than at the launch because the failure would otherwise surface as a
        // missing main class in a `classes-dir` that holds a `.wasm` — true, and useless.
        if matches!(
            manifest.build.backend,
            jals_config::BackendKind::JalsWasm {}
        ) {
            bail!(
                "`jals run` runs a main class on a JVM, and `[build] backend` is `jals-wasm`, \
                 which compiles the project to a WebAssembly module instead. Run the module with a \
                 wasm engine (`wasmtime run --invoke <method> {}/project.wasm`), or switch the \
                 backend to `jals` or `javac` to produce class files.",
                manifest.build.classes_dir
            );
        }
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
        // One fetch capability for the whole command; see `BuildArgs::run`.
        let fetcher = jals_classpath::ReqwestFetcher::for_project(
            root.clone(),
            jals_classpath::NetworkPolicy::when_offline(self.offline),
        );
        let (sources, tree, inputs) = App::prepare_compile_inputs(
            &mut manifest,
            &root,
            exec,
            &features,
            &fetcher,
            if self.dry_run {
                jals_project::SourcePublication::Skip
            } else {
                jals_project::SourcePublication::Apply
            },
        )
        .await?;
        let run_request = jals_build::RunRequest {
            manifest: &manifest,
            project_root: &root,
            jvm_args: &inputs.jvm_args,
            main_class: &main_class,
            program_args: &self.args,
            extra_classpath: &inputs.extra_classpath,
            run_env: &inputs.run_env,
        };
        // The compile step goes through the same `[build] backend` selection `jals build` uses, so a
        // manifest asking for the in-process compiler gets it here too. The run step is selected
        // independently from `[toolchain] runtime`: `"builtin"` is the in-process dummy, anything
        // else spawns `java` (env override → discovered JDK → `$JAVA_HOME` → `PATH`).
        let plan = CompilePlan::prepare(&manifest, &root, &sources, tree, &inputs, exec).await?;
        let runtime = <dyn Runtime>::select(&manifest, exec).await;
        let compile_request = plan.request();

        if self.dry_run || self.verbose {
            println!("{}", plan.backend.describe(&compile_request));
            println!("{}", runtime.describe_run(&run_request));
        }
        if self.dry_run {
            return Ok(ExitCode::SUCCESS);
        }

        // Compile first; only run when compilation succeeds. `finish_compile` also persists whatever
        // an in-process backend produced, so the classes are on disk before `java` looks for them.
        let outcome = plan
            .backend
            .compile(&compile_request)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        App::finish_compile(&manifest, &root, &outcome)?;
        if !outcome.success() {
            return Ok(App::outcome_exit_code(outcome.code()));
        }
        let run_outcome = runtime
            .run(&run_request)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(App::outcome_exit_code(run_outcome.code))
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

        let mut files = jals_build::InitOptions { name: name.clone() }.scaffold();
        // A native formatter config already in the target directory becomes a fourth scaffold
        // file. `InitOptions::scaffold` is pure — it cannot look at a filesystem — so the
        // detection happens here and its result joins the list. Only the directory itself is
        // probed: a new project should not silently inherit an unrelated parent repository's
        // formatter settings.
        if let Some(migration) =
            migrate::Migration::detect(&dir, migrate::Walk::DirectoryOnly, exec).await?
        {
            Reporter::report_migration(&migration);
            files.push(jals_build::ScaffoldFile {
                path: FileKey::parse("jalsfmt.toml").expect("static key is valid"),
                contents: migration
                    .provenance
                    .jalsfmt_toml(&migration.config, &migration.warnings),
            });
        }
        // The scopes have to be derived after the push, or the generated file falls outside the
        // snapshot and both the existence check and the create below misbehave.
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

/// The project `jals lint` analyses, as the editor seam takes it: one open aggregate plus the
/// layout describing what to index.
///
/// Everything the CLI used to re-derive around this — file identity, `#[cfg]` evaluation, and the
/// path-identity dedupe that kept one file from being indexed twice — belongs to
/// [`jals_editor::Workspace`]. What is left is the host's own job: discovering the manifest,
/// opening the aggregate, and lowering `PathBuf` values into typed keys.
///
/// Every failure below degrades to a narrower project rather than failing the run. A missing
/// manifest degrades silently; a malformed one, an unopenable root, and a graph failure each warn
/// first. A graph failure keeps the aggregate and the manifest's own source roots, so the
/// project's own files still resolve — only the classpath, the feature set, and the dependency
/// sources are lost. Otherwise the degraded run would resolve *fewer* names than it has files
/// for, and report the difference as unresolved.
struct LintProject {
    /// The project root every key is relative to: the manifest's directory, or the anchor for a
    /// run with no project at all.
    root: PathBuf,
    storage: NativeStorage,
    layout: jals_editor::ProjectLayout,
}

impl LintProject {
    /// Where a file the project snapshot does not hold is mounted.
    ///
    /// Not a `[build] source-dirs` root and captured by no snapshot scope, so it collides with
    /// nothing a project declares. The same `.jals/` convention the language server mounts a
    /// cached navigation source under.
    const MOUNT_ROOT: &'static str = ".jals/lint";

    /// Discover the project upward from `start_dir` and open its aggregate.
    async fn open(start_dir: &Path, exec: &Exec, selection: &FeatureArgs) -> Result<Self> {
        let Some(manifest_path) = Manifest::discover_path(start_dir).await else {
            return Self::detached(start_dir, exec).await;
        };
        let manifest = match Manifest::from_file(&manifest_path).await {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("warning: project analysis inputs unavailable: {error}");
                return Self::detached(start_dir, exec).await;
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
        // `jals lint` takes the same `--features` flags as `build`/`run`; nothing selected
        // resolves the manifest's `default` list. An invalid selection (an unknown feature name)
        // warns and degrades to the default rather than dropping the whole project context.
        let features = selection.resolve(&manifest).unwrap_or_else(|error| {
            eprintln!("warning: invalid feature selection ({error}); using defaults");
            manifest
                .resolve_build_features(&[], false, false)
                .unwrap_or_default()
        });
        let environment = App::build_script_environment(&manifest, &features);
        // Opened by this caller rather than inside `project_inputs`, because it outlives that
        // call: the revision the graph phase read is the one the lint index is built over.
        let mut storage = match App::open_project_storage(&manifest, root, exec).await {
            Ok(storage) => storage,
            Err(error) => {
                eprintln!("warning: project analysis inputs unavailable: {error:#}");
                return Self::detached(start_dir, exec).await;
            }
        };
        // The project's analysis inputs, best-effort: the classpath `.class` from `[build]
        // classpath` plus resolved `[dependencies]` jars, the `[package] features`, and the
        // `.java` of `git`/`path` dependencies — every typing authority a name can resolve to.
        let inputs = match App::project_inputs(
            &mut storage,
            &manifest,
            root,
            jals_classpath::ProjectInputOptions::Analysis,
            // Lint analyses what is already on disk; opening a project to report diagnostics must
            // not execute an unreviewed `build.rhai`.
            RootScript::skipped(),
            &RootScriptInputs {
                environment: &environment,
                features: &features,
            },
            // Lint analyses what is already here; it does not acquire dependencies. The refusal is
            // the capability's, so it holds for every phase this hands the fetcher to.
            &jals_classpath::ReqwestFetcher::for_project(
                root.to_path_buf(),
                jals_classpath::NetworkPolicy::Offline,
            ),
        )
        .await
        {
            Ok(inputs) => inputs,
            Err(error) => {
                eprintln!("warning: project analysis inputs unavailable: {error:#}");
                // The same lowering the assembly would have used, not a second rule for what
                // `[build] source-dirs` means.
                let source_roots = jals_classpath::NativeProjectPlan::from_manifest(
                    &manifest,
                    &features,
                    root,
                    &storage.view(),
                )
                .source_roots;
                return Ok(Self {
                    root: root.to_path_buf(),
                    storage,
                    layout: jals_editor::ProjectLayout::new(source_roots),
                });
            }
        };

        let mut layout = jals_editor::ProjectLayout {
            // Resolved once by the assembly, so no host re-lowers `[build] source-dirs` itself.
            source_roots: inputs.source_roots,
            feature_set: inputs.feature_set,
            // What each project file's `#[cfg(feature = "…")]` evaluates against, read only when
            // `feature_set` enables the `attributes` dialect — so an attribute-free project's lint
            // output is independent of `--features`.
            build_features: features.into_features(),
            ..jals_editor::ProjectLayout::default()
        }
        .with_classpath(&inputs.classpath_classes)
        .await;
        layout.source_dep_sources =
            Self::mount_source_deps(&mut storage, &inputs.source_dep_files).await;
        Ok(Self {
            root: root.to_path_buf(),
            storage,
            layout,
        })
    }

    /// An aggregate rooted at `anchor` that captures nothing — the shape a run with no usable
    /// project takes.
    ///
    /// An empty scope list is what makes the snapshot empty, so nothing is read from disk. Every
    /// reported file is then mounted, which makes the index exactly the files the caller named —
    /// what a run outside a project always had.
    async fn detached(anchor: &Path, exec: &Exec) -> Result<Self> {
        let storage = NativeStorage::for_project_scoped(anchor, [], exec.clone())
            .await
            .with_context(|| format!("opening {} for analysis", anchor.display()))?;
        Ok(Self {
            root: anchor.to_path_buf(),
            storage,
            layout: jals_editor::ProjectLayout::default(),
        })
    }

    /// The workspace key of `path`, or `None` when it lies outside this project's root or spells a
    /// segment no portable name allows.
    ///
    /// Both sides are canonicalized first, which is what makes a path named on the command line
    /// and the same file reached through a `[build] source-dirs` root produce one key — and one
    /// key is one indexed file. The CLI used to compare canonicalized paths itself for exactly
    /// this; now the comparison is the key's.
    fn key_of(&self, path: &Path) -> Option<FileKey> {
        let root = App::canonical_path(&self.root);
        let path = App::canonical_path(path);
        RelativePath::from_host_path(&root, &path).and_then(|relative| FileKey::new(relative).ok())
    }

    /// Whether the captured project revision holds `key`.
    ///
    /// Addressable is not the same as captured: `snapshot_scopes` decides what a revision reads,
    /// so a `.java` outside every scope has a perfectly good key and no bytes behind it.
    fn holds(&self, key: &FileKey) -> bool {
        self.storage.view().tree().lookup_file(key).is_some()
    }

    /// Mount `bytes` as this run's `index`-th reported file and register it as a project source.
    ///
    /// For a file the project snapshot does not hold: named outside the root, outside every
    /// snapshot scope, or piped in with no path at all. The overlay lives in memory for the length
    /// of the run — nothing is written to disk — and the key goes into `project_sources`, which is
    /// what indexes a file lying under no source root.
    fn mount(&mut self, index: usize, name: Option<&Path>, bytes: Vec<u8>) -> Result<FileKey> {
        let key = self.mount_key(index, name);
        // The revision is re-read per mount because each one publishes a new revision; the
        // workspace sorts what it indexes, so the order they land in does not matter.
        self.storage
            .set_overlay(self.storage.revision(), key.clone(), bytes)
            .with_context(|| format!("mounting `{key}` for analysis"))?;
        self.layout.project_sources.push(key.clone());
        Ok(key)
    }

    /// A key under [`MOUNT_ROOT`](Self::MOUNT_ROOT) for the `index`-th reported file.
    ///
    /// `index` makes mounts unique among themselves by construction. The file name is cosmetic —
    /// the assembly is handed a parse and never reads a name — so stdin gets a fixed one and a
    /// basename that is not a portable name falls back rather than failing the run. A key the tree
    /// already holds takes a numeric prefix, which in practice never fires: no snapshot scope
    /// captures `.jals`.
    fn mount_key(&self, index: usize, name: Option<&Path>) -> FileKey {
        let stem = name.map_or("stdin.java", |path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .filter(|name| Name::new(*name).is_ok())
                .unwrap_or("source.java")
        });
        let root = DirKey::parse(&format!("{}/{index}", Self::MOUNT_ROOT))
            .expect("the mount root and a decimal index are portable segments");
        let segment =
            |spelling: &str| Name::new(spelling).expect("a checked stem, or a numeric prefix");
        let mut key = root.file(segment(stem));
        let mut suffix = 1_u32;
        while self.holds(&key) {
            key = root.file(segment(&format!("{suffix}-{stem}")));
            suffix += 1;
        }
        key
    }

    /// Mount every `git`/`path` dependency source into the aggregate and return their keys.
    ///
    /// A dependency source the project revision captured is already addressable and needs nothing.
    /// One that lives in the verified cache is mounted under `.jals/source-dependency/`, the same
    /// convention the language server uses and for the same reason: materializing it would put it
    /// under `target/jals/cache`, which the project snapshot deliberately excludes.
    async fn mount_source_deps(
        storage: &mut NativeStorage,
        sources: &[jals_classpath::SourceFile],
    ) -> Vec<FileKey> {
        let mount_root =
            DirKey::parse(".jals/source-dependency").expect("the mount root is a portable path");
        let mut keys = Vec::new();
        let mut mounts = Vec::new();
        for source in sources {
            match source {
                jals_classpath::SourceFile::Project(key) => keys.push(key.clone()),
                jals_classpath::SourceFile::Artifact(source) => {
                    // Best-effort: an artifact missing from the cache narrows what resolves, and
                    // the assembly has already reported why.
                    let Ok(Some(bytes)) = storage.artifacts().lookup(&source.key).await else {
                        continue;
                    };
                    let Ok(key) = mount_root.file_at(&source.path) else {
                        continue;
                    };
                    mounts.push((key.clone(), bytes));
                    keys.push(key);
                }
            }
        }
        if !mounts.is_empty() {
            // One revision for the whole batch: these are resolved together and read together.
            let _ = storage.set_overlays(storage.revision(), mounts);
        }
        keys
    }
}

/// Host-side helper operations for the CLI commands with no more natural home: manifest/source
/// resolution, JDK tool discovery and spawning, exit-code mapping, and `.java` file collection. A
/// stateless namespace grouping these cross-command utilities.
struct App;

#[derive(Default)]
struct HostProjectInputs {
    extra_classpath: Vec<PathBuf>,
    /// Every cached archive the compiled classes are reobfuscated against: the resolved
    /// `[dependencies]` jars, then the archives the build tasks put on the classpath.
    ///
    /// One field rather than two halves a caller concatenates, because a caller that passed one
    /// half is the bug: a member inherited from the jar that was left out keeps its original name in
    /// an otherwise remapped archive, which is a wrong answer and not a failure.
    ///
    /// Declared dependencies lead and script-added archives follow, which is where
    /// `ProjectScript::augment_classpath` already places a `build.add_classpath` directive relative
    /// to an authored `[build] classpath` entry. First occurrence wins, so a jar two nodes both
    /// fetched is unpacked once.
    ///
    /// Kept beside `extra_classpath` rather than derived from it: a post-compile remap needs the
    /// compile classpath as a *class hierarchy*, and re-reading those paths off disk to publish
    /// them again would be the same bytes acquired a second way.
    remap_hierarchy: Vec<jals_storage::CacheKey>,
    classpath_classes: Vec<jals_classfile::ClassFile>,
    extra_sources: Vec<PathBuf>,
    /// The manifest's source roots as the assembly resolved them — the directory half of a
    /// [`jals_editor::ProjectLayout`]. Derived once, here, so no host re-lowers
    /// `[build] source-dirs` into keys a second way.
    source_roots: Vec<jals_storage::DirKey>,
    /// The `git`/`path` dependency sources *before* materialization.
    ///
    /// `build`/`run` consume the host paths in [`extra_sources`](Self::extra_sources); `jals lint`
    /// mounts these into its own aggregate instead, because a materialized artifact lives under
    /// `target/jals/cache`, which the project snapshot deliberately excludes.
    source_dep_files: Vec<jals_classpath::SourceFile>,
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

/// The compile step, selected and ready to run.
///
/// Owns what a [`BackendRequest`](jals_build::BackendRequest) borrows, which is the whole reason it
/// exists: `jals build` and `jals run` assemble the step identically, and a plain helper returning
/// the request would hand back borrows of its own locals.
struct CompilePlan {
    backend: Box<dyn jals_build::Backend>,
    tree: Vec<jals_build::BackendSource>,
    options: jals_build::BackendOptions,
}

impl CompilePlan {
    /// Select the backend `[build] backend` names and gather what its request borrows.
    ///
    /// Absence is a value the selection returns rather than a failure raised somewhere downstream,
    /// so this is the only place a missing backend has to be handled.
    async fn prepare(
        manifest: &Manifest,
        root: &Path,
        staged: &jals_build::StagedTree,
        tree: Vec<jals_build::BackendSource>,
        inputs: &HostProjectInputs,
        exec: &Exec,
    ) -> Result<Self> {
        let selection = jals_build::BackendSelection::for_host(
            manifest,
            root,
            staged,
            // The compile inputs that cannot travel in a portable request — every one of them a
            // resolved host path — for the `javac` adapter that needs them.
            &jals_build::HostCompileInputs {
                extra_sources: &inputs.extra_sources,
                extra_classpath: &inputs.extra_classpath,
                extra_javac_args: &inputs.javac_args,
                compile_env: &inputs.compile_env,
            },
            exec,
        )
        .await;
        match selection {
            jals_build::BackendSelection::Available(backend) => Ok(Self {
                backend,
                tree,
                options: jals_build::BackendOptions::from_manifest(manifest),
            }),
            jals_build::BackendSelection::Absent { id, reason } => {
                bail!("`[build] backend` selects `{id}`, but {reason}")
            }
        }
    }

    /// What the selected backend compiles.
    fn request(&self) -> jals_build::BackendRequest<'_> {
        jals_build::BackendRequest {
            tree: &self.tree,
            // The in-process compiler reads its library signatures from the embedded stubs rather
            // than from the classpath; wiring dependency classes in is what would let it compile
            // against them. The `javac` adapter takes the real classpath as a host input instead,
            // because its entries are paths and this request is portable.
            classpath: &[],
            options: &self.options,
        }
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

/// The root script phase's two products: the token that carries it into the graph phase, and the
/// host-path inputs it contributed. They are meaningless apart — a caller holding one without the
/// other is either assembling a graph the script did not run for, or dropping the script's flags.
struct RootScript {
    assembled: jals_project::ProjectScript,
    host: HostBuildScript,
}

impl RootScript {
    /// A run that deliberately executed no script.
    fn skipped() -> Self {
        Self {
            assembled: jals_project::ProjectScript::skipped(),
            host: HostBuildScript::default(),
        }
    }
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
    /// Open the project aggregate the graph phase reads and writes.
    ///
    /// Scoped by [`snapshot_scopes`](jals_classpath::NativeProjectPlan::snapshot_scopes), which is
    /// the only rule for what a project revision captures. Separate from
    /// [`project_inputs`](Self::project_inputs) because the aggregate outlives that call for one
    /// caller: `jals lint` goes on to index the same revision through `jals_editor::Workspace`,
    /// while `build`/`run` drop it as soon as their artifacts are materialized.
    async fn open_project_storage(
        manifest: &Manifest,
        root: &Path,
        exec: &Exec,
    ) -> Result<NativeStorage> {
        let scopes = jals_classpath::NativeProjectPlan::snapshot_scopes(manifest, root);
        NativeStorage::for_project_scoped(root, scopes, exec.clone())
            .await
            .context("opening project storage")
    }

    /// Discover and preprocess the complete dependency graph, then project it together with the
    /// root manifest over one immutable project revision and its verified native artifact cache.
    ///
    /// The aggregate is the caller's — see [`open_project_storage`](Self::open_project_storage) —
    /// and it carries the execution context, so there is no separate `exec` to hand over and no
    /// way to hand over one that is not the aggregate's. `jals_editor::Workspace::load` takes its
    /// own the same way.
    async fn project_inputs(
        storage: &mut NativeStorage,
        manifest: &Manifest,
        root: &Path,
        options: jals_classpath::ProjectInputOptions,
        script: RootScript,
        scripts: &RootScriptInputs<'_>,
        fetcher: &jals_classpath::ReqwestFetcher,
    ) -> Result<HostProjectInputs> {
        let mut result = HostProjectInputs::from(script.host);
        let exec = storage.exec().clone();
        let assembly = script
            .assembled
            .resolve_native(
                manifest,
                root,
                storage,
                jals_project::GraphPreprocess {
                    exec: &exec,
                    // The caller's capability, which is the root's: a dependency's build tasks and
                    // its jars resolve under the same policy, from the same project cache —
                    // `--offline` means offline for the whole graph.
                    fetcher,
                    environment: scripts.environment,
                    root_features: scripts.features,
                    limits: &BuildScriptLimits::default(),
                },
                options,
            )
            .await
            .map_err(|failure| {
                // Discovery had already found something worth saying about this project before a
                // later phase failed, and it is usually the half that explains the other: the
                // dependency preprocessing could not resolve is often the one discovery warned was
                // unavailable. The assembly orders and grades both; this prints what it produced.
                //
                // The script phase is `Skipped` here whichever command is running: whoever ran a
                // script reports it (`run_build_script`), and `jals lint` runs none at all.
                Reporter::report_project(
                    &jals_project::ProjectDiagnostics::assemble(
                        jals_project::ScriptOutcome::Skipped,
                        jals_project::GraphOutcome::Failed(&failure),
                        None,
                    ),
                    None,
                );
                // No `.context()` on top: it would restate this sentence, and the detail is in the
                // diagnostics just reported rather than in the error chain.
                anyhow!("the project dependency graph could not be resolved")
            })?;

        let reported = jals_project::ProjectDiagnostics::assemble(
            jals_project::ScriptOutcome::Skipped,
            jals_project::GraphOutcome::Resolved(assembly.report()),
            None,
        );
        Reporter::report_project(&reported, None);
        // What "could not be assembled" means is the assembly's, not a severity test spelled here.
        if jals_project::ProjectDiagnostics::has_errors(&reported) {
            // No outer phrase and no restated detail: every failure has just been reported in full,
            // and repeating one here would print it twice under two different leads.
            return Err(anyhow!("the project could not be assembled"));
        }

        // The two halves are in hand here and nowhere else: `ProjectInputs` single-sources the
        // declared jars for every host, and the assembly states which archives the scripts added.
        // This is therefore also the only place the duplicates between them can be dropped.
        //
        // On the *content* digest rather than the whole key, because the two halves address the
        // same archive differently: a library declared under `[dependencies]` and fetched again by
        // a task is one jar under two namespaces, and the hierarchy index reads nothing but the
        // bytes. Keying on the whole key would decode a game jar twice for no difference in the
        // index it builds.
        let mut seen = HashSet::new();
        result.remap_hierarchy = assembly
            .inputs
            .dependency_jars
            .iter()
            .chain(&assembly.task_classpath)
            .filter(|key| seen.insert(key.content()))
            .cloned()
            .collect();

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
        // Kept beside the materialized host paths below rather than derived from them: a host that
        // indexes these mounts them into its own project revision, and a materialized artifact is
        // a path under `target/jals/cache`, which that revision does not capture.
        result
            .source_dep_files
            .clone_from(&assembly.inputs.source_dep_sources);
        result.source_roots.clone_from(&assembly.source_roots);
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
        fetcher: &jals_classpath::ReqwestFetcher,
        publications: jals_project::SourcePublication,
    ) -> Result<(
        jals_build::StagedTree,
        Vec<jals_build::BackendSource>,
        HostProjectInputs,
    )> {
        let environment = Self::build_script_environment(manifest, features);
        let script =
            Self::run_build_script(manifest, root, exec, &environment, fetcher, publications)
                .await?;
        let sources =
            Self::discover_sources(manifest, root, !script.host.generated_sources.is_empty())?;
        // The root build script's output is root project source, so it goes through the root
        // frontend alongside the authored files. Dependency-contributed sources, which land in
        // `extra_sources` further down, deliberately do not: a dependency is lowered under its
        // own manifest's frontend, never re-expanded by whoever consumes it.
        let generated = script.host.generated_sources.clone();
        // A compile consumes its dependency artifacts as host paths, so the aggregate is done with
        // once they are materialized; only `jals lint` keeps one past this call.
        let mut storage = Self::open_project_storage(manifest, root, exec).await?;
        let mut inputs = Self::project_inputs(
            &mut storage,
            manifest,
            root,
            jals_classpath::ProjectInputOptions::Compile,
            script,
            &RootScriptInputs {
                environment: &environment,
                features,
            },
            fetcher,
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
        let (staged, tree) = Self::lower_sources(manifest, root, &to_lower, features).await?;
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
        Ok((staged, tree, inputs))
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
        fetcher: &jals_classpath::ReqwestFetcher,
        publications: jals_project::SourcePublication,
    ) -> Result<RootScript> {
        let mut storage = NativeStorage::for_project_scoped(
            root,
            [NativeScope::all(RelativePath::ROOT)],
            exec.clone(),
        )
        .await
        .context("opening project storage for the build script")?;
        let mut session = BuildScriptSession::new();
        // The configured script's key and text, so a failure that carries a Rhai position can be
        // pointed at the offending line rather than reported without a location.
        let script_key = manifest
            .build
            .script
            .as_ref()
            .and_then(|script| match script {
                jals_config::BuildScript::Rhai { file } => FileKey::parse(file).ok(),
            });
        let script_text = script_key
            .as_ref()
            .and_then(|key| storage.view().file_text(key).ok().map(ToOwned::to_owned));
        let script_label = script_key
            .as_ref()
            .map(|key| key.path().to_host_path(root).display().to_string());
        let script_file = script_key.as_ref().map(|key| jals_project::ScriptFile {
            key,
            text: script_text.as_deref(),
        });
        let report = |outcome: jals_project::ScriptOutcome<'_>| {
            Reporter::report_project(
                &jals_project::ProjectDiagnostics::assemble(
                    outcome,
                    jals_project::GraphOutcome::NotReached,
                    script_file,
                ),
                // `zip` already gates on both halves: a configured script whose text would not read
                // reports its diagnostics without a source to point at, like an unconfigured one.
                script_label.as_deref().zip(script_text.as_deref()),
            );
        };
        let assembled = match jals_project::ProjectAssembly::script(
            exec,
            fetcher,
            &mut storage,
            &mut session,
            jals_project::RootBuildScriptOptions {
                manifest,
                environment,
                limits: &BuildScriptLimits::default(),
                host: jals_project::BuildTaskHost::Project,
                blocked_files: &[],
                publications,
            },
        )
        .await
        {
            Ok(assembled) => assembled,
            Err(error) => {
                report(jals_project::ScriptOutcome::Failed(&error));
                // Reported in full above, with a span when the script gave one. Restating the error
                // here would print it a second time under a different lead.
                return Err(anyhow!("the build script failed"));
            }
        };
        report(
            assembled
                .output()
                .map_or(jals_project::ScriptOutcome::Skipped, |output| {
                    jals_project::ScriptOutcome::Ran(output)
                }),
        );
        let mut task_classpath = Vec::new();
        for (index, key) in assembled.task_classpath().iter().enumerate() {
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
        // A project with no script declares no task plan either, so there is nothing materialized
        // above to carry forward on that path.
        let host = assembled
            .output()
            .map_or_else(HostBuildScript::default, |output| {
                let mut additional_classpath: Vec<_> = output
                    .additional_classpath
                    .iter()
                    .map(|key| key.path().to_host_path(root))
                    .collect();
                additional_classpath.extend(task_classpath);
                HostBuildScript {
                    generated_sources: output
                        .generated_sources
                        .iter()
                        .map(|key| key.path().to_host_path(root))
                        .collect(),
                    additional_classpath,
                    javac_args: output.javac_args.clone(),
                    jvm_args: output.jvm_args.clone(),
                    compile_env: output.compile_env.clone(),
                    run_env: output.run_env.clone(),
                }
            });
        Ok(RootScript { assembled, host })
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
        features: &ResolvedBuildFeatures,
    ) -> Result<(jals_build::StagedTree, Vec<jals_build::BackendSource>)> {
        // `[build.frontend]` and the dialect features that override it are answered in
        // `jals-frontend`, not here — the host supplies the resolved build features (the same set
        // a build script queries) and asks once.
        let frontend =
            jals_frontend::FrontendSelection::for_manifest(manifest, features.features());

        let mut files = Vec::with_capacity(sources.len());
        for path in sources {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading source {}", path.display()))?;
            // Logical, project-relative identity. The seam sorts on this rather than on the
            // filesystem walk order, which is what keeps cache keys identical across machines.
            let relative = RelativePath::from_host_path(root, path)
                .ok_or_else(|| anyhow!("source {} is outside the project root", path.display()))?;
            files.push(jals_frontend::IrFile::new(relative, bytes.into()));
        }

        // Only the artifact cache is needed, so open it directly rather than taking a whole
        // project snapshot: lowering reads its inputs from `files`, never from a `ProjectView`.
        let mut cache = jals_storage::ArtifactCache::new(jals_storage::NativeCache::new(
            root.join(NativeStorage::PROJECT_CACHE_DIR),
        ));

        let lowered = frontend
            .lower(&mut cache, files)
            .await
            .map_err(|error| anyhow!("frontend `{}` failed: {error}", frontend.id()))?;

        // Resolve the published keys to bytes once, here: the [`Backend`](jals_build::Backend)
        // contract is object-safe and `ArtifactCache` is not, so this is the host's job — and the
        // same list is what both staging and the backend consume, so looking it up twice would
        // read and verify every lowered file a second time.
        let mut tree = Vec::with_capacity(lowered.tree.files().len());
        for file in lowered.tree.files() {
            let bytes = cache
                .lookup(&file.key)
                .await
                .map_err(|error| anyhow!("reading lowered source `{}`: {error}", file.path))?
                .ok_or_else(|| {
                    anyhow!(
                        "lowered source `{}` is not in the artifact cache",
                        file.path
                    )
                })?;
            tree.push(jals_build::BackendSource {
                path: file.path.clone(),
                key: file.key.clone(),
                bytes,
            });
        }

        // The tree is staged for *both* backends: a process-based compiler needs the files on
        // disk, and having them there keeps `--verbose` and post-mortem debugging identical
        // whichever backend ran.
        let staged = jals_build::StagedTree::write(&tree, root.join(jals_build::FRONTEND_OUT_DIR))
            .await
            .map_err(|error| anyhow!("staging frontend output failed: {error}"))?;
        Ok((staged, tree))
    }

    /// Report what a compile said and persist what it produced.
    ///
    /// Artifacts are only written on success, and only an in-process backend has any: a process-based
    /// one already wrote its own output through `javac -d`, so the loop is a no-op for it and the two
    /// backends need no branch here.
    fn finish_compile(
        manifest: &Manifest,
        root: &Path,
        outcome: &jals_build::BackendOutcome,
    ) -> Result<()> {
        for message in &outcome.messages {
            eprintln!("error: {message}");
        }
        if !outcome.success() {
            return Ok(());
        }
        // Output goes to the filesystem directly, exactly where `javac -d` would have put it: this
        // is build output, not tracked project source, and `jals clean` already owns removing it.
        let classes_dir = root.join(&manifest.build.classes_dir);
        for (path, bytes) in &outcome.artifacts {
            let target = classes_dir.join(path.to_string());
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&target, bytes)
                .with_context(|| format!("writing {}", target.display()))?;
        }
        Ok(())
    }

    /// Run the post-compile packaging `[build] remap` names, when the manifest asks for one.
    ///
    /// Packaging, not only remapping: a declared step writes its jar whether or not a mapping set
    /// is active, so a release that ships deobfuscated produces the same distributable as one that
    /// does not. This host never matches on `[build] remap` itself: it asks
    /// [`RemapSelection`](jals_project::RemapSelection) once and does what comes back. What is left
    /// here is only what a host path forces — collecting the class bytes, and writing the jar.
    async fn finish_package(
        manifest: &Manifest,
        root: &Path,
        exec: &Exec,
        features: &ResolvedBuildFeatures,
        fetcher: &jals_classpath::ReqwestFetcher,
        outcome: &jals_build::BackendOutcome,
        inputs: &HostProjectInputs,
    ) -> Result<()> {
        if !outcome.success() {
            return Ok(());
        }
        let plan = match jals_project::RemapSelection::for_manifest(manifest, features) {
            jals_project::RemapSelection::NotRequested => return Ok(()),
            jals_project::RemapSelection::Ambiguous(ambiguous) => {
                bail!("`[build] remap` cannot be resolved: {ambiguous}");
            }
            jals_project::RemapSelection::Unsupported { backend, reason } => {
                bail!(
                    "`[build] remap` is declared, but `[build] backend` selects `{backend}`: {reason}"
                );
            }
            jals_project::RemapSelection::Requested(plan) => plan,
        };

        // Where the class bytes are is the one part of this a host has to answer: an in-process
        // backend returns them, and `javac` wrote its own through `-d` and hands back nothing.
        let classes = if jals_project::CompiledClasses::are_in_memory(&outcome.artifacts) {
            outcome.artifacts.clone()
        } else {
            Self::read_classes_dir(&root.join(&manifest.build.classes_dir))?
        };
        if classes.is_empty() {
            bail!("`[build] remap` found no class files to package");
        }

        let scopes = jals_classpath::NativeProjectPlan::snapshot_scopes(manifest, root);
        let mut storage = NativeStorage::for_project_scoped(root, scopes, exec.clone())
            .await
            .context("opening project storage")?;
        let main_class = jals_build::RunTarget::resolve(manifest, None).ok();
        let bytes = plan
            .run(
                exec,
                // The command's own capability: under `--offline` a `url` mapping that is not
                // already in the verified cache now fails here rather than being fetched.
                fetcher,
                &mut storage,
                &classes,
                &inputs.remap_hierarchy,
                main_class,
            )
            .await
            .map_err(|error| anyhow!("`[build] remap` failed: {error}"))?;

        let target = root.join(&plan.jar);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&target, &bytes).with_context(|| format!("writing {}", target.display()))?;
        Ok(())
    }

    /// Every `.class` file below `dir`, addressed relative to it.
    ///
    /// Reading the compiler's own output back is what a process-based backend forces: it wrote the
    /// files and reported nothing, so a step that consumes what it produced has to go and look.
    fn read_classes_dir(dir: &Path) -> Result<Vec<(jals_storage::RelativePath, Vec<u8>)>> {
        let mut pending = vec![dir.to_path_buf()];
        let mut classes = Vec::new();
        while let Some(current) = pending.pop() {
            let entries = match std::fs::read_dir(&current) {
                Ok(entries) => entries,
                // A project that has never compiled has no directory, which is not a failure of
                // this step — the caller reports the empty result as the missing input it is.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(anyhow!("reading {}: {error}", current.display()));
                }
            };
            for entry in entries {
                let entry = entry.with_context(|| format!("reading {}", current.display()))?;
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "class") {
                    continue;
                }
                let relative = path.strip_prefix(dir).unwrap_or(&path);
                let Some(text) = relative.to_str() else {
                    bail!("class file `{}` is not a portable path", path.display());
                };
                let key = jals_storage::RelativePath::parse(&text.replace('\\', "/"))
                    .map_err(|error| anyhow!("class file `{text}` is not portable: {error:?}"))?;
                classes.push((
                    key,
                    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
                ));
            }
        }
        // Deterministic output: the jar's member order is part of its bytes, and a directory walk
        // is not ordered by anything a filesystem promises.
        classes.sort_by_key(|(path, _)| path.to_string());
        Ok(classes)
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

    /// Maps a compile or run step's exit code to a CLI [`ExitCode`]: 0 succeeds, any other code
    /// propagates, and a signal-terminated process (no code) fails with code 1.
    ///
    /// The mapping stays here because it is this driver's policy: `javac` distinguishes a compile
    /// error (1) from bad arguments (2) and a system error (3), and a shell that sees only "nonzero"
    /// cannot tell a broken invocation from broken source.
    fn outcome_exit_code(code: Option<i32>) -> ExitCode {
        match code {
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

    /// A path as the filesystem knows it: absolute and symlink-free where possible, so `src/a`,
    /// `./src/a` and an absolute spelling of the same thing agree.
    ///
    /// Two callers need that agreement for different reasons — a host config lookup keys its memo
    /// by the discovered root, and `jals lint` decides whether a named file and a project source
    /// are one file before indexing both. Comparing canonicalized paths is also what makes a macOS
    /// temporary directory (`/var` → `/private/var`) compare equal to itself.
    ///
    /// Falls back to the path as given when it cannot be canonicalized (it may not exist yet),
    /// which is no worse than not canonicalizing at all.
    fn canonical_path(path: &Path) -> PathBuf {
        let path = if path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            path
        };
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
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
///
/// Roots are keyed by their canonical path, so `src/a`, `./src/a` and an absolute spelling of one
/// directory share a memo entry — and so a [seeded](Self::seed) root matches however the caller
/// spells the directory it asks about.
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

    /// Record a config as governing `root` even though no `C::FILE_NAME` file exists there.
    ///
    /// `jals fmt` uses this for a native formatter config it migrated but did not write out
    /// (`--check`, `--diff`, and stdin all format against the migrated config while leaving the
    /// project untouched). Nothing seeds the lint config, so `jals lint` keeps its exact
    /// file-or-default behavior.
    fn seed(&mut self, root: &Path, config: C) {
        self.by_root.insert(App::canonical_path(root), config);
    }

    /// The config governing `dir`: the explicit override, the memoized or seeded config of the
    /// discovered root, or the default when no ancestor carries `C::FILE_NAME`.
    fn for_dir(&mut self, dir: &Path) -> Result<C> {
        if let Some(config) = &self.explicit {
            return Ok(config.clone());
        }
        // Nearest first, so an authored config always beats one seeded further up.
        let start = App::canonical_path(dir);
        let Some(root) = start.ancestors().find(|candidate| {
            self.by_root.contains_key(*candidate) || candidate.join(C::FILE_NAME).is_file()
        }) else {
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

/// Host-side memoized `[package] features` discovery for one `jals fmt` run.
///
/// The sibling of [`HostConfigs`] for the one formatter input that is not a formatter config:
/// `[imports] granularity = "package"` writes jals dialect syntax, which compiles only where
/// `[package] features` enables `grouped-imports`. So `jals fmt` resolves the manifest governing
/// each file the same way it resolves the `jalsfmt.toml` governing it — upward from the file's own
/// directory — instead of assuming a project or assuming none.
///
/// A directory with no manifest above it, or one whose manifest does not parse, answers with the
/// empty set. That is not a silent degradation: the empty set is exactly what the formatter reads
/// as "do not write dialect syntax", and it reports the rounding as a warning of its own.
#[derive(Default)]
struct HostFeatures {
    by_dir: HashMap<PathBuf, FeatureSet>,
}

impl HostFeatures {
    /// The feature set governing `dir`, discovered once per directory.
    async fn for_dir(&mut self, dir: &Path) -> FeatureSet {
        let key = App::canonical_path(dir);
        if let Some(features) = self.by_dir.get(&key) {
            return *features;
        }
        let features = match Manifest::discover_path(&key).await {
            Some(path) => Manifest::from_file(&path)
                .await
                .map(|manifest| manifest.feature_set())
                .unwrap_or_default(),
            None => FeatureSet::default(),
        };
        self.by_dir.insert(key, features);
        features
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
