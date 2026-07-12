//! `jals-golden` command-line interface: format `*.input` files from a golden
//! corpus with a Google Java Style config and report how close the result is to the
//! paired `*.output` produced by `google-java-format`.
//!
//! This is the formatter-fidelity counterpart to `jals-tests` (which checks parser
//! soundness). See [`jals_tests::golden`] for the metric and the known gaps.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use jals_tests::Harness;
use jals_tests::golden::{GOLDEN_SOURCES, GoldenReport, GoldenSource};

#[derive(Parser)]
#[command(
    name = "jals-golden",
    version,
    about = "Compare jals-fmt (Google-style) output against google-java-format goldens"
)]
struct Cli {
    /// Golden corpora to check, by name. With none given, every known corpus is checked.
    sources: Vec<String>,

    /// Directory holding the source checkouts (defaults to this crate's `sources/`).
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// Check an ad-hoc corpus directory directly (a tree of `*.input`/`*.output`
    /// pairs), ignoring the named sources. Useful for pointing at your own
    /// google-java-format-formatted project.
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,

    /// List the N least-similar files per corpus (0 = none).
    #[arg(long, value_name = "N", default_value_t = 20)]
    worst: usize,

    /// Number of parallel worker threads (defaults to the number of logical CPUs).
    #[arg(short = 'j', long, value_name = "N")]
    jobs: Option<usize>,

    /// Emit a GitHub-flavored Markdown summary instead of plain text.
    #[arg(long)]
    markdown: bool,
}

fn main() -> ExitCode {
    Cli::run()
}

impl Cli {
    /// Parse the arguments, format every selected corpus, and report the results.
    fn run() -> ExitCode {
        let cli = Self::parse();

        if let Err(msg) = Harness::configure_threads(cli.jobs) {
            eprintln!("error: {msg}");
            return ExitCode::from(1);
        }

        let cfg = GoldenReport::google_config();

        // An explicit `--dir` is an ad-hoc corpus: scan it directly, no registry.
        if let Some(dir) = &cli.dir {
            if !dir.is_dir() {
                eprintln!("error: --dir not found at {}", dir.display());
                return ExitCode::from(2);
            }
            let report = GoldenReport::run("dir", dir, &cfg);
            cli.emit(&[report]);
            return ExitCode::SUCCESS;
        }

        let sources_dir = cli
            .root
            .clone()
            .unwrap_or_else(Harness::default_sources_dir);

        let selected: Vec<&str> = if cli.sources.is_empty() {
            GOLDEN_SOURCES.iter().map(|s| s.name).collect()
        } else {
            cli.sources.iter().map(String::as_str).collect()
        };

        let mut any_missing = false;
        let mut reports = Vec::new();

        for name in selected {
            let Some(source) = GoldenSource::by_name(name) else {
                eprintln!(
                    "error: unknown golden corpus `{name}` (known: {})",
                    GOLDEN_SOURCES
                        .iter()
                        .map(|s| s.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                any_missing = true;
                continue;
            };
            let root = sources_dir.join(source.root_rel);
            if !root.is_dir() {
                eprintln!(
                    "error: corpus `{}` not found at {}\n       see jals-tests/README.md for how to fetch / generate it",
                    source.name,
                    root.display()
                );
                any_missing = true;
                continue;
            }
            eprintln!("formatting `{}` under {} ...", source.name, root.display());
            reports.push(GoldenReport::run(source.name, &root, &cfg));
        }

        cli.emit(&reports);

        if any_missing {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        }
    }

    /// Print the reports as plain text or, with `--markdown`, a Markdown table.
    fn emit(&self, reports: &[GoldenReport]) {
        if self.markdown {
            print!("{}", GoldenReport::markdown_report(reports, self.worst));
            return;
        }
        for report in reports {
            self.print_report(report);
            println!();
        }
    }

    fn print_report(&self, report: &GoldenReport) {
        println!("Corpus: {}  ({})", report.name, report.root.display());
        println!("  pairs            {}", report.total);
        println!(
            "  exact matches    {}  ({:.2}%)",
            report.exact,
            report.exact_rate() * 100.0
        );
        println!("  mean similarity  {:.2}%", report.mean_similarity * 100.0);

        if self.worst > 0 && !report.results.is_empty() {
            let shown = self.worst.min(report.results.len());
            println!("  {shown} least similar:");
            for r in report.results.iter().take(shown) {
                println!("    {:6.2}%  {}", r.similarity * 100.0, r.rel.display());
            }
        }
    }
}
