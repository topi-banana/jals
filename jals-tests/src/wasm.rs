//! WasmGC backend end-to-end verification.
//!
//! [`compile`](crate::compile) asks how much real Java `jals-javac` turns into class files a real
//! JVM accepts. This module asks the same question of the *other* backend: how much of it becomes
//! a WebAssembly module the specification's own validator accepts, an engine instantiates, and —
//! where the two compilers offer the same callable surface — that answers what javac's own class
//! files answer on a real JVM.
//!
//! # The corpus is the compiler corpus, unchanged
//!
//! Both backends compile the same language from the same front end, so a second corpus would be a
//! second denominator for one question. `jals-wasm` walks `sources/javac-langtools` — the cases
//! `scripts/gen-javac-corpus.sh` generated for [`compile`](crate::compile), each a `.java` javac
//! compiled on its own beside the `expected/` class files it produced. Nothing here generates
//! anything, and the `expected/` directory is read for the same reason the other harness reads it:
//! it is javac's own answer, sitting beside the question.
//!
//! # The ladder
//!
//! | rung | what it proves |
//! | --- | --- |
//! | parsed | `jals-syntax` accepted the source with no syntax error |
//! | lowered | [`CompileWasm::project`] produced a module rather than a [`WasmError`] |
//! | validated | `wasm-tools validate` accepted it — the specification's own authority |
//! | instantiated | `wasmtime` instantiated it, which is where the start function runs |
//! | agreed | every jointly-callable method answered what javac's class file answers on a JVM |
//!
//! `validated` is this harness's `verified`. The encoder writes its own type indices, block types
//! and local counts, and nothing upstream of a validator has an opinion on whether they cohere —
//! `Module::finish` will happily encode a body whose stack does not balance.
//!
//! `instantiated` is a rung and not a formality: a Java `static` initialiser is lowered into the
//! module's start function, so instantiating is the cheapest way to *run* the lowering of every
//! case that has one, with no entry point to choose and no arguments to invent.
//!
//! `agreed` is the rung a validator structurally cannot reach, and it is the wasm counterpart of
//! [`descriptor-equal`](crate::compile::Outcome::Verified): a module can be perfectly well-typed
//! and compute the wrong number. Only a second opinion catches that, and the corpus already holds
//! one. See [`CaseResult::agreement`] for exactly what is compared, which is much narrower than
//! "ran the way javac's class files run".
//!
//! # Two denominators, because the backend has a target subset
//!
//! [`WasmError::NoRepresentation`] is not a gap: a wasm host has no `java.base`, so a file naming
//! `String` is *outside what this backend compiles* by design, exactly as a file javac declines
//! alone is outside the other harness's corpus. Those cases are reported as
//! [`out of subset`](Outcome::OutOfSubset) and excluded from the rate that measures the compiler —
//! and the whole-corpus rate is printed beside it, so the scoped one can never read as coverage of
//! Java.
//!
//! The classification is **post hoc and order-dependent**: lowering reports the first thing it
//! cannot do, so a file that both names `String` *and* declares an `@interface` lands in whichever
//! the lowering reached first. That is a property of the traversal and not of the source, and it is
//! stated rather than solved — nothing here re-lowers a case to find out what else it would have
//! said.
//!
//! # Pins
//!
//! Three tools decide this measurement and all three are pinned. javac still decides the corpus
//! and `ct.sym` is still the classpath ([`JAVAC_PIN`](crate::compile::JAVAC_PIN)); on top of that
//! [`WASM_TOOLS_PIN`] is the validator whose message text the failure buckets are keyed on, and
//! [`WASMTIME_PIN`] is the engine. A validator bump silently re-partitions the report, which is
//! why the pin is a value with a test behind it rather than a convention.
//!
//! # `--strict` is off, and why
//!
//! Eight cases in the current corpus emit a module `wasm-tools` refuses, so the report is a
//! measurement rather than a gate. Turning `--strict` on is the decision to take once that list is
//! empty — the same call [`compile`](crate::compile) leaves open.

use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use jals_classfile::ClassFile;
use jals_hir::{FileAnalysis, FileId, LoweredClasspath, ProjectIndex};
use jals_javac::wasm::{CompileWasm, ExportKind, WasmError};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::compile::Jdk;

/// The `wasm-tools` release the failure buckets are defined against.
///
/// The validator's message text *is* the bucket key, so two releases that word one rejection
/// differently partition the same findings into different rows. Pinned like a formatter release,
/// and checked against the workflow by [`the pins match CI`](tests::the_pins_match_ci).
pub(crate) const WASM_TOOLS_PIN: &str = "1.258.0";

/// The `wasmtime` release the `instantiated` and `agreed` rungs are defined against.
pub(crate) const WASMTIME_PIN: &str = "48.0.1";

