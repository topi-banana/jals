//! `jals-tests` command-line interface: parse real Java corpora with
//! `jals_syntax` and report parse-failure rates and the files that fail.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use jals_tests::{ALL_SOURCES, Source, SourceReport, first_error, run_source, source_by_name};

#[derive(Parser)]
#[command(
    name = "jals-tests",
    version,
    about = "Parse real Java corpora and report parse-failure rates"
)]
struct Cli {
    /// Sources to test. With none given, every known source is tested.
    #[arg(value_enum)]
    sources: Vec<SourceArg>,

    /// Directory holding the source checkouts (defaults to this crate's `sources/`).
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// List the path of every file that failed to parse.
    #[arg(short = 'l', long)]
    list_failures: bool,

    /// Max number of failures to list per source (0 = no limit).
    #[arg(long, value_name = "N", default_value_t = 50)]
    limit: usize,

    /// Also show the first syntax error of each listed failure.
    #[arg(long)]
    show_errors: bool,

    /// Number of parallel worker threads (defaults to the number of logical CPUs).
    #[arg(short = 'j', long, value_name = "N")]
    jobs: Option<usize>,
}

/// CLI-facing source names. Kept in sync with [`jals_tests::ALL_SOURCES`].
#[derive(Clone, Copy, ValueEnum)]
enum SourceArg {
    Openjdk,
    Langtools,
}

impl SourceArg {
    fn name(self) -> &'static str {
        match self {
            SourceArg::Openjdk => "openjdk",
            SourceArg::Langtools => "langtools",
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Err(msg) = configure_threads(cli.jobs) {
        eprintln!("error: {msg}");
        return ExitCode::from(1);
    }

    let sources_dir = cli.root.clone().unwrap_or_else(default_sources_dir);

    let selected: Vec<&'static Source> = if cli.sources.is_empty() {
        ALL_SOURCES.iter().collect()
    } else {
        cli.sources
            .iter()
            .map(|s| source_by_name(s.name()).expect("SourceArg name is always known"))
            .collect()
    };

    let mut any_missing = false;
    let mut any_violation = false;

    for source in selected {
        let root = sources_dir.join(source.root_rel);
        if !root.is_dir() {
            eprintln!(
                "error: source `{}` not found at {}\n       run: git submodule update --init --depth 1",
                source.name,
                root.display()
            );
            any_missing = true;
            continue;
        }

        eprintln!("parsing `{}` under {} ...", source.name, root.display());
        let report = run_source(source.name, &root);
        print_report(source, &report, &cli);
        any_violation |= report.has_invariant_violations();
        println!();
    }

    if any_missing {
        ExitCode::from(2)
    } else if any_violation {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Per-worker stack size. Deeply nested Java (long binary-expression chains,
/// nested generics) can overflow the default ~2 MiB stack of a recursive-descent
/// parser, so each worker gets a generous stack.
const WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;

/// Build the global rayon pool with a large per-worker stack and an optional
/// `--jobs` thread count.
fn configure_threads(jobs: Option<usize>) -> Result<(), String> {
    let mut builder = rayon::ThreadPoolBuilder::new().stack_size(WORKER_STACK_SIZE);
    if let Some(jobs) = jobs {
        builder = builder.num_threads(jobs);
    }
    builder
        .build_global()
        .map_err(|e| format!("could not configure the worker thread pool: {e}"))
}

/// The default `sources/` directory, resolved relative to this crate.
fn default_sources_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sources")
}

/// `n` as a percentage of `total`, guarding against division by zero.
fn pct(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        n as f64 * 100.0 / total as f64
    }
}

fn print_report(source: &Source, report: &SourceReport, cli: &Cli) {
    println!("Source: {}  ({})", source.name, report.root.display());
    println!("  {}", source.description);
    println!("  files          {}", report.total);
    println!(
        "  ok             {}  ({:.2}%)",
        report.ok,
        pct(report.ok, report.total)
    );
    println!(
        "  syntax errors  {}  ({:.2}%)",
        report.syntax_errors,
        pct(report.syntax_errors, report.total)
    );
    println!("  non-lossless   {}", report.non_lossless);
    println!("  panicked       {}", report.panicked);
    println!("  read errors    {}", report.read_errors);
    if report.has_invariant_violations() {
        println!("  ** INVARIANT VIOLATION: non-lossless / panicked must be 0 **");
    }

    if cli.list_failures && !report.failures.is_empty() {
        let shown = if cli.limit == 0 {
            report.failures.len()
        } else {
            cli.limit.min(report.failures.len())
        };
        println!("  failures ({} of {} shown):", shown, report.failures.len());
        for (path, outcome) in report.failures.iter().take(shown) {
            let rel = path.strip_prefix(&report.root).unwrap_or(path);
            println!("    [{}] {}", outcome.label(), rel.display());
            if let Some(msg) = cli.show_errors.then(|| first_error(path)).flatten() {
                println!("        {msg}");
            }
        }
        if shown < report.failures.len() {
            println!("    ... and {} more", report.failures.len() - shown);
        }
    }
}
