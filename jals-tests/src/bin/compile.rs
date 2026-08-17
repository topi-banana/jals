//! `jals-compile` command-line interface: compile a corpus of real Java with `jals-javac` and
//! report how far each file got — parsed, lowered, read back, and finally accepted by a real JVM's
//! bytecode verifier.
//!
//! This is the compiler counterpart to `jals-tests` (parser soundness) and `jals-golden`
//! (formatter fidelity). See [`jals_tests::compile`] for why javac is the oracle, why the corpus
//! is generated rather than found, and what the denominator excludes.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use jals_tests::Harness;
use jals_tests::compile::{
    COMPILE_SOURCES, CompileReport, CompileSource, JAVAC_PIN, Jdk, Verifier,
};

#[derive(Parser)]
#[command(
    name = "jals-compile",
    version,
    about = "Compile real Java with jals-javac and report how much a real JVM accepts"
)]
struct Cli {
    /// Corpora to compile, by name. With none given, every known corpus is compiled.
    sources: Vec<String>,

    /// Directory holding the source checkouts (defaults to this crate's `sources/`).
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// Compile an ad-hoc corpus directory directly (a tree of `<Base>.java` with a sibling
    /// `<Base>.expected/`), ignoring the named sources.
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,

    /// List the N cases that stopped lowest per corpus (0 = none).
    #[arg(long, value_name = "N", default_value_t = 20)]
    limit: usize,

    /// Number of parallel worker threads (defaults to the number of logical CPUs).
    #[arg(short = 'j', long, value_name = "N")]
    jobs: Option<usize>,

    /// Report the corpora that are present instead of failing on the ones that are not.
    #[arg(long)]
    allow_missing: bool,

    /// Skip the JVM rung: compile and read back, but do not link anything.
    ///
    /// The rate then stops at `re-read`, which proves the assembler is self-consistent and
    /// nothing about whether a JVM would load what it wrote.
    #[arg(long)]
    no_verify: bool,

    /// Emit a GitHub-flavored Markdown summary instead of plain text.
    #[arg(long)]
    markdown: bool,

    /// Exit non-zero when a case violated an invariant (a rejected class file, output that does
    /// not read back, a panic, or a syntax error on valid Java).
    ///
    /// Off by default: the harness reports a rate, and an unimplemented lowering path is not a
    /// failing build. CI's corpus report leaves it off too, since known defects are still open —
    /// turning it on is what makes this a gate rather than a measurement.
    #[arg(long)]
    strict: bool,
}

fn main() -> ExitCode {
    Cli::run()
}

impl Cli {
    /// Parse the arguments, compile every selected corpus, and report the results.
    fn run() -> ExitCode {
        let cli = Self::parse();

        if let Err(message) = Harness::configure_threads(cli.jobs) {
            eprintln!("error: {message}");
            return ExitCode::from(1);
        }

        // The JDK is half the measurement: javac decided which files are in the corpus, and its
        // `ct.sym` supplies the signatures the analysis resolves against. Without one there is
        // nothing to measure, so this is an error rather than a fallback onto the embedded stubs —
        // those would report stub coverage under a compiler's name.
        let Some(jdk) = Jdk::detect() else {
            eprintln!(
                "error: no JDK on this host (`java` is not on PATH)\n       \
                 the corpus is defined against one: javac decides its scope and ct.sym is its classpath"
            );
            return ExitCode::from(2);
        };
        eprintln!("using JDK {} at {} ...", jdk.version, jdk.home.display());
        // Not the pinned release, so this run is not the measurement the numbers are defined as:
        // javac chose the scope under one JDK and `ct.sym` supplied the classpath under another.
        // The report's `reference` column states the host's version, which is why an unpinned host
        // has to say so here — the alternative is a rate labelled with a release it was not scored
        // against. A warning rather than a refusal: a local run on whatever JDK is installed is
        // still worth having, it just is not the number CI publishes.
        if !JAVAC_PIN.parse::<u32>().is_ok_and(|pin| pin == jdk.version) {
            eprintln!(
                "warning: the corpus is defined against JDK {JAVAC_PIN}; rates from this run are \
                 not comparable with the pinned ones"
            );
        }
        let (classpath, signatures) = match jdk.classpath() {
            Ok(classpath) => classpath,
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::from(2);
            }
        };
        eprintln!("classpath: {signatures} signatures from ct.sym");

        let verify = !cli.no_verify && Verifier::jvm_available();

        // An explicit `--dir` is an ad-hoc corpus: compile it directly, no registry.
        if let Some(dir) = &cli.dir {
            if !dir.is_dir() {
                eprintln!("error: --dir not found at {}", dir.display());
                return ExitCode::from(2);
            }
            let Some(report) = Self::compile_one("dir", dir, &jdk, &classpath, verify) else {
                return ExitCode::from(1);
            };
            let reports = std::slice::from_ref(&report);
            cli.emit(reports);
            return cli.exit_code(reports, false);
        }