/// How far one case got up the ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Every jointly-callable method answered what javac's class file answers — the top rung.
    Agreed,
    /// A jointly-callable method answered something else. The module is well-typed and the engine
    /// ran it, so this is a miscompile rather than a malformed module: the one finding on this
    /// ladder that says the compiled program computes the wrong thing.
    Disagreed(String),
    /// The module instantiated and the agreement rung compared nothing, for the reason carried.
    ///
    /// Its own outcome rather than a pass, for the reason
    /// [`DescriptorsUnjudged`](crate::compile::Outcome::DescriptorsUnjudged) is: folding
    /// "nothing compared" into "agreed" makes the top rung fail *open*, and the top rung is where
    /// an inflated number does the most damage.
    Unjudged(Unjudged),
    /// The module instantiated and the agreement rung did not run (`--no-run`, or no JVM).
    NotRun,
    /// The module validated, but instantiating it trapped — which means the start function did,
    /// and the start function is where a Java `static` initialiser lives.
    ///
    /// Not a defect: a `<clinit>` that throws is legal Java, and the corpus is a compiler's own
    /// test suite. It is a rung stop, listed so the trap is visible rather than averaged away.
    Trapped(String),
    /// `wasm-tools` refused the module. The bytes are not a WebAssembly module, and this is the
    /// finding the harness exists to produce.
    Rejected(String),
    /// The module was emitted but the engine stage did not run (`--no-validate`, or no tools).
    Unvalidated,
    /// Lowering refused the file because it names a type this backend does not represent — a
    /// library type, which a wasm host has no `java.base` to supply. Out of the target subset by
    /// design, and never a failure.
    OutOfSubset(String),
    /// The module outgrew a WebAssembly format limit and the backend said so rather than
    /// truncating. A refusal, not a defect.
    TooLarge,
    /// Lowering refused the file because the backend does not emit that construct yet.
    ///
    /// The message is a fixed `&'static str` naming the construct, so it *is* its own bucket —
    /// eliding it the way a resolution failure is elided would turn "an `@interface` declaration"
    /// into "an `…` declaration" and throw away the only thing the row says.
    Unsupported(&'static str),
    /// Lowering refused the file because a name did not resolve. The message quotes the corpus's
    /// own identifier, which is elided so equivalent failures bucket together.
    Unresolved(String),
    /// Lowering refused the file because a method it *declares* has no body in the module — a
    /// `native` one, or an interface method whose only implementation in the source is a lambda or a
    /// method reference this backend does not lower into a struct.
    ///
    /// Kept apart from [`OutOfSubset`](Self::OutOfSubset), which is the library-type refusal: the
    /// owner here is a project type, so the case is squarely *inside* the subset and this is a gap
    /// in the backend. Scoring it as out-of-subset would shrink the denominator by exactly the cases
    /// the backend cannot do, which is the one direction a rate must never move on its own.
    NoImplementation(String),
    /// The parser reported syntax errors on a file javac compiled — a parser gap, since every file
    /// in the corpus is valid Java by construction.
    ParseError(usize),
    /// The pipeline panicked: a hard invariant violation, and never an acceptable outcome.
    Panicked,
    /// The case's source could not be read. A host problem, kept apart from every rung.
    ReadError,
}

impl Outcome {
    /// A short, stable label for display and for the failure buckets.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Agreed => "agreed",
            Self::Disagreed(_) => "disagreed",
            Self::Unjudged(_) => "unjudged",
            Self::NotRun => "not-run",
            Self::Trapped(_) => "trapped",
            Self::Rejected(_) => "wasm-rejected",
            Self::Unvalidated => "unvalidated",
            Self::OutOfSubset(_) => "out-of-subset",
            Self::TooLarge => "too-large",
            Self::Unsupported(_) => "unsupported",
            Self::Unresolved(_) => "unresolved",
            Self::NoImplementation(_) => "no-implementation",
            Self::ParseError(_) => "parse-error",
            Self::Panicked => "panicked",
            Self::ReadError => "read-error",
        }
    }

    /// The detail worth printing beside the label, if any.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Disagreed(message)
            | Self::Trapped(message)
            | Self::Rejected(message)
            | Self::Unresolved(message)
            | Self::NoImplementation(message)
            | Self::OutOfSubset(message) => Some(message),
            Self::Unsupported(what) => Some(what),
            Self::Unjudged(reason) => Some(reason.detail()),
            _ => None,
        }
    }

    /// Whether the case is inside the subset this backend compiles.
    ///
    /// The rate that measures the compiler is taken over these; the whole-corpus rate is printed
    /// beside it so the scoped one cannot read as coverage of Java.
    const fn in_subset(&self) -> bool {
        !matches!(self, Self::OutOfSubset(_) | Self::ReadError)
    }

    /// Whether the source parsed with no syntax error.
    const fn parsed(&self) -> bool {
        !matches!(self, Self::ParseError(_) | Self::Panicked | Self::ReadError)
    }

    /// Whether lowering produced a module.
    const fn lowered(&self) -> bool {
        self.parsed()
            && !matches!(
                self,
                Self::Unsupported(_)
                    | Self::Unresolved(_)
                    | Self::NoImplementation(_)
                    | Self::TooLarge
                    | Self::OutOfSubset(_)
            )
    }

    /// Whether `wasm-tools` accepted the module.
    const fn validated(&self) -> bool {
        self.lowered() && !matches!(self, Self::Rejected(_) | Self::Unvalidated)
    }

    /// Whether an engine instantiated it, running the start function.
    const fn instantiated(&self) -> bool {
        self.validated() && !matches!(self, Self::Trapped(_))
    }

    /// Whether every jointly-callable method agreed with javac's own — the top rung.
    const fn agreed(&self) -> bool {
        matches!(self, Self::Agreed)
    }

    /// Whether a violated invariant produced this outcome, rather than an unimplemented path.
    ///
    /// These are the outcomes that should fail a run rather than lower a percentage: a panic, a
    /// module the validator refuses, a syntax error on a file that is valid Java by construction,
    /// and a compiled program that computes something else than javac's.
    const fn is_invariant_violation(&self) -> bool {
        matches!(
            self,
            Self::Panicked | Self::Rejected(_) | Self::ParseError(_) | Self::Disagreed(_)
        )
    }

    /// Which rung this stopped at, lowest first — the sort key that puts defects above gaps.
    const fn rung(&self) -> u8 {
        match self {
            Self::Panicked => 0,
            Self::Rejected(_) => 1,
            Self::Disagreed(_) => 2,
            Self::ParseError(_) => 3,
            Self::TooLarge => 4,
            Self::Unsupported(_) | Self::Unresolved(_) | Self::NoImplementation(_) => 5,
            Self::Trapped(_) => 6,
            Self::Unvalidated => 7,
            Self::NotRun => 8,
            Self::Agreed => 9,
            // Not rungs at all: one compared nothing, one is outside what this backend compiles,
            // and one is a case the harness never saw the source of.
            Self::Unjudged(_) => 10,
            Self::OutOfSubset(_) => 11,
            Self::ReadError => 12,
        }
    }

    /// The message with its quoted source snippet elided, so equivalent failures bucket together.
    ///
    /// `` `names.stream()` did not resolve `` and `` `x.y()` did not resolve `` are one gap. The
    /// validator's own messages carry a byte offset and a printed type name in place of a quoted
    /// snippet, so those are elided too — eight rejections that are one shape must not read as
    /// eight findings.
    ///
    /// Buckets cover the **gaps** only. A defect is listed in full in its own section, and counting
    /// it here as well would show one refused module as two findings.
    fn bucket(&self) -> Option<String> {
        match self {
            // Listed in full in a section of its own; counting it here too would show one finding
            // as two.
            _ if self.is_invariant_violation() => None,
            // Both have a section of their own, and one finding must not read as two.
            Self::Trapped(_) | Self::Unjudged(_) => None,
            // A fixed construct name is already the bucket, and eliding it would erase it.
            Self::Unsupported(what) => Some((*what).to_owned()),
            _ => Some(Self::elide(self.detail()?)),
        }
    }

    /// One message with everything case-specific taken out of it.
    ///
    /// Three shapes carry a case's own identity: a backtick-quoted snippet (the corpus's
    /// identifiers), `wasm-tools`' `(at offset 0x…)`, and the function index in its `func 1 failed
    /// to validate`. Eight rejections of one shape must read as one row, so all three are elided
    /// and whatever is left is the finding.
    fn elide(message: &str) -> String {
        let mut out = String::with_capacity(message.len());
        let mut rest = message;
        while let Some((head, tail)) = rest.split_once('`') {
            let Some((_, tail)) = tail.split_once('`') else {
                break;
            };
            out.push_str(head);
            out.push_str("`…`");
            rest = tail;
        }
        out.push_str(rest);

        let mut collapsed = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(at) = rest.find("(at offset 0x") {
            let Some(end) = rest[at..].find(')') else {
                break;
            };
            collapsed.push_str(&rest[..at]);
            collapsed.push_str("(at offset …)");
            rest = &rest[at + end + 1..];
        }
        collapsed.push_str(rest);

        // `error: func 1 failed to validate` — the index names the case's own function, not the
        // shape of the rejection.
        let mut out = String::with_capacity(collapsed.len());
        let mut rest = collapsed.as_str();
        while let Some(at) = rest.find("func ") {
            let after = &rest[at + "func ".len()..];
            let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            out.push_str(&rest[..at]);
            out.push_str("func ");
            if digits == 0 {
                rest = after;
                continue;
            }
            out.push('…');
            rest = &after[digits..];
        }
        out.push_str(rest);
        out
    }
}

