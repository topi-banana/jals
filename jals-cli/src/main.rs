//! `jals` command-line interface.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use jals_fmt::{Config, FormatOutput};
use jals_lint::{Config as LintConfig, LintOutput};

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
}

#[derive(Args)]
struct FmtArgs {
    /// Files or directories to format. Directories are searched recursively for `.java`
    /// files. With no paths, source is read from stdin and written to stdout.
    paths: Vec<PathBuf>,

    /// Check mode: do not write anything; exit non-zero if any file would change.
    #[arg(long)]
    check: bool,

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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Fmt(args) => run_fmt(args),
        Commands::Lsp(args) => run_lsp(args),
        Commands::Lint(args) => run_lint(args),
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
        any_changed |= out.formatted != src;
        any_warning |= out.has_warnings();
        report_warnings(&out, "<stdin>");
        if !args.check {
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
            report_warnings(&out, &path.display().to_string());

            if args.check {
                if changed {
                    eprintln!("Would reformat: {}", path.display());
                }
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
        any_finding |= report_lint(&out, "<stdin>", &src);
    } else {
        for path in collect_java_files(&args.paths)? {
            let src = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            let cfg = discovery.for_dir(parent)?;
            let out = jals_lint::lint_source(&src, &cfg);
            any_finding |= report_lint(&out, &path.display().to_string(), &src);
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

fn report_warnings(out: &FormatOutput, label: &str) {
    for w in &out.warnings {
        eprintln!(
            "warning: {label}:{}..{}: {}",
            w.range.start, w.range.end, w.message
        );
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

/// Print every diagnostic (and parser error) for one file as `label:line:col: severity[rule]:
/// message`. Returns whether anything was reported.
fn report_lint(out: &LintOutput, label: &str, src: &str) -> bool {
    for d in out.diagnostics.iter().chain(&out.parse_errors) {
        let (line, col) = line_col(src, d.range.start);
        eprintln!(
            "{label}:{line}:{col}: {}[{}]: {}",
            d.severity, d.rule, d.message
        );
    }
    out.has_diagnostics()
}

/// Translate a byte offset into a 1-based (line, column) pair, counting columns in characters.
fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in src.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