        let sources_dir = cli
            .root
            .clone()
            .unwrap_or_else(Harness::default_sources_dir);

        let selected: Vec<&str> = if cli.sources.is_empty() {
            COMPILE_SOURCES.iter().map(|source| source.name).collect()
        } else {
            cli.sources.iter().map(String::as_str).collect()
        };

        let mut any_missing = false;
        let mut reports = Vec::new();

        for name in selected {
            let Some(source) = CompileSource::by_name(name) else {
                eprintln!(
                    "error: unknown corpus `{name}` (known: {})",
                    COMPILE_SOURCES
                        .iter()
                        .map(|source| source.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                any_missing = true;
                continue;
            };
            let root = sources_dir.join(source.root_rel);
            if !root.is_dir() {
                let level = if cli.allow_missing { "note" } else { "error" };
                eprintln!(
                    "{level}: corpus `{}` not found at {}\n       \
                     generate it: jals-tests/scripts/gen-javac-corpus.sh (see jals-tests/README.md)",
                    source.name,
                    root.display()
                );
                any_missing |= !cli.allow_missing;
                continue;
            }
            eprintln!("compiling `{}` under {} ...", source.name, root.display());
            if let Some(report) = Self::compile_one(source.name, &root, &jdk, &classpath, verify) {
                reports.push(report);
            }
        }

        cli.emit(&reports);
        cli.exit_code(&reports, any_missing)
    }

    /// Compile one corpus, staging its output for the JVM rung when `verify` is on.
    fn compile_one(
        name: &str,
        root: &Path,
        jdk: &Jdk,
        classpath: &jals_hir::LoweredClasspath,
        verify: bool,
    ) -> Option<CompileReport> {
        let verifier = match verify.then(|| Verifier::new(root)).transpose() {
            Ok(verifier) => verifier,
            Err(message) => {
                eprintln!("error: {message}");
                return None;
            }
        };
        Some(CompileReport::run(
            name,
            root,
            jdk,
            classpath,
            verifier.as_ref(),
        ))
    }

    /// `2` for a corpus that could not be found, `1` under `--strict` for a violated invariant.
    fn exit_code(&self, reports: &[CompileReport], any_missing: bool) -> ExitCode {
        if any_missing {
            return ExitCode::from(2);
        }
        if self.strict && reports.iter().any(CompileReport::has_invariant_violations) {
            return ExitCode::from(1);
        }
        ExitCode::SUCCESS
    }

    /// Print the reports as plain text or, with `--markdown`, a Markdown summary.
    fn emit(&self, reports: &[CompileReport]) {
        if self.markdown {
            print!("{}", CompileReport::markdown_report(reports, self.limit));
            return;
        }
        for report in reports {
            self.print_report(report);
            println!();
        }
    }

    fn print_report(&self, report: &CompileReport) {
        let total = report.total();
        let rate = |n: usize| {
            if total == 0 {
                0.0
            } else {
                n as f64 * 100.0 / total as f64
            }
        };
        println!("Corpus: {}  ({})", report.name, report.root.display());
        if let Some(source) = CompileSource::by_name(&report.name) {
            println!("  {}", source.description);
        }
        println!("  reference   {}", report.reference);
        println!("  in scope    {total}  (javac compiles each of these on its own)");
        if !report.skipped.is_empty() {
            println!(
                "  out of scope {}  (javac declined them alone; see SKIPPED.tsv)",
                report.skipped.len()
            );
        }
        let [parsed, lowered, reread, verified, descriptor_equal] = report.ladder();
        for (label, count) in [
            ("parsed", parsed),
            ("lowered", lowered),
            ("re-read", reread),
            ("verified", verified),
            ("descriptor-equal", descriptor_equal),
        ] {
            println!("  {label:<11} {count:6}  {:6.2}%", rate(count));
        }

        let violations = report.violations();
        if !violations.is_empty() {
            println!(
                "  {} invariant violation(s) — a class file the JVM rejects, output that does not \
                 read back, or a panic:",
                violations.len()
            );
            let listed = self.limit.max(CompileReport::DEFECTS_ALWAYS_LISTED);
            for result in violations.iter().take(listed) {
                println!(
                    "    {:<12} {}  {}",
                    result.outcome.label(),
                    result.rel.display(),
                    result.outcome.detail().unwrap_or_default(),
                );
            }
        }

        self.print_counts("what stopped the rest:", &report.buckets());
        self.print_counts(
            "why javac declined the out-of-scope files:",
            &report.skip_reasons(),
        );
    }

    /// One `--limit`-bounded count-and-message list, printed only when there is something in it.
    fn print_counts(&self, title: &str, rows: &[(String, usize)]) {
        if rows.is_empty() || self.limit == 0 {
            return;
        }
        println!("  {title}");
        for (message, count) in rows.iter().take(self.limit) {
            println!("    {count:5}  {message}");
        }
    }
}