/// Why the agreement rung compared nothing for a case that instantiated.
///
/// Three situations, and only the first is about the corpus. Keeping them apart is what stopped the
/// same sentence from standing for a missing oracle, a missing callable surface, and a naming rule
/// that cannot pick between two methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unjudged {
    /// The case's `expected/` directory yielded no class file this workspace's reader could read.
    Unreadable,
    /// Nothing the module exports is a method javac also declares `static` with an all-primitive
    /// parameter list. The overwhelmingly common answer on this corpus, because a Java entry point
    /// takes `String[]` and a file naming `String` never reaches this rung at all.
    NoCallableMethod,
    /// The module exports a bare method name javac declares more than once, so there is no single
    /// method the export can be paired with. A finding about the *export naming rule* rather than
    /// about agreement, and it is why the rung declines instead of picking one.
    AmbiguousExport,
    /// Every call failed on both sides — the module trapped and javac's own class file threw.
    ///
    /// Not agreement, deliberately. A trap and a thrown exception are different events, and this
    /// rung does not yet claim they are the same one: folding them together is what would hide a
    /// missing bounds check reading garbage where the JVM throws.
    BothFailed,
}

impl Unjudged {
    /// The sentence printed beside the case, which is the whole of what this outcome says.
    const fn detail(self) -> &'static str {
        match self {
            Self::Unreadable => "javac produced no class file this harness could read",
            Self::NoCallableMethod => {
                "the module exports no method javac declares static over primitives"
            }
            Self::AmbiguousExport => {
                "an exported bare name is a method javac declares more than once"
            }
            Self::BothFailed => "every call trapped here and threw under javac's own class file",
        }
    }
}

/// One corpus case: a `.java` file javac compiled on its own.
struct Case {
    /// The source path, relative to the corpus root.
    rel: PathBuf,
    /// The absolute source path.
    path: PathBuf,
}

impl Case {
    /// Where a case's javac-produced class files live — and what makes it a case at all.
    fn expected_dir(source: &Path) -> PathBuf {
        source.with_extension("expected")
    }
}

/// What one case produced.
#[derive(Debug, Clone)]
pub struct CaseResult {
    /// The source path, relative to the corpus root.
    pub rel: PathBuf,
    /// How far it got.
    pub outcome: Outcome,
    /// The module's bytes, while the engine stages still need them.
    module: Option<Vec<u8>>,
    /// The methods this case offers both compilers, empty until the module validates.
    callable: Vec<Callable>,
    /// Whether javac's own class files for the case read at all — the difference between "there
    /// was no oracle" and "the oracle declares nothing this rung can call".
    expected_readable: bool,
    /// How many of this case's comparisons read a returned value rather than only completion.
    pub valued: usize,
    /// How many read completion alone, because the method returns `void`.
    pub completions: usize,
}

/// One method the module exports and javac declares, so both compilers can be asked to run it.
#[derive(Debug, Clone)]
struct Callable {
    /// The exported name, which is also javac's method name.
    name: String,
    /// The binary name of the class javac declared it in.
    owner: String,
    /// javac's descriptor, which is where the argument types come from.
    descriptor: String,
}

