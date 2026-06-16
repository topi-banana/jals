//! `jals` command-line interface.

mod report;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use jals_build::Manifest;
use jals_fmt::Config;
use jals_lint::Config as LintConfig;

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

    /// Run this fully-qualified main class instead of `[run] main-class`.
    #[arg(long, value_name = "FQCN")]
    main_class: Option<String>,

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
        Commands::Fmt(args) => run_fmt(args),
        Commands::Lsp(args) => run_lsp(args),
        Commands::Lint(args) => run_lint(args),
        Commands::Build(args) => run_build(args),
        Commands::Run(args) => run_run(args),
        Commands::Clean(args) => run_clean(args),
        Commands::Init(args) => run_init(args),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run_fmt(args: FmtArgs) -> Result<ExitCode> {
    let deny_warnings = args.deny.iter().any(|d| d == "warnings");
    let explicit_config = args
        .config
        .as_deref()
        .map(Config::from_file)
        .transpose()
        .context("loading --config")?;

    // `--check` and `--diff` both render a diff and write nothing; `--check` additionally
    // fails the run. With neither, stdin is echoed to stdout and files are rewritten in place.
    let show_diff = args.check || args.diff;

    let mut discovery = Discovery::new(explicit_config);
    let mut any_changed = false;
    let mut any_warning = false;

    if args.paths.is_empty() {
        // stdin -> stdout
        let mut src = String::new();
        std::io::stdin()
            .read_to_string(&mut src)
            .context("reading stdin")?;
        let cfg = discovery.for_dir(&std::env::current_dir().context("getting current dir")?)?;
        let out = jals_fmt::format_source(&src, &cfg);
        let changed = out.formatted != src;
        any_changed |= changed;
        any_warning |= out.has_warnings();
        report::report_format_warnings("<stdin>", &src, &out);
        if show_diff {
            report::print_diff("<stdin>", &src, &out.formatted);
        } else {
            std::io::stdout()
                .write_all(out.formatted.as_bytes())
                .context("writing stdout")?;
        }
    } else {
        for path in collect_java_files(&args.paths)? {
            let src = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            let cfg = discovery.for_dir(parent)?;
            let out = jals_fmt::format_source(&src, &cfg);
            let changed = out.formatted != src;
            any_changed |= changed;
            any_warning |= out.has_warnings();
            let label = path.display().to_string();
            report::report_format_warnings(&label, &src, &out);

            if show_diff {
                report::print_diff(&label, &src, &out.formatted);
            } else if changed {
                std::fs::write(&path, out.formatted.as_bytes())
                    .with_context(|| format!("writing {}", path.display()))?;
            }
        }
    }

    let fail = (args.check && any_changed) || (deny_warnings && any_warning);
    Ok(if fail {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn run_lint(args: LintArgs) -> Result<ExitCode> {
    let explicit_config = args
        .config
        .as_deref()
        .map(LintConfig::from_file)
        .transpose()
        .context("loading --config")?;

    let mut discovery = LintDiscovery::new(explicit_config);
    let mut any_finding = false;

    if args.paths.is_empty() {
        // stdin
        let mut src = String::new();
        std::io::stdin()
            .read_to_string(&mut src)
            .context("reading stdin")?;
        let cfg = discovery.for_dir(&std::env::current_dir().context("getting current dir")?)?;
        let out = jals_lint::lint_source(&src, &cfg);
        any_finding |= report::report_lint("<stdin>", &src, &out);
    } else {
        for path in collect_java_files(&args.paths)? {
            let src = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            let cfg = discovery.for_dir(parent)?;
            let out = jals_lint::lint_source(&src, &cfg);
            any_finding |= report::report_lint(&path.display().to_string(), &src, &out);
        }
    }

    Ok(if any_finding {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Runs the language server over stdio until the client disconnects.
fn run_lsp(_args: LspArgs) -> Result<ExitCode> {
    jals_lsp::run()?;
    Ok(ExitCode::SUCCESS)
}

/// Compiles the project: discovers the manifest and sources, builds the `javac` invocation, and
/// either prints it (`--dry-run`) or spawns `javac` and maps its exit code.
fn run_build(args: BuildArgs) -> Result<ExitCode> {
    let (mut manifest, root) = resolve_manifest(args.manifest_path.as_deref())?;
    if let Some(out) = &args.out_dir {
        manifest.build.classes_dir = out.to_string_lossy().into_owned();
    }
    let sources = discover_sources(&manifest, &root)?;
    let invocation = jals_build::build_invocation(&manifest, &root, &sources, path_sep());

    if args.dry_run || args.verbose {
        println!("{}", invocation.display_command());
    }
    if args.dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    let javac = jdk_tool("JAVAC", "javac");
    let status = spawn_tool(&javac, &invocation.args)?;
    Ok(to_exit_code(status))
}

/// Compiles the project, then runs its main class with `java`. Compilation must succeed before the
/// run; `--dry-run` prints both commands without executing either.
fn run_run(args: RunArgs) -> Result<ExitCode> {
    let (manifest, root) = resolve_manifest(args.manifest_path.as_deref())?;
    let main_class = args
        .main_class
        .clone()
        .or_else(|| manifest.run.main_class.clone())
        .ok_or_else(|| {
            anyhow!("no main class: set `[run] main-class` in jals.toml or pass --main-class")
        })?;
    let sources = discover_sources(&manifest, &root)?;
    let sep = path_sep();
    let build_inv = jals_build::build_invocation(&manifest, &root, &sources, sep);
    let run_inv = jals_build::run_invocation(&manifest, &root, &main_class, &args.args, sep);

    if args.dry_run || args.verbose {
        println!("{}", build_inv.display_command());
        println!("{}", run_inv.display_command());
    }
    if args.dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    // Compile first; only run when compilation succeeds.
    let javac = jdk_tool("JAVAC", "javac");
    let build_status = spawn_tool(&javac, &build_inv.args)?;
    if !build_status.success() {
        return Ok(to_exit_code(build_status));
    }
    let java = jdk_tool("JAVA", "java");
    let run_status = spawn_tool(&java, &run_inv.args)?;
    Ok(to_exit_code(run_status))
}

/// Removes the project's build output: discovers the manifest, resolves the artifact paths, and
/// deletes each existing directory (a missing one is simply skipped, so cleaning a never-built
/// project succeeds quietly). `--dry-run` prints the paths without deleting them.
fn run_clean(args: CleanArgs) -> Result<ExitCode> {
    let (manifest, root) = resolve_manifest(args.manifest_path.as_deref())?;
    let paths = jals_build::clean_paths(&manifest, &root);

    for path in &paths {
        if args.dry_run {
            println!("would remove {}", path.display());
            continue;
        }
        if !path.exists() {
            continue;
        }
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
        println!("removed {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// Scaffolds a new project: resolves the target directory and name, then writes the files from
/// [`jals_build::scaffold`]. Refuses to overwrite an existing `jals.toml`; any other pre-existing
/// scaffold file (e.g. a hand-written `Main.java`) is left untouched.
fn run_init(args: InitArgs) -> Result<ExitCode> {
    let dir = match args.path {
        Some(p) => p,
        None => std::env::current_dir().context("getting current dir")?,
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    if dir.join("jals.toml").exists() {
        return Err(anyhow!("`jals.toml` already exists in {}", dir.display()));
    }

    let name = match args.name {
        Some(n) => n,
        None => project_name_from_dir(&dir)?,
    };

    let files = jals_build::scaffold(&jals_build::InitOptions { name: name.clone() });
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

/// Infers a project name from a target directory's final component, canonicalizing first so a
/// relative path or `.` resolves to the directory's real name rather than the literal `.`.
fn project_name_from_dir(dir: &Path) -> Result<String> {
    let absolute = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    absolute
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            anyhow!(
                "could not infer a project name from {}; pass --name",
                dir.display()
            )
        })
}

/// Resolves the manifest from an explicit path or by discovering `jals.toml` upward from the cwd,
/// returning the parsed manifest and the project root (the manifest's parent directory). A missing
/// manifest is an error, unlike the formatter/linter configs.
fn resolve_manifest(explicit: Option<&Path>) -> Result<(Manifest, PathBuf)> {
    let manifest_path = match explicit {
        Some(p) => p.to_path_buf(),
        None => {
            let cwd = std::env::current_dir().context("getting current dir")?;
            Manifest::discover_path(&cwd)
                .ok_or_else(|| anyhow!("no `jals.toml` found in {} or any parent", cwd.display()))?
        }
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
    let source_roots: Vec<PathBuf> = manifest
        .build
        .source_dirs
        .iter()
        .map(|d| root.join(d))
        .collect();
    for dir in &source_roots {
        if !dir.is_dir() {
            return Err(anyhow!("source directory {} does not exist", dir.display()));
        }
    }
    let sources = collect_java_files(&source_roots)?;
    if sources.is_empty() {
        return Err(anyhow!(
            "no .java files found under {:?}",
            manifest.build.source_dirs
        ));
    }
    Ok(sources)
}

/// The platform classpath separator.
fn path_sep() -> char {
    if cfg!(windows) { ';' } else { ':' }
}

/// Resolves a JDK tool (`javac`/`java`): honor `$<env>` first, then `$JAVA_HOME/bin/<tool>`, and
/// finally fall back to the bare tool name on `PATH`.
fn jdk_tool(env: &str, tool: &str) -> PathBuf {
    if let Some(explicit) = std::env::var_os(env) {
        return PathBuf::from(explicit);
    }
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let candidate = Path::new(&home).join("bin").join(tool);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(tool)
}

/// Spawns `program` with `args`, inheriting stdio so the tool's diagnostics pass straight through.
fn spawn_tool(program: &Path, args: &[String]) -> Result<std::process::ExitStatus> {
    std::process::Command::new(program)
        .args(args)
        .status()
        .with_context(|| {
            format!(
                "failed to spawn `{}` (is a JDK installed and on PATH?)",
                program.display()
            )
        })
}

/// Maps a process exit status to a CLI [`ExitCode`]: 0 succeeds, any other code propagates, and a
/// signal-terminated process fails with code 1.
fn to_exit_code(status: std::process::ExitStatus) -> ExitCode {
    match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(code as u8),
        None => ExitCode::from(1),
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
        Discovery {
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
        let cfg = Config::discover(dir)
            .with_context(|| format!("discovering config from {}", dir.display()))?;
        self.cache.insert(dir.to_path_buf(), cfg.clone());
        Ok(cfg)
    }
}

/// Collect the files to format: explicit files as-is, directories searched recursively
/// for `.java` files.
fn collect_java_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
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

/// Resolves the lint config for a directory, mirroring [`Discovery`] for [`jals_lint::Config`]:
/// either from an explicit `--config` (used for all files) or by discovering `jalslint.toml`,
/// memoized per directory.
struct LintDiscovery {
    explicit: Option<LintConfig>,
    cache: HashMap<PathBuf, LintConfig>,
}

impl LintDiscovery {
    fn new(explicit: Option<LintConfig>) -> Self {
        LintDiscovery {
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
        let cfg = LintConfig::discover(dir)
            .with_context(|| format!("discovering config from {}", dir.display()))?;
        self.cache.insert(dir.to_path_buf(), cfg.clone());
        Ok(cfg)
    }
}
