//! `jals-wasm` command-line interface: compile a corpus of real Java to WebAssembly with
//! `jals-javac`'s WasmGC backend and report how far each file got — parsed, lowered, validated,
//! instantiated, and finally in agreement with what javac's own class files answer on a JVM.
//!
//! The WasmGC counterpart of `jals-compile`, over the very same corpus. See [`jals_tests::wasm`]
//! for why there are two denominators, what the agreement rung compares, and why `--strict` is off.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use jals_tests::Harness;
use jals_tests::compile::{COMPILE_SOURCES, CompileSource, JAVAC_PIN, Jdk};
use jals_tests::wasm::{Engine, Oracle, WasmReport};

#[derive(Parser)]
#[command(
    name = "jals-wasm",
    version,
    about = "Compile real Java to WebAssembly with jals-javac and report what an engine accepts"
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

    /// Also list every in-subset gap case by name, with its message unelided.
    ///
    /// The bucket list says what the remaining work looks like; this says which file to open to
    /// do it. Off by default because it is one line per case.
    #[arg(long)]
    list_gaps: bool,

    /// Number of parallel worker threads (defaults to the number of logical CPUs).
    #[arg(short = 'j', long, value_name = "N")]
    jobs: Option<usize>,

    /// Seconds any one engine invocation may run before it is abandoned.
    ///
    /// A corpus of compiler tests contains `static` initialisers that do not terminate, and the
    /// start function is exactly where one of those lands.
    #[arg(long, value_name = "SECONDS", default_value_t = 10)]
    timeout: u64,

    /// Report the corpora that are present instead of failing on the ones that are not.
    #[arg(long)]
    allow_missing: bool,

    /// Skip the engine rungs: lower every case, but neither validate nor run what came out.
    ///
    /// The rate then stops at `lowered`, which proves the backend produced bytes and nothing about
    /// whether they are a WebAssembly module.
    #[arg(long)]
    no_validate: bool,

    /// Skip the agreement rung: validate and instantiate, but ask javac's own class files nothing.
    #[arg(long)]
    no_run: bool,

    /// Emit a GitHub-flavored Markdown summary instead of plain text.
    #[arg(long)]
    markdown: bool,

    /// Exit non-zero when a case violated an invariant (a module the validator refuses, a
    /// compiled program that answers something else than javac's, a panic, or a syntax error on
    /// valid Java).
    ///
    /// Off by default, for the reason `jals-compile` leaves it off: known defects are still open,
    /// so the report is a measurement. Turning it on is what makes this a gate.
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

        // The JDK is still half the measurement even for a backend that emits no class file: javac
        // decided which files are in the corpus, its `ct.sym` supplies the signatures the analysis
        // resolves against, and its own output is the agreement rung's oracle.
        let Some(jdk) = Jdk::detect() else {
            eprintln!(
                "error: no JDK on this host (`java` is not on PATH)\n       \
                 the corpus is defined against one: javac decides its scope, ct.sym is its \
                 classpath, and its class files are what the top rung compares against"
            );
            return ExitCode::from(2);
        };
        eprintln!("using JDK {} at {} ...", jdk.version, jdk.home.display());
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

        let engine = match (!cli.no_validate && Engine::available())
            .then(|| Engine::new(Duration::from_secs(cli.timeout)))
            .transpose()
        {
            Ok(engine) => engine,
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::from(2);
            }
        };
        let oracle = match (engine.is_some() && !cli.no_run && Oracle::jvm_available())
            .then(Oracle::new)
            .transpose()
        {
            Ok(oracle) => oracle,
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::from(2);
            }
        };

        if let Some(dir) = &cli.dir {
            if !dir.is_dir() {
                eprintln!("error: --dir not found at {}", dir.display());
                return ExitCode::from(2);
            }
            let report = WasmReport::run(
                "dir",
                dir,
                &jdk,
                &classpath,
                engine.as_ref(),
                oracle.as_ref(),
            );
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
            reports.push(WasmReport::run(
                source.name,
                &root,
                &jdk,
                &classpath,
                engine.as_ref(),
                oracle.as_ref(),
            ));
        }

        cli.emit(&reports);
        cli.exit_code(&reports, any_missing)
    }

    /// `2` for a corpus that could not be found, `1` under `--strict` for a violated invariant.
    fn exit_code(&self, reports: &[WasmReport], any_missing: bool) -> ExitCode {
        if any_missing {
            return ExitCode::from(2);
        }
        if self.strict && reports.iter().any(WasmReport::has_invariant_violations) {
            return ExitCode::from(1);
        }
        ExitCode::SUCCESS
    }

    /// Print the reports as plain text or, with `--markdown`, a Markdown summary.
    fn emit(&self, reports: &[WasmReport]) {
        if self.markdown {
            print!("{}", WasmReport::markdown_report(reports, self.limit));
            return;
        }
        for report in reports {
            self.print_report(report);
            println!();
        }
    }

    fn print_report(&self, report: &WasmReport) {
        let subset = report.in_subset();
        let rate = |n: usize| {
            if subset == 0 {
                0.0
            } else {
                n as f64 * 100.0 / subset as f64
            }
        };
        println!("Corpus: {}  ({})", report.name, report.root.display());
        if let Some(source) = CompileSource::by_name(&report.name) {
            println!("  {}", source.description);
        }
        println!("  reference   {}", report.reference);
        println!(
            "  in corpus   {}  (javac compiles each of these on its own)",
            report.total()
        );
        println!(
            "  out of subset {}  (a library type this backend has no `java.base` to supply)",
            report.out_of_subset()
        );
        println!("  in subset   {subset}  (the denominator every rate below is over)");

        let [parsed, lowered, validated, instantiated, agreed] = report.ladder();
        for (label, count) in [
            ("parsed", parsed),
            ("lowered", lowered),
            ("validated", validated),
            ("instantiated", instantiated),
            ("agreed", agreed),
        ] {
            println!("  {label:<13} {count:6}  {:6.2}%", rate(count));
        }
        let (valued, completions) = report.comparisons();
        println!(
            "  comparisons   {valued} returned value(s), {completions} completion(s) — a value \
             that matches is a computed answer, a completion only says neither side failed"
        );

        let violations = report.violations();
        if !violations.is_empty() {
            println!(
                "  {} invariant violation(s) — a module the validator refuses, a program that \
                 answers something else than javac's, or a panic:",
                violations.len()
            );
            let listed = self.limit.max(WasmReport::DEFECTS_ALWAYS_LISTED);
            for result in violations.iter().take(listed) {
                println!(
                    "    {:<14} {}  {}",
                    result.outcome.label(),
                    result.rel.display(),
                    result.outcome.detail().unwrap_or_default(),
                );
            }
        }

        let agreements = report.agreements();
        if !agreements.is_empty() && self.limit > 0 {
            println!(
                "  {} case(s) the agreement rung judged — everything the top rung actually rests \
                 on:",
                agreements.len()
            );
            for result in agreements.iter().take(self.limit) {
                println!(
                    "    {}  {} value(s), {} completion(s)",
                    result.rel.display(),
                    result.valued,
                    result.completions
                );
            }
        }

        let trapped = report.trapped();
        if !trapped.is_empty() && self.limit > 0 {
            println!(
                "  {} case(s) whose start function trapped — a `static` initialiser that threw:",
                trapped.len()
            );
            for result in trapped.iter().take(self.limit) {
                println!(
                    "    {}  {}",
                    result.rel.display(),
                    result.outcome.detail().unwrap_or_default()
                );
            }
        }

        self.print_counts("what stopped the rest:", &report.buckets());
        self.print_gaps(report);
        self.print_counts(
            "why the agreement rung compared nothing:",
            &report.unjudged_reasons(),
        );
        self.print_counts(
            "the types that put a case outside the subset:",
            &report.out_of_subset_types(),
        );
    }

    /// Every in-subset gap case by name, with its message unelided — `--list-gaps` only.
    ///
    /// Deliberately unbounded by `--limit`: `--limit` bounds a listing chosen for a summary, and
    /// this one is asked for by name to be worked through.
    fn print_gaps(&self, report: &WasmReport) {
        if !self.list_gaps {
            return;
        }
        let gaps = report.gaps();
        if gaps.is_empty() {
            return;
        }
        println!("  {} gap case(s):", gaps.len());
        for result in gaps {
            println!(
                "    {:<14} {}  {}",
                result.outcome.label(),
                result.rel.display(),
                result.outcome.detail().unwrap_or_default(),
            );
        }
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