/// Aggregated WasmGC outcomes for one corpus.
#[derive(Debug, Clone)]
pub struct WasmReport {
    /// Corpus name.
    pub name: String,
    /// The three tools this measurement is defined against.
    pub reference: String,
    /// Resolved root directory that was walked.
    pub root: PathBuf,
    /// Every case's result, worst rung first.
    results: Vec<CaseResult>,
}

impl WasmReport {
    /// How many defects a report lists however small `--limit` is.
    pub const DEFECTS_ALWAYS_LISTED: usize = 20;

    /// Every `.java` under `root` with a sibling `<Base>.expected/` — the generator's own pairs.
    fn collect_cases(root: &Path) -> Vec<Case> {
        let mut cases: Vec<Case> = WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(walkdir::DirEntry::into_path)
            .filter(|path| path.extension().is_some_and(|ext| ext == "java"))
            .filter(|path| Case::expected_dir(path).is_dir())
            .map(|path| Case {
                rel: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
                path,
            })
            .collect();
        cases.sort_by(|a, b| a.rel.cmp(&b.rel));
        cases
    }

    /// Lower every case under `root`, then hand what came out to the engine and to the oracle.
    ///
    /// `engine` is the validate/instantiate stage and `oracle` the agreement rung; `None` for
    /// either leaves the cases at the rung below, shown as its own row rather than folded into a
    /// pass rate.
    pub fn run(
        name: &str,
        root: &Path,
        jdk: &Jdk,
        classpath: &LoweredClasspath,
        engine: Option<&Engine>,
        oracle: Option<&Oracle>,
    ) -> Self {
        let cases = Self::collect_cases(root);
        let mut results: Vec<CaseResult> = cases
            .par_iter()
            .map(|case| CaseResult::of(case, classpath))
            .collect();

        if let Some(engine) = engine {
            engine.judge_all(&mut results);
        }
        // The agreement rung invokes the very modules the engine staged, so it needs both. With no
        // engine there is nothing to call, and the cases stay at `unvalidated`.
        if let (Some(engine), Some(oracle)) = (engine, oracle) {
            oracle.judge_all(root, &mut results, engine);
        }
        // The bytes were only ever needed by the two stages above.
        for result in &mut results {
            result.module = None;
        }

        results.sort_by_key(|result| (result.outcome.rung(), result.rel.clone()));

        Self {
            name: name.to_owned(),
            reference: Self::reference(jdk),
            root: root.to_path_buf(),
            results,
        }
    }

    /// `javac 25 + wasm-tools 1.258.0 + wasmtime 48.0.1` — everything the rate is defined against.
    fn reference(jdk: &Jdk) -> String {
        format!(
            "javac {} + wasm-tools {WASM_TOOLS_PIN} + wasmtime {WASMTIME_PIN}",
            jdk.version
        )
    }

    /// Cases in the corpus — every one of which javac compiles on its own.
    pub fn total(&self) -> usize {
        self.results.len()
    }

    /// Cases inside the subset this backend compiles: the denominator that measures the compiler.
    pub fn in_subset(&self) -> usize {
        self.results
            .iter()
            .filter(|result| result.outcome.in_subset())
            .count()
    }

    /// Cases outside it — a library type this backend has no `java.base` to supply.
    pub fn out_of_subset(&self) -> usize {
        self.results
            .iter()
            .filter(|result| matches!(result.outcome, Outcome::OutOfSubset(_)))
            .count()
    }

    /// The five rung counts, cumulative and lowest first.
    pub fn ladder(&self) -> [usize; 5] {
        let count = |f: fn(&Outcome) -> bool| {
            self.results
                .iter()
                .filter(|result| result.outcome.in_subset() && f(&result.outcome))
                .count()
        };
        [
            count(Outcome::parsed),
            count(Outcome::lowered),
            count(Outcome::validated),
            count(Outcome::instantiated),
            count(Outcome::agreed),
        ]
    }

    /// How many comparisons the agreement rung actually made, split by what each one read.
    ///
    /// Reported apart because they are different claims: a returned value that matches is
    /// "computed the same number", while a `void` method that completes on both sides is only
    /// "neither trapped where the other did not". One number over the two would overstate the rung.
    pub fn comparisons(&self) -> (usize, usize) {
        self.results.iter().fold((0, 0), |(valued, void), result| {
            (valued + result.valued, void + result.completions)
        })
    }

    /// Whether any case violated an invariant, which is what `--strict` exits on.
    pub fn has_invariant_violations(&self) -> bool {
        self.results
            .iter()
            .any(|result| result.outcome.is_invariant_violation())
    }

    /// The invariant violations, worst first.
    pub fn violations(&self) -> Vec<&CaseResult> {
        self.results
            .iter()
            .filter(|result| result.outcome.is_invariant_violation())
            .collect()
    }

    /// The cases the agreement rung judged, and what each one's comparisons read.
    ///
    /// Listed rather than only counted: at this corpus's scale the rung is a handful of methods,
    /// and a reader who cannot see *which* ones cannot tell a rate of 2% that means "two per cent
    /// of the compiler is checked" from one that means "two per cent of the corpus offers anything
    /// to check". It is the second.
    pub fn agreements(&self) -> Vec<&CaseResult> {
        self.results
            .iter()
            .filter(|result| result.outcome == Outcome::Agreed)
            .collect()
    }

    /// The cases whose start function trapped, which is a `static` initialiser that threw.
    pub fn trapped(&self) -> Vec<&CaseResult> {
        self.results
            .iter()
            .filter(|result| matches!(result.outcome, Outcome::Trapped(_)))
            .collect()
    }

    /// The in-subset gap cases one per line, with their messages unelided.
    ///
    /// The counterpart of [`buckets`](Self::buckets) rather than a replacement: a bucket says what
    /// the shape of the remaining work is, and this says which file to open to work on it. A case
    /// outside the subset is not a gap and is not listed — that is the denominator, not the rate —
    /// and neither is one that *lowered*, whose remaining rungs are the engine's answer rather than
    /// the compiler's refusal.
    pub fn gaps(&self) -> Vec<&CaseResult> {
        self.results
            .iter()
            .filter(|result| result.outcome.in_subset() && !result.outcome.lowered())
            .filter(|result| {
                !result.outcome.is_invariant_violation() && result.outcome.detail().is_some()
            })
            .collect()
    }

    /// What stopped the in-subset cases, bucketed by shape.
    pub fn buckets(&self) -> Vec<(String, usize)> {
        Self::tally(
            self.results
                .iter()
                .filter(|result| result.outcome.in_subset())
                .filter_map(|result| result.outcome.bucket()),
        )
    }

    /// Which types put a case outside the subset, most common first.
    pub fn out_of_subset_types(&self) -> Vec<(String, usize)> {
        Self::tally(
            self.results
                .iter()
                .filter_map(|result| match &result.outcome {
                    Outcome::OutOfSubset(ty) => Some(ty.clone()),
                    _ => None,
                }),
        )
    }

    /// Why the agreement rung declined, most common first.
    pub fn unjudged_reasons(&self) -> Vec<(String, usize)> {
        Self::tally(
            self.results
                .iter()
                .filter_map(|result| match &result.outcome {
                    Outcome::Unjudged(reason) => Some(reason.detail().to_owned()),
                    _ => None,
                }),
        )
    }

    /// The whole report as a GitHub-flavored Markdown summary — what CI posts.
    pub fn markdown_report(reports: &[Self], limit: usize) -> String {
        let mut out = String::from("## jals-javac WasmGC end-to-end\n\n");
        // The two-denominator caveat is the one thing that changes how the table is read, so it
        // stays visible; the rest of the prose is the same on every run and sits behind a summary.
        out.push_str(
            "Rates are over the **in-subset** cases; *out of subset* is what this backend does \
             not compile by design, not what it fails to.\n\n",
        );
        out.push_str(
            "| corpus | reference | in corpus | out of subset | in subset | parsed | lowered | \
             validated | instantiated | agreed |\n",
        );
        out.push_str("| --- | --- | --: | --: | --: | --: | --: | --: | --: | --: |\n");
        for report in reports {
            report.push_ladder_row(&mut out);
        }
        out.push_str(
            "\n<details><summary>What this measures, and why there are two denominators\
             </summary>\n\n\
             How much of the *same* corpus `jals-compile` scores turns into a WebAssembly module \
             the specification's validator accepts, an engine instantiates, and — where both \
             compilers offer the same callable surface — that answers what javac's own class files \
             answer on a real JVM. Two denominators, because this backend has a target subset: a \
             file naming a library type is **out of subset** by design (a wasm host has no \
             `java.base`), so the rates are over what is left, and the corpus total is printed \
             beside them so the scoped rate can never read as coverage of Java. `validated` is \
             this ladder's `verified` — nothing upstream of the validator can tell a well-formed \
             module from a plausible one — and `agreed` is the rung above it, since a module can \
             be perfectly well-typed and compute the wrong number.\n\n\
             </details>\n",
        );
        for report in reports {
            report.push_violations(&mut out, limit);
            report.push_agreements(&mut out, limit);
            report.push_trapped(&mut out, limit);
            let buckets = report.buckets();
            report.push_details(
                &mut out,
                &format!("what stopped the rest ({} kinds)", buckets.len()),
                "| cases | reason |\n| --: | --- |\n",
                &buckets,
            );
            report.push_details(
                &mut out,
                "why the agreement rung compared nothing",
                "| cases | reason |\n| --: | --- |\n",
                &report.unjudged_reasons(),
            );
            report.push_details(
                &mut out,
                &format!(
                    "{} out of subset (a type this backend does not represent)",
                    report.out_of_subset()
                ),
                "| cases | type |\n| --: | --- |\n",
                &report.out_of_subset_types(),
            );
        }
        out
    }

    /// This corpus's row of the ladder table.
    fn push_ladder_row(&self, out: &mut String) {
        let subset = self.in_subset();
        let [parsed, lowered, validated, instantiated, agreed] = self.ladder();
        let cell = |n: usize| {
            if subset == 0 {
                "0".to_owned()
            } else {
                format!("{n} ({:.1}%)", n as f64 * 100.0 / subset as f64)
            }
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {subset} | {} | {} | {} | {} | {} |\n",
            self.name,
            self.reference,
            self.total(),
            self.out_of_subset(),
            cell(parsed),
            cell(lowered),
            cell(validated),
            cell(instantiated),
            cell(agreed),
        ));
    }

    /// The defects, every one of them, behind a `<details>` whose summary already names them.
    ///
    /// Collapsed is not hidden, and the distinction is the count: [`DEFECTS_ALWAYS_LISTED`] says a
    /// defect is never dropped from the report by a display setting, and it still is not — the
    /// summary line carries the number and what kind of failure it is, so a reader who never
    /// expands has already been told.
    ///
    /// [`DEFECTS_ALWAYS_LISTED`]: Self::DEFECTS_ALWAYS_LISTED
    fn push_violations(&self, out: &mut String, limit: usize) {
        let violations = self.violations();
        if violations.is_empty() {
            return;
        }
        let (valued, completions) = self.comparisons();
        out.push_str(&format!(
            "\n<details><summary><strong>{}: {} invariant violation(s)</strong> — a module the \
             validator refuses, a compiled program that answers something else than javac's, a \
             panic, or a syntax error on valid Java. These are defects, not unimplemented syntax. \
             The agreement rung made {valued} value comparison(s) and {completions} \
             completion-only comparison(s) in this run.</summary>\n\n",
            self.name,
            violations.len()
        ));
        out.push_str("| outcome | case | detail |\n| --- | --- | --- |\n");
        for result in violations
            .iter()
            .take(limit.max(Self::DEFECTS_ALWAYS_LISTED))
        {
            out.push_str(&format!(
                "| {} | `{}` | {} |\n",
                result.outcome.label(),
                result.rel.display(),
                result.outcome.detail().unwrap_or("—"),
            ));
        }
        out.push_str("\n</details>\n");
    }

    /// The cases the agreement rung judged, by name.
    ///
    /// Counted *and* named, because at this corpus's scale the difference matters: a reader has to
    /// be able to tell a rate of 2% that means "2% of the compiler is checked" from one that means
    /// "2% of the corpus offers anything to check". It is the second.
    fn push_agreements(&self, out: &mut String, limit: usize) {
        let agreements = self.agreements();
        if agreements.is_empty() || limit == 0 {
            return;
        }
        out.push_str(&format!(
            "\n<details><summary>{}: {} case(s) the agreement rung judged — everything the top \
             rung rests on</summary>\n\n",
            self.name,
            agreements.len()
        ));
        out.push_str("| case | values | completions |\n| --- | --: | --: |\n");
        for result in agreements.iter().take(limit) {
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                result.rel.display(),
                result.valued,
                result.completions
            ));
        }
        out.push_str("\n</details>\n");
    }

    /// The cases whose start function trapped, one per row: each is a different initialiser.
    fn push_trapped(&self, out: &mut String, limit: usize) {
        let trapped = self.trapped();
        if trapped.is_empty() || limit == 0 {
            return;
        }
        out.push_str(&format!(
            "\n<details><summary>{}: {} case(s) whose start function trapped</summary>\n\n",
            self.name,
            trapped.len()
        ));
        out.push_str("| case | detail |\n| --- | --- |\n");
        for result in trapped.iter().take(limit) {
            out.push_str(&format!(
                "| `{}` | {} |\n",
                result.rel.display(),
                result.outcome.detail().unwrap_or("—"),
            ));
        }
        out.push_str("\n</details>\n");
    }

    /// One collapsed count-and-message table, skipped when there is nothing to put in it.
    fn push_details(
        &self,
        out: &mut String,
        summary: &str,
        header: &str,
        rows: &[(String, usize)],
    ) {
        if rows.is_empty() {
            return;
        }
        out.push_str(&format!(
            "\n<details><summary>{}: {summary}</summary>\n\n",
            self.name
        ));
        out.push_str(header);
        for (message, count) in rows {
            out.push_str(&format!("| {count} | {message} |\n"));
        }
        out.push_str("\n</details>\n");
    }

    /// Count equal messages, most common first, ties broken by the message for a stable report.
    fn tally(messages: impl Iterator<Item = String>) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for message in messages {
            *counts.entry(message).or_default() += 1;
        }
        let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        rows
    }
}

impl CaseResult {
    /// Lower one case, catching a panic as an outcome of its own.
    fn of(case: &Case, classpath: &LoweredClasspath) -> Self {
        let lowered = Self::lower(&case.path, classpath);
        Self {
            rel: case.rel.clone(),
            outcome: lowered.outcome,
            module: lowered.module,
            callable: lowered.callable,
            expected_readable: lowered.expected_readable,
            valued: 0,
            completions: 0,
        }
    }

    /// Run the front end and the backend over one source file.
    ///
    /// Never panics: a panic anywhere is caught and reported as [`Outcome::Panicked`], since
    /// catching invariant violations is the whole point.
    fn lower(path: &Path, classpath: &LoweredClasspath) -> Lowered {
        let Ok(source) = std::fs::read_to_string(path) else {
            return Lowered::stopped(Outcome::ReadError);
        };
        panic::catch_unwind(AssertUnwindSafe(|| {
            let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse(&source));
            if !parse.errors().is_empty() {
                return Lowered::stopped(Outcome::ParseError(parse.errors().len()));
            }
            let root = parse.syntax();
            let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
            // `ct.sym` only — no `with_stdlib`, for the reason `compile.rs` states: the embedded
            // stubs are registered first and would *outrank* the real JDK's signatures, so this
            // would score stub coverage under a compiler's name.
            let index = jals_exec::block_on_inline(
                ProjectIndex::builder(&[(FileId(0), root)])
                    .with_classpath(classpath)
                    .build(),
            );
            let semantics = analysis.in_project(&index, FileId(0));
            let typed = jals_exec::block_on_inline(semantics.typed());
            let module = match CompileWasm::module(&[typed], &index) {
                Ok(module) => module,
                Err(WasmError::NoRepresentation(ty)) => {
                    return Lowered::stopped(Outcome::OutOfSubset(ty));
                }
                Err(WasmError::TooLarge) => return Lowered::stopped(Outcome::TooLarge),
                Err(WasmError::Unsupported(what)) => {
                    return Lowered::stopped(Outcome::Unsupported(what));
                }
                Err(error @ WasmError::Unresolved(_)) => {
                    return Lowered::stopped(Outcome::Unresolved(format!("{error}")));
                }
                Err(WasmError::NoImplementation(what)) => {
                    return Lowered::stopped(Outcome::NoImplementation(what));
                }
            };
            let exports: Vec<String> = module
                .exports
                .iter()
                .filter(|(_, kind, _)| matches!(kind, ExportKind::Func))
                .map(|(name, _, _)| name.clone())
                .collect();
            // A module whose own lengths do not fit the format's `u32` is refused rather than
            // truncated, which is the same answer `project` gives.
            let Some(bytes) = module.finish() else {
                return Lowered::stopped(Outcome::TooLarge);
            };
            let (callable, expected_readable) = Self::callable(&Case::expected_dir(path), &exports);
            Lowered {
                outcome: Outcome::Unvalidated,
                module: Some(bytes),
                callable,
                expected_readable,
            }
        }))
        .unwrap_or_else(|_| Lowered::stopped(Outcome::Panicked))
    }

    /// The methods both compilers offer, from javac's own class files and the module's exports.
    ///
    /// **What this pairs, and what it does not.** A wasm export carries a bare method name and no
    /// owner, so the pairing is only defined where javac declares that name exactly once across the
    /// case, `static`, over an all-primitive parameter list. A name javac declares twice is
    /// [`AmbiguousExport`](Unjudged::AmbiguousExport) — the rung declines rather than picking one.
    /// A parameter this harness cannot spell (anything but a primitive) takes the method out of
    /// scope, since inventing a receiver would be inventing the test.
    ///
    /// javac's methods are read with no visibility filter, because the backend exports with none:
    /// `is_static && !is_constructor` is the whole rule, so a package-private `static` is on the
    /// module's surface and has to be on the oracle's too.
    fn callable(expected: &Path, exports: &[String]) -> (Vec<Callable>, bool) {
        let (declared, readable) = Self::expected_statics(expected);
        let mut out = Vec::new();
        for name in exports {
            let Some(candidates) = declared.get(name) else {
                continue;
            };
            if let [only] = candidates.as_slice() {
                out.push(only.clone());
            } else {
                // Recorded as one ambiguous entry so the rung can say which reason it declined
                // for, without the caller re-deriving it from an empty list.
                out.push(Callable {
                    name: name.clone(),
                    owner: String::new(),
                    descriptor: String::new(),
                });
            }
        }
        (out, readable)
    }

    /// Every all-primitive `static` method javac declared, grouped by name, from `expected/`.
    ///
    /// A `.class` this workspace's reader refuses is skipped rather than abandoning the map: one
    /// unreadable file among twenty is nineteen classes' worth of oracle still worth having.
    fn expected_statics(dir: &Path) -> (BTreeMap<String, Vec<Callable>>, bool) {
        let mut out: BTreeMap<String, Vec<Callable>> = BTreeMap::new();
        let mut readable = false;
        for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("class") {
                continue;
            }
            let Some(class) = std::fs::read(path).ok().and_then(|bytes| {
                jals_exec::block_on_inline(ClassFile::read(bytes.as_slice())).ok()
            }) else {
                continue;
            };
            readable = true;
            let pool = &class.constant_pool;
            let Some(internal) = pool.class_name(class.this_class) else {
                continue;
            };
            let owner = internal.replace('/', ".");
            for method in &class.methods {
                if !method.access_flags.is_static() {
                    continue;
                }
                let (Some(name), Some(descriptor)) = (
                    pool.utf8(method.name_index),
                    pool.utf8(method.descriptor_index),
                ) else {
                    continue;
                };
                let descriptor = descriptor.into_owned();
                if !Signature::is_callable(&descriptor) {
                    continue;
                }
                out.entry(name.into_owned()).or_default().push(Callable {
                    name: String::new(),
                    owner: owner.clone(),
                    descriptor,
                });
            }
        }
        // The name is on the key, so fill it in once the group is known to be a pairing candidate.
        for (name, group) in &mut out {
            for callable in group.iter_mut() {
                callable.name.clone_from(name);
            }
        }
        (out, readable)
    }

    /// Why the agreement rung declined, given what the pairing found.
    ///
    /// Ambiguity outranks absence: a case whose export names a method javac declares twice has a
    /// callable surface, and saying it has none would hide the one finding here that is about the
    /// export naming rule rather than about this corpus.
    fn why_unjudged(&self) -> Unjudged {
        if self.callable.iter().any(|c| c.descriptor.is_empty()) {
            Unjudged::AmbiguousExport
        } else if self.expected_readable {
            Unjudged::NoCallableMethod
        } else {
            Unjudged::Unreadable
        }
    }
}

/// What lowering one case produced, before any engine has seen it.
struct Lowered {
    outcome: Outcome,
    module: Option<Vec<u8>>,
    callable: Vec<Callable>,
    expected_readable: bool,
}

impl Lowered {
    /// A case that never reached a module.
    const fn stopped(outcome: Outcome) -> Self {
        Self {
            outcome,
            module: None,
            callable: Vec::new(),
            expected_readable: false,
        }
    }
}

pub use engine::Engine;
pub use oracle::Oracle;

use oracle::Signature;

mod engine;
mod oracle;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::*;

    /// A `.java` with no sibling `expected/` is not a case: the generator writes that directory
    /// only for a file javac compiled, so a stray source cannot enter either denominator.
    #[test]
    fn a_case_needs_its_expected_directory() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Lone.java"), "class Lone {}\n").unwrap();
        fs::write(dir.path().join("Paired.java"), "class Paired {}\n").unwrap();
        fs::create_dir(dir.path().join("Paired.expected")).unwrap();

        let cases = WasmReport::collect_cases(dir.path());
        assert_eq!(cases.len(), 1, "only the paired case is a case");
        assert_eq!(cases[0].rel, Path::new("Paired.java"));
    }

    /// A construct name is the whole of what an `Unsupported` row says, so it survives bucketing.
    ///
    /// The elision exists for the corpus's own identifiers. `WasmError::Unsupported` carries a
    /// `&'static str` and can carry nothing case-specific, so eliding it would turn every row into
    /// `an `…` declaration` and throw the finding away.
    #[test]
    fn a_construct_name_is_its_own_bucket() {
        let outcome = Outcome::Unsupported("an `@interface` declaration");
        assert_eq!(outcome.bucket().unwrap(), "an `@interface` declaration");
    }

    /// A resolution failure quotes the corpus's identifier, so equivalent ones are one bucket.
    #[test]
    fn resolution_failures_bucket_by_shape() {
        let a = Outcome::Unresolved("`names.stream()` did not resolve".to_owned());
        let b = Outcome::Unresolved("`other.call()` did not resolve".to_owned());
        assert_eq!(a.bucket(), b.bucket(), "one gap, one bucket");
        assert_eq!(a.bucket().unwrap(), "`…` did not resolve");
    }

    /// Two validator rejections of one shape are one row: the function index and the byte offset
    /// name the case, not the defect.
    #[test]
    fn equivalent_validator_rejections_are_one_shape() {
        let a = "error: func 1 failed to validate — 0: type mismatch: expected (ref null $type), \
                 found (ref $type) (at offset 0x64)";
        let b = "error: func 0 failed to validate — 0: type mismatch: expected (ref null $type), \
                 found (ref $type) (at offset 0x71)";
        assert_eq!(Outcome::elide(a), Outcome::elide(b));
        assert!(
            Outcome::elide(a).contains("type mismatch"),
            "the shape of the rejection has to survive the elision"
        );
    }

    /// A defect is listed in full in a section of its own; counting it as a gap too would show
    /// one finding as two. The same holds for the two outcomes that have their own listing.
    #[test]
    fn a_finding_with_its_own_section_is_not_also_a_bucket() {
        for outcome in [
            Outcome::Rejected("error: func 1 failed to validate".to_owned()),
            Outcome::Disagreed("answered `0` where javac answers `1`".to_owned()),
            Outcome::Panicked,
            Outcome::Trapped("unreachable".to_owned()),
            Outcome::Unjudged(Unjudged::NoCallableMethod),
        ] {
            assert_eq!(
                outcome.bucket(),
                None,
                "{} is listed on its own",
                outcome.label()
            );
        }
    }

    /// Ambiguity outranks absence, and neither is reachable on today's corpus — so the rule that
    /// keeps them apart is held here rather than by a case that happens to hit it.
    ///
    /// A bare-name export javac declares twice has a callable surface; calling that "no callable
    /// method" would hide the one finding here that is about the *export naming rule* rather than
    /// about this corpus.
    #[test]
    fn an_ambiguous_export_outranks_an_absent_one() {
        let case = |callable: Vec<Callable>, expected_readable: bool| CaseResult {
            rel: PathBuf::from("Case.java"),
            outcome: Outcome::NotRun,
            module: None,
            callable,
            expected_readable,
            valued: 0,
            completions: 0,
        };
        let ambiguous = Callable {
            name: "f".to_owned(),
            owner: String::new(),
            descriptor: String::new(),
        };
        let paired = Callable {
            name: "f".to_owned(),
            owner: "Case".to_owned(),
            descriptor: "()I".to_owned(),
        };

        assert_eq!(
            case(vec![paired.clone(), ambiguous], true).why_unjudged(),
            Unjudged::AmbiguousExport,
            "one unpairable export decides the case, whatever its siblings offered"
        );
        assert_eq!(
            case(vec![paired], true).why_unjudged(),
            Unjudged::NoCallableMethod
        );
        assert_eq!(
            case(Vec::new(), false).why_unjudged(),
            Unjudged::Unreadable,
            "no oracle at all is not the same as an oracle with nothing to call"
        );
    }

    /// The subset line is what keeps the compiler's rate apart from the corpus's coverage.
    #[test]
    fn only_a_representable_type_leaves_the_subset() {
        assert!(!Outcome::OutOfSubset("String".to_owned()).in_subset());
        assert!(!Outcome::ReadError.in_subset());
        assert!(Outcome::Unsupported("an `@interface` declaration").in_subset());
        assert!(Outcome::Rejected("bad".to_owned()).in_subset());
    }

    /// Every rung above `lowered` implies the ones below it, or the ladder is not a ladder.
    #[test]
    fn the_ladder_is_monotone() {
        for outcome in [
            Outcome::Agreed,
            Outcome::Unjudged(Unjudged::NoCallableMethod),
            Outcome::NotRun,
            Outcome::Trapped("t".to_owned()),
            Outcome::Rejected("r".to_owned()),
            Outcome::Unvalidated,
            Outcome::Unsupported("u"),
            Outcome::ParseError(1),
        ] {
            let rungs = [
                outcome.parsed(),
                outcome.lowered(),
                outcome.validated(),
                outcome.instantiated(),
                outcome.agreed(),
            ];
            for pair in rungs.windows(2) {
                assert!(
                    !pair[1] || pair[0],
                    "{}: a rung was reached without the one below it",
                    outcome.label()
                );
            }
        }
    }

    /// Read a file that lives beside this crate, by a path relative to its manifest dir.
    fn repo_file(rel: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        fs::read_to_string(&path).unwrap_or_else(|_| panic!("{rel} should exist"))
    }

    /// Both tool pins are half this measurement's definition, so CI has to run what they name.
    ///
    /// The validator's message text *is* the failure-bucket key and the engine decides two rungs,
    /// so a run under other releases is a different measurement wearing the same number.
    #[test]
    fn the_pins_match_ci() {
        let ci = repo_file("../.github/workflows/ci.yml");
        for entry in [
            format!("CORPUS_WASM_TOOLS_VERSION: \"{WASM_TOOLS_PIN}\""),
            format!("CORPUS_WASMTIME_VERSION: \"{WASMTIME_PIN}\""),
        ] {
            assert!(
                ci.contains(&entry),
                "ci.yml does not pin `{entry}` — the pin and the workflow have drifted"
            );
        }
    }

    /// CI has to actually run the harness, or the report it is pinned for is never produced.
    #[test]
    fn ci_runs_the_harness() {
        let ci = repo_file("../.github/workflows/ci.yml");
        assert!(
            ci.contains("--bin jals-wasm"),
            "ci.yml's corpus report does not run jals-wasm"
        );
    }
}
