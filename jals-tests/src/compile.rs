//! Compiler end-to-end verification.
//!
//! The crate root ([`crate`]) checks parser invariants and [`golden`](crate::golden) checks
//! formatter fidelity. This module checks the third thing: how much real Java `jals-javac` turns
//! into class files a real JVM accepts.
//!
//! # There is no ready-made corpus, so javac is the oracle
//!
//! OpenJDK's `test/langtools/tools/javac` is a *behavioural and diagnostic* suite driven by jtreg,
//! not a table of `.java` → expected `.class`. A fifth of it is `@compile/fail` — deliberately
//! invalid Java, which measures nothing here, because `jals-javac` never checks (diagnostics are
//! `jals-lint`'s job over `jals-hir`). Another third has no `@test` header at all: those are
//! auxiliary sources that only mean anything beside a sibling.
//!
//! So the corpus is *generated*, exactly as the four OpenJDK formatter corpora are:
//! `scripts/gen-javac-corpus.sh` runs the pinned `javac` over every candidate file on its own and
//! keeps the ones it compiles. That is what makes the denominator honest — a file javac itself
//! cannot compile alone (a multi-file test, a preview feature, an annotation processor) is
//! **out of scope**, recorded with its reason in `SKIPPED.tsv`, and never counted as a failure.
//!
//! # The ladder
//!
//! One number over a compiler is uninformative, so each case reports how far it got:
//!
//! | rung | what it proves |
//! | --- | --- |
//! | parsed | `jals-syntax` accepted the source with no syntax error |
//! | lowered | `Compile::file` produced class files rather than a [`LowerError`] |
//! | re-read | `jals_classfile::ClassFile::read` reads back what the assembler wrote |
//! | verified | a real JVM **linked** the class: the bytecode verifier accepted it |
//! | descriptor-equal | every method's descriptor is one javac gave the same name |
//!
//! `verified` is the rung this harness was built for: nothing upstream of the JVM's verifier can
//! tell a well-formed class file from a plausible one.
//!
//! `descriptor-equal` is what the verifier structurally cannot reach. It judges one compilation at
//! a time, and every case here is a single file — so an erasure the declaration and its call sites
//! get *equally* wrong is self-consistent and links cleanly. Only a second opinion can catch that,
//! and the corpus already holds one: javac's own class files, beside every case in `expected/`.
//! See [`CaseResult::descriptor_disagreement`] for exactly what is compared, which is narrower than
//! "compiled the same way javac did".
//!
//! # The classpath is a real JDK's, not the embedded stubs
//!
//! `jals-hir`'s embedded stubs are ~58 signature-only types — enough for the analysis to say
//! something useful about an editor buffer, nowhere near enough to compile arbitrary Java. Scoring
//! a corpus against them would report *stub coverage* wearing the name of a compiler pass rate.
//!
//! `$JAVA_HOME/lib/ct.sym` is the signature data `javac --release` reads: an ordinary zip of
//! ordinary class files with their method bodies stripped. Lowering it into a
//! [`LoweredClasspath`] gives the analysis the real JDK's own signatures, which is what the
//! product does through `jals-classpath` when it compiles against a real dependency. Reading it
//! needs a host path, which is why this lives in a test harness — `jals-javac`'s stdlib oracle
//! reads the same file for the same reason.
//!
//! # Pin
//!
//! The rate depends on the host JDK **twice**: javac decides the denominator and `ct.sym` supplies
//! the classpath. So it is pinned like the formatter references are ([`JAVAC_PIN`]), and
//! [`the javac pin matches CI`](tests::the_javac_pin_matches_ci) fails when the workflow drifts
//! from it.

use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;

use jals_classfile::ClassFile;
use jals_hir::{FileAnalysis, FileId, LoweredClasspath, ProjectIndex};
use jals_javac::lower::Compile;
use rayon::prelude::*;
use walkdir::WalkDir;

/// The class-file major version the corpus is compiled to: Java 25, matching the JDK the corpus
/// is pinned to and the fixtures the rest of the workspace uses.
const MAJOR_JAVA_25: u16 = 69;

/// The JDK feature release the corpus is defined against.
///
/// Both halves of the measurement depend on it — javac decides which files are in scope, and its
/// `ct.sym` is the classpath the analysis resolves against — so a run on a different JDK is a
/// different measurement, in the same way a formatter similarity is only defined against a pinned
/// release (`jals-fmt/DESIGN.md` §7.1).
pub const JAVAC_PIN: &str = "25";

/// The `ct.sym` modules whose signatures the corpus resolves against.
///
/// `java.*` is the platform proper. `jdk.compiler` is here because langtools' own tests routinely
/// import `com.sun.source.*` — those files compile alone under javac, so excluding the module
/// would move them from "the compiler could not lower this" to "the harness never showed it the
/// types", which is exactly the confusion the real classpath exists to prevent.
const CLASSPATH_MODULES: &[&str] = &["java.", "jdk.compiler"];

/// A named compiler corpus, rooted at a path relative to the `sources/` directory.
pub struct CompileSource {
    /// Stable identifier used on the command line.
    pub name: &'static str,
    /// Root directory, relative to the `sources/` dir.
    pub root_rel: &'static str,
    /// Human-readable description.
    pub description: &'static str,
}

/// Every compiler corpus the CLI knows about. Add an entry here to register a new one.
pub const COMPILE_SOURCES: &[CompileSource] = &[CompileSource {
    name: "langtools",
    root_rel: "javac-langtools",
    description: "OpenJDK test/langtools/tools/javac files that javac compiles on their own \
                  (generated; see scripts/gen-javac-corpus.sh)",
}];

impl CompileSource {
    /// Look up a corpus by its command-line name.
    pub fn by_name(name: &str) -> Option<&'static Self> {
        COMPILE_SOURCES.iter().find(|s| s.name == name)
    }
}

/// A host JDK: where it lives and which feature release it is.
#[derive(Debug, Clone)]
pub struct Jdk {
    /// The JDK's home directory (`java.home`).
    pub home: PathBuf,
    /// Its feature release (`java.specification.version`), e.g. 25.
    pub version: u32,
}

impl Jdk {
    /// The running JDK, asked of the JVM itself.
    ///
    /// `$JAVA_HOME` is routinely unset even where a JDK is installed, so it is not consulted;
    /// asking the JVM for `java.home` works whenever `java` is on `PATH` at all.
    pub fn detect() -> Option<Self> {
        let output = Command::new("java")
            .args(["-XshowSettings:properties", "-version"])
            .output()
            .ok()?;
        // The settings dump goes to stderr, one `  name = value` per line.
        let text = String::from_utf8_lossy(&output.stderr).into_owned();
        let property = |name: &str| {
            text.lines()
                .filter_map(|line| line.split_once('='))
                .find(|(key, _)| key.trim() == name)
                .map(|(_, value)| value.trim().to_owned())
        };
        let home = PathBuf::from(property("java.home")?);
        let version = property("java.specification.version")?.parse().ok()?;
        Some(Self { home, version })
    }

    /// The letter `ct.sym` files a release under: `8` and `9` are themselves, then `A` is 10, `B`
    /// is 11, and so on. A directory name is a *set* of these (`MNOP` covers 22 through 25), so an
    /// entry belongs to a release when its first path segment contains that release's letter.
    fn release_letter(version: u32) -> Option<char> {
        match version {
            8 | 9 => char::from_digit(version, 10),
            10.. => char::from_u32('A' as u32 + (version - 10)),
            _ => None,
        }
    }

    /// Whether a `ct.sym` entry is a signature this corpus resolves against.
    ///
    /// Entries are named `<releases>/<module>/<package>/<Class>.sig`, where `<releases>` is the set
    /// of release letters the signature is valid for.
    fn is_wanted_signature(entry: &str, release: char) -> bool {
        let Some((releases, rest)) = entry.split_once('/') else {
            return false;
        };
        let Some((module, class_path)) = rest.split_once('/') else {
            return false;
        };
        releases.contains(release)
            && class_path.ends_with(".sig")
            && CLASSPATH_MODULES
                .iter()
                .any(|wanted| module.starts_with(wanted))
    }

    /// Lower this JDK's own signatures into the classpath the corpus resolves against.
    ///
    /// # Errors
    /// If `ct.sym` is missing or unreadable — a JDK that ships no `ct.sym` (a JRE, or a stripped
    /// image) cannot supply the classpath, and silently falling back to the embedded stubs would
    /// report stub coverage under this harness's name.
    pub fn classpath(&self) -> Result<(LoweredClasspath, usize), String> {
        let release = Self::release_letter(self.version)
            .ok_or_else(|| format!("JDK {} has no ct.sym release letter", self.version))?;
        let path = self.home.join("lib").join("ct.sym");
        let file =
            std::fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("{} is not a zip: {e}", path.display()))?;

        let mut classes = Vec::new();
        for index in 0..archive.len() {
            let Ok(mut entry) = archive.by_index(index) else {
                continue;
            };
            let Some(name) = entry.enclosed_name() else {
                continue;
            };
            let name = name.to_string_lossy().replace('\\', "/");
            if !Self::is_wanted_signature(&name, release) {
                continue;
            }
            let mut bytes = Vec::new();
            if std::io::Read::read_to_end(&mut entry, &mut bytes).is_err() {
                continue;
            }
            // A signature entry this workspace's own reader cannot parse is a `jals-classfile`
            // gap, but it is not this harness's subject: one missing type shows up downstream as
            // the cases that needed it, which is the honest place for it to surface.
            if let Ok(class) = jals_exec::block_on_inline(ClassFile::read(bytes.as_slice())) {
                classes.push(class);
            }
        }
        if classes.is_empty() {
            return Err(format!(
                "{} holds no signatures for release {release} — is this JDK {}?",
                path.display(),
                self.version
            ));
        }
        let count = classes.len();
        Ok((
            jals_exec::block_on_inline(ProjectIndex::lower_classpath(&classes)),
            count,
        ))
    }

    /// `javac <version>` — what a corpus and the rate scored over it are defined by.
    fn reference(&self) -> String {
        format!("javac {}", self.version)
    }
}

/// How far one case got up the ladder.
///
/// Ordered by rung: everything below [`Verified`](Self::Verified) is a place the pipeline stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The JVM linked every emitted class *and* every method carries a descriptor javac gave the
    /// same name — the top rung.
    Verified,
    /// The JVM linked every emitted class, but a method's descriptor disagrees with javac's own
    /// class file for that type.
    ///
    /// Not a defect: the bytes load and run. It is a rung because the verifier structurally cannot
    /// catch it — a single-file corpus compiles the declaration and its call sites together, so an
    /// erasure both sides get equally wrong is self-consistent and links. What it *would* break is a
    /// separately-compiled caller, which is the thing a compiler's descriptors exist to agree with.
    DescriptorMismatch(String),
    /// The JVM's verifier rejected a class — a `VerifyError` or `ClassFormatError`. The class file
    /// is wrong, and this is the finding the harness exists to produce.
    Rejected(String),
    /// Linking failed for a reason that is not about this class file's shape (a type it references
    /// is not on the harness's classpath). The bytes were not judged either way.
    Unlinkable(String),
    /// The JVM linked every emitted class and the descriptor rung could not be *judged*: no
    /// `(name, descriptor)` pair was compared, for the reason carried.
    ///
    /// Its own outcome rather than a pass, because the rung fails **open** otherwise and open is the
    /// top rung — a corpus or class-file-reader problem would be scored as "jals agrees with javac"
    /// and would inflate the headline number systematically, since a construct family shares its
    /// `expected/` output. "Nothing compared" and "everything agreed" have to be distinguishable, so
    /// the count of compared pairs is what separates them. Kept apart from every *finding*, exactly
    /// as [`ReadError`](Self::ReadError) is: this says nothing about the compiler.
    DescriptorsUnjudged(Unjudged),
    /// The class files were emitted but the JVM stage did not run (`--no-verify`, or no JVM).
    Unverified,
    /// Lowering succeeded but produced no class file at all, so there is nothing for a JVM to
    /// accept. Kept apart from a lowering error: the pipeline did not fail, it fell silent.
    NoClasses,
    /// The assembler's own output does not read back through `jals_classfile::ClassFile::read`.
    RereadError,
    /// Lowering refused the file. The message is bucketed in the report.
    LowerError(String),
    /// The parser reported syntax errors on a file javac compiled — a parser gap, since every file
    /// in the corpus is valid Java by construction.
    ParseError(usize),
    /// The pipeline panicked: a hard invariant violation, and never an acceptable outcome.
    Panicked,
    /// The case's source could not be read. A host problem, kept apart from every rung: reporting
    /// it as a parse failure would fail `--strict` for a permission bit.
    ReadError,
}

impl Outcome {
    /// A short, stable label for display and for the failure buckets.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Verified => "descriptor-equal",
            Self::DescriptorMismatch(_) => "verified",
            Self::DescriptorsUnjudged(_) => "descriptors-unjudged",
            Self::Rejected(_) => "jvm-rejected",
            Self::Unlinkable(_) => "unlinkable",
            Self::Unverified => "unverified",
            Self::NoClasses => "no-classes",
            Self::RereadError => "reread-error",
            Self::LowerError(_) => "lower-error",
            Self::ParseError(_) => "parse-error",
            Self::Panicked => "panicked",
            Self::ReadError => "read-error",
        }
    }

    /// Whether the source parsed with no syntax error.
    const fn parsed(&self) -> bool {
        !matches!(self, Self::ParseError(_) | Self::Panicked | Self::ReadError)
    }

    /// Whether lowering produced class files.
    const fn lowered(&self) -> bool {
        self.parsed() && !matches!(self, Self::LowerError(_) | Self::NoClasses)
    }

    /// Whether the emitted bytes read back through this workspace's own class-file reader.
    const fn reread(&self) -> bool {
        self.lowered() && !matches!(self, Self::RereadError)
    }

    /// Whether a real JVM linked every class the case emitted.
    const fn verified(&self) -> bool {
        matches!(
            self,
            Self::Verified | Self::DescriptorMismatch(_) | Self::DescriptorsUnjudged(_)
        )
    }

    /// Whether the emitted descriptors also agree with javac's own — the top rung.
    const fn descriptor_equal(&self) -> bool {
        matches!(self, Self::Verified)
    }

    /// Whether a violated invariant produced this outcome, rather than an unimplemented path.
    ///
    /// These are the outcomes that should fail a run rather than lower a percentage: a panic, a
    /// class file the JVM rejects, output this workspace cannot read back, and a syntax error on
    /// a file that is valid Java by construction.
    const fn is_invariant_violation(&self) -> bool {
        matches!(
            self,
            Self::Panicked | Self::Rejected(_) | Self::RereadError | Self::ParseError(_)
        )
    }

    /// Which rung this stopped at, lowest first — the sort key that puts defects above gaps.
    const fn rung(&self) -> u8 {
        match self {
            Self::Panicked => 0,
            Self::Rejected(_) => 1,
            Self::RereadError => 2,
            Self::ParseError(_) => 3,
            Self::NoClasses => 4,
            Self::LowerError(_) => 5,
            Self::Unlinkable(_) => 6,
            Self::Unverified => 7,
            Self::DescriptorMismatch(_) => 8,
            Self::Verified => 9,
            // Not rungs at all: one is a case the harness never saw the source of, the other one it
            // could not read javac's answer for. Both are corpus problems.
            Self::DescriptorsUnjudged(_) => 10,
            Self::ReadError => 11,
        }
    }

    /// The detail worth printing beside the label, if any.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Rejected(message)
            | Self::Unlinkable(message)
            | Self::LowerError(message)
            | Self::DescriptorMismatch(message) => Some(message),
            // Fixed per reason rather than carried per case: nothing here is case-specific, and a
            // bucket is the only place a reader would otherwise see that the rung was not judged at
            // all — which is half of what makes it distinct from agreement.
            Self::DescriptorsUnjudged(reason) => Some(reason.detail()),
            _ => None,
        }
    }

    /// The message with its quoted source snippet elided, so equivalent failures bucket together.
    ///
    /// `` `names.stream()` did not resolve `` and `` `x.y()` did not resolve `` are one gap, and a
    /// report that lists them separately buries the shape of the remaining work under the corpus's
    /// variable names.
    ///
    /// Buckets cover the **gaps** only. A defect is listed in full in its own section, and counting
    /// it here as well would show one rejected class file as two findings.
    fn bucket(&self) -> Option<String> {
        if self.is_invariant_violation() {
            return None;
        }
        let message = self.detail()?;
        let Some((head, rest)) = message.split_once('`') else {
            return Some(message.to_owned());
        };
        let tail = rest.rsplit_once('`').map_or("", |(_, tail)| tail);
        Some(format!("{head}`…`{tail}"))
    }
}

/// Why the descriptor rung compared nothing for a case that linked.
///
/// Three distinct situations wore one sentence — "javac's own class files for the case could not be
/// read" — and only the first of them is that. An annotation interface declares no method at all,
/// so javac's class file has none for jals's to agree with; an implicitly-declared class
/// (`NestedEnum.java`) is named after its *file* by javac, so a case where jals spells the type
/// differently offers no type to compare within. Reporting either as an unreadable class file sends
/// a reader to the corpus generator for something the compiler decided, which is the opposite of
/// what this rung's listing is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unjudged {
    /// The case's `expected/` directory yielded no class file this workspace's reader could read —
    /// the corpus problem the rung was originally guarding against.
    Unreadable,
    /// javac's class files read, but jals emitted no type javac also named. The two compilers
    /// disagree about the *names*, which is a finding of its own and not one about descriptors.
    NoSharedType,
    /// Both compilers named a type, and neither declares a method the other also declares — an
    /// annotation interface, a marker interface, a `package-info`. Nothing to compare, and nothing
    /// wrong.
    NoSharedMethod,
}

impl Unjudged {
    /// The sentence printed beside the case, which is the whole of what this outcome says.
    const fn detail(self) -> &'static str {
        match self {
            Self::Unreadable => "javac's own class files for the case could not be read",
            Self::NoSharedType => {
                "jals emitted no type javac also named, so no descriptor was compared"
            }
            Self::NoSharedMethod => {
                "neither compiler declares a method the other does, so no descriptor was compared"
            }
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
    /// Where a case's javac-produced class files live.
    ///
    /// Its presence is what makes a `.java` under the corpus root a case at all: the generator
    /// writes the directory only for a file javac compiled, so a stray source dropped into the
    /// tree cannot enter the denominator.
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
    /// What javac's own class files said about the descriptors jals emitted.
    ///
    /// Computed while the emitted bytes are still in hand, and applied only *after* the JVM stage:
    /// the rung sits above `verified`, so a case that never linked is not judged on it.
    descriptors: Descriptors,
}

/// The three answers the descriptor rung has, which is one more than "agreed or not".
///
/// [`Unjudged`](Self::Unjudged) exists because the comparison can fail to happen: javac's own class
/// files are read through this workspace's own reader, and a `.class` it refuses, a constant-pool
/// entry it cannot resolve, or a directory a partial generation run left empty all mean *nothing was
/// compared*. Folding that into "agreed" made the rung fail open — and open is the top rung, so a
/// corpus problem was scored as "jals agrees with javac", per construct family, invisibly: no
/// bucket, no `--list-failures` entry, and no effect on `--strict`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Descriptors {
    /// Every method both compilers named carried a descriptor javac also gave that name — and at
    /// least one pair was actually compared.
    Agreed,
    /// The first method jals emitted whose descriptor javac did not give that name.
    Disagreed(String),
    /// No `(name, descriptor)` pair was compared, so the rung has no answer for this case.
    Unjudged(Unjudged),
}

/// A file the generator kept out of the corpus, and why.
///
/// Only the reason is held: the report tallies reasons, and the path each one belongs to is
/// already in `SKIPPED.tsv` beside the corpus, which is where a reader goes to find out *which*
/// file javac declined.
#[derive(Debug, Clone)]
pub struct Skipped {
    /// Why javac could not compile it alone — the generator's classification.
    reason: String,
}

/// Aggregated compiler outcomes for one corpus.
#[derive(Debug, Clone)]
pub struct CompileReport {
    /// Corpus name.
    pub name: String,
    /// `javac <version>` — the JDK that decided the scope and supplied the classpath.
    pub reference: String,
    /// Resolved root directory that was walked.
    pub root: PathBuf,
    /// Every case's result, worst rung first.
    results: Vec<CaseResult>,
    /// What the generator left out of the corpus, with reasons.
    pub skipped: Vec<Skipped>,
}

impl CompileReport {
    /// How many defects a report lists however small `--limit` is.
    ///
    /// `--limit` bounds the *gap* listings, which are a long tail worth truncating. A defect is a
    /// class file no JVM will load, and a report that hid one behind a display setting would be
    /// reporting a rate while withholding the finding that rate is there to surface.
    pub const DEFECTS_ALWAYS_LISTED: usize = 20;

    /// Every `.java` under `root` that has a sibling `<Base>.expected/` directory — the pairs the
    /// generator wrote, and nothing a stray file could add to them.
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
        // Deterministic order: the report's `--list-failures` is read as a work list.
        cases.sort_by(|a, b| a.rel.cmp(&b.rel));
        cases
    }

    /// The generator's out-of-scope list, if the corpus carries one.
    fn read_skipped(root: &Path) -> Vec<Skipped> {
        let Ok(text) = std::fs::read_to_string(root.join("SKIPPED.tsv")) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(_rel, reason)| Skipped {
                reason: reason.to_owned(),
            })
            .collect()
    }

    /// Compile every case under `root` against `classpath`, then link the results on a real JVM.
    ///
    /// `verifier` is the JVM stage; `None` leaves every emitted case at [`Outcome::Unverified`],
    /// which the report shows as its own column rather than folding into the pass rate.
    pub fn run(
        name: &str,
        root: &Path,
        jdk: &Jdk,
        classpath: &LoweredClasspath,
        verifier: Option<&Verifier>,
    ) -> Self {
        let cases = Self::collect_cases(root);
        let mut results: Vec<CaseResult> = cases
            .par_iter()
            .map(|case| CaseResult::of(case, classpath, verifier))
            .collect();

        if let Some(verifier) = verifier {
            verifier.link_all(&mut results);
        }
        Self::apply_descriptor_rung(&mut results);

        // Worst rung first, so a truncated listing surfaces the hard failures rather than the
        // long tail of unimplemented syntax.
        results.sort_by_key(|result| (result.outcome.rung(), result.rel.clone()));

        Self {
            name: name.to_owned(),
            reference: jdk.reference(),
            root: root.to_path_buf(),
            results,
            skipped: Self::read_skipped(root),
        }
    }

    /// Cases in the corpus: the honest denominator, since every one of them compiles under javac.
    pub fn total(&self) -> usize {
        self.results.len()
    }

    /// How many cases reached each rung of the ladder.
    pub fn ladder(&self) -> [usize; 5] {
        let count = |reached: fn(&Outcome) -> bool| {
            self.results
                .iter()
                .filter(|result| reached(&result.outcome))
                .count()
        };
        [
            count(Outcome::parsed),
            count(Outcome::lowered),
            count(Outcome::reread),
            count(Outcome::verified),
            count(Outcome::descriptor_equal),
        ]
    }

    /// Whether any case violated an invariant rather than merely stopping short.
    pub fn has_invariant_violations(&self) -> bool {
        self.results
            .iter()
            .any(|result| result.outcome.is_invariant_violation())
    }

    /// Every defect this corpus produced, worst rung first — a class file the JVM rejects, output
    /// that does not read back, a panic, or a syntax error on valid Java.
    pub fn violations(&self) -> Vec<&CaseResult> {
        self.results
            .iter()
            .filter(|result| result.outcome.is_invariant_violation())
            .collect()
    }

    /// Every case the descriptor rung stopped — a descriptor javac spells differently, or one it
    /// could not be judged on at all — worst rung first.
    ///
    /// Listed **per case**, unlike a gap, because a bucket exists to bundle failures of one shape and
    /// these have none in common: each one names a different method of a different class. Eliding the
    /// names (which is what [`Outcome::bucket`] does to keep a gap list readable) leaves one row
    /// saying `a descriptor javac spells differently` forty-seven times over — a count with nothing
    /// in it to act on. A reader working the rung needs the class, the method, and both spellings,
    /// which is exactly what the outcome's detail already carries.
    pub fn descriptor_findings(&self) -> Vec<&CaseResult> {
        self.results
            .iter()
            .filter(|result| {
                matches!(
                    result.outcome,
                    Outcome::DescriptorMismatch(_) | Outcome::DescriptorsUnjudged(_)
                )
            })
            .collect()
    }

    /// The failure messages, most frequent first, with source snippets elided.
    pub fn buckets(&self) -> Vec<(String, usize)> {
        Self::tally(
            self.results
                .iter()
                .filter_map(|result| result.outcome.bucket()),
        )
    }

    /// Why the generator left files out, most frequent reason first.
    pub fn skip_reasons(&self) -> Vec<(String, usize)> {
        Self::tally(self.skipped.iter().map(|skipped| skipped.reason.clone()))
    }

    /// Count each distinct message, most frequent first and ties broken alphabetically.
    ///
    /// The tie-break is what makes two runs over one corpus produce the same report: a plain count
    /// sort leaves equally-frequent reasons in whatever order the map yielded them.
    fn tally(messages: impl Iterator<Item = String>) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for message in messages {
            *counts.entry(message).or_default() += 1;
        }
        let mut tallied: Vec<(String, usize)> = counts.into_iter().collect();
        tallied.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        tallied
    }

    /// Apply the descriptor rung to every case a JVM linked.
    ///
    /// The rung sits *above* `verified`, so it is applied only to what linked: bytes no JVM would
    /// load have already failed for a reason that says more than this one would. All three answers
    /// are applied, [`Descriptors::Unjudged`] included — leaving that one as
    /// [`Outcome::Verified`] is the fail-open this rung cannot afford, because `Verified` *is* the
    /// `descriptor-equal` column.
    fn apply_descriptor_rung(results: &mut [CaseResult]) {
        for result in results {
            if result.outcome != Outcome::Verified {
                continue;
            }
            result.outcome = match &result.descriptors {
                Descriptors::Agreed => Outcome::Verified,
                Descriptors::Disagreed(message) => Outcome::DescriptorMismatch(message.clone()),
                Descriptors::Unjudged(reason) => Outcome::DescriptorsUnjudged(*reason),
            };
        }
    }

    /// Render the reports as a GitHub-flavored Markdown summary, for a CI step summary or a
    /// pull-request comment.
    ///
    /// `limit` is how many failing cases to list per corpus (0 = none), wrapped in a collapsed
    /// `<details>` so it stays tidy in a comment.
    pub fn markdown_report(reports: &[Self], limit: usize) -> String {
        let mut out = String::from("## jals-javac end-to-end\n\n");
        out.push_str(
            "How much of a corpus of real Java `jals-javac` turns into class files a real JVM \
             links. Each corpus holds only files the pinned `javac` compiles **on their own**, so \
             the denominator excludes what no single-file compiler could do; what the generator \
             left out is listed under *out of scope*. The rungs are cumulative — `verified` is \
             the one that means the class file is right, because nothing upstream of the JVM's \
             bytecode verifier can tell a well-formed class file from a plausible one. \
             `descriptor-equal` is the rung above it: the verifier judges one compilation at a \
             time, so an erasure the declaration and its call sites get equally wrong still \
             links — this rung asks javac's own class files whether the descriptors agree.\n\n",
        );
        out.push_str(
            "| corpus | reference | in scope | parsed | lowered | re-read | verified | \
             descriptor-equal |\n",
        );
        out.push_str("| --- | --- | --: | --: | --: | --: | --: | --: |\n");
        for report in reports {
            report.push_ladder_row(&mut out);
        }
        for report in reports {
            report.push_violations(&mut out, limit);
            report.push_descriptor_findings(&mut out, limit);
            let buckets = report.buckets();
            report.push_details(
                &mut out,
                &format!("what stopped the rest ({} kinds)", buckets.len()),
                "| cases | reason |\n| --: | --- |\n",
                &buckets,
            );
            report.push_details(
                &mut out,
                &format!(
                    "{} out of scope (javac declined them alone)",
                    report.skipped.len()
                ),
                "| files | javac said |\n| --: | --- |\n",
                &report.skip_reasons(),
            );
        }
        out
    }

    /// This corpus's row of the ladder table.
    fn push_ladder_row(&self, out: &mut String) {
        let total = self.total();
        let [parsed, lowered, reread, verified, descriptor_equal] = self.ladder();
        let cell = |n: usize| {
            if total == 0 {
                "0".to_owned()
            } else {
                format!("{n} ({:.1}%)", n as f64 * 100.0 / total as f64)
            }
        };
        out.push_str(&format!(
            "| {} | {} | {total} | {} | {} | {} | {} | {} |\n",
            self.name,
            self.reference,
            cell(parsed),
            cell(lowered),
            cell(reread),
            cell(verified),
            cell(descriptor_equal),
        ));
    }

    /// The defects, in full and out in the open rather than behind a `<details>`.
    fn push_violations(&self, out: &mut String, limit: usize) {
        let violations = self.violations();
        if violations.is_empty() {
            return;
        }
        out.push_str(&format!(
            "\n**{}: {} invariant violation(s)** — a class file the JVM rejects, output that does \
             not read back, or a panic. These are defects, not unimplemented syntax.\n\n",
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
    }

    /// The descriptor rung's own cases, collapsed like a gap listing but written out one per row.
    fn push_descriptor_findings(&self, out: &mut String, limit: usize) {
        let findings = self.descriptor_findings();
        if findings.is_empty() || limit == 0 {
            return;
        }
        out.push_str(&format!(
            "\n<details><summary>{}: {} case(s) the descriptor rung stopped</summary>\n\n",
            self.name,
            findings.len()
        ));
        out.push_str("| outcome | case | detail |\n| --- | --- | --- |\n");
        for result in findings.iter().take(limit) {
            out.push_str(&format!(
                "| {} | `{}` | {} |\n",
                result.outcome.label(),
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
}

impl CaseResult {
    /// Compile one case and, when a [`Verifier`] is staging class files, hand it the output.
    ///
    /// Never panics: a panic anywhere in the pipeline is caught and reported as
    /// [`Outcome::Panicked`], since catching invariant violations is the whole point.
    fn of(case: &Case, classpath: &LoweredClasspath, verifier: Option<&Verifier>) -> Self {
        let mut descriptors = Descriptors::Unjudged(Unjudged::Unreadable);
        let outcome = Self::compile(&case.path, classpath, verifier, &mut descriptors);
        Self {
            rel: case.rel.clone(),
            outcome,
            descriptors,
        }
    }

    /// The first method jals emitted whose descriptor javac did not give that name, if any.
    ///
    /// **What this compares, and what it does not.** For each emitted type javac also produced, each
    /// method jals emitted under a name javac also declares must carry one of the descriptors javac
    /// gave that name. Deliberately nothing else: a type jals did not emit, a member javac has and
    /// jals does not, an access flag, and every attribute are all out of scope, so a pass here is
    /// *not* the claim that jals compiled the file the way javac did. It is the narrower claim the
    /// verifier structurally cannot make — that where both compilers named a method, they agree on
    /// what it takes and returns, which is the whole of what a separately-compiled caller links
    /// against.
    ///
    /// A corpus problem answers [`Descriptors::Unjudged`] rather than "agreed": it is not a finding
    /// about the compiler, the same way an unreadable source is not a parse failure — and the same
    /// way, it gets an outcome of its own rather than the benefit of the doubt on the rung above
    /// `verified`.
    fn descriptor_agreement(
        source: &Path,
        classes: &[jals_javac::lower::CompiledClass],
    ) -> Descriptors {
        let expected = Self::expected_signatures(&Case::expected_dir(source));
        if expected.is_empty() {
            return Descriptors::Unjudged(Unjudged::Unreadable);
        }
        // What was actually compared, which is the whole difference between "nothing to compare"
        // and "everything agreed" — and, one level up, whether the two compilers even named the
        // same type.
        let mut compared = 0usize;
        let mut shared_types = 0usize;
        for class in classes {
            let Some(javac) = expected.get(&class.internal_name) else {
                continue;
            };
            shared_types += 1;
            let Some(ours) = Self::signatures(&class.bytes) else {
                continue;
            };
            for (name, descriptor) in ours {
                // A name javac never declared is a synthetic jals emits and javac does not (or the
                // reverse of one it omits); that is a different question from descriptor agreement.
                let Some(theirs) = javac.get(&name) else {
                    continue;
                };
                compared += 1;
                if !theirs.contains(&descriptor) {
                    // Shaped so `Outcome::bucket`'s elision leaves the sentence and drops only the
                    // names: every one of these is one finding, and 47 rows spelled with the
                    // corpus's own identifiers would bury that.
                    return Descriptors::Disagreed(format!(
                        "a descriptor javac spells differently: `{}.{name}{descriptor}` against \
                         javac's {}`",
                        class.internal_name,
                        theirs
                            .iter()
                            .map(|d| format!("`{name}{d}"))
                            .collect::<Vec<_>>()
                            .join("` / ")
                    ));
                }
            }
        }
        if compared == 0 {
            return Descriptors::Unjudged(if shared_types == 0 {
                Unjudged::NoSharedType
            } else {
                Unjudged::NoSharedMethod
            });
        }
        Descriptors::Agreed
    }

    /// Every `<name> -> [descriptor]` javac produced, per internal class name, from `expected/`.
    ///
    /// A `.class` this workspace's reader refuses is *skipped* rather than abandoning the map: one
    /// unreadable file among twenty is nineteen classes' worth of comparison still worth doing, and
    /// aborting made a single refusal look like agreement on the whole case.
    fn expected_signatures(dir: &Path) -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
        let mut out = BTreeMap::new();
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
            let Some(internal) = class.constant_pool.class_name(class.this_class) else {
                continue;
            };
            let Some(methods) = Self::method_map(&class) else {
                continue;
            };
            out.insert(internal.into_owned(), methods);
        }
        out
    }

    /// The `(name, descriptor)` pairs of one emitted class file, in declaration order.
    fn signatures(bytes: &[u8]) -> Option<Vec<(String, String)>> {
        let class = jals_exec::block_on_inline(ClassFile::read(bytes)).ok()?;
        let pool = &class.constant_pool;
        class
            .methods
            .iter()
            .map(|method| {
                Some((
                    pool.utf8(method.name_index)?.into_owned(),
                    pool.utf8(method.descriptor_index)?.into_owned(),
                ))
            })
            .collect()
    }

    /// One class file's methods, grouped by name — a name can carry several overloads.
    fn method_map(class: &ClassFile) -> Option<BTreeMap<String, Vec<String>>> {
        let pool = &class.constant_pool;
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for method in &class.methods {
            let name = pool.utf8(method.name_index)?.into_owned();
            let descriptor = pool.utf8(method.descriptor_index)?.into_owned();
            out.entry(name).or_default().push(descriptor);
        }
        Some(out)
    }

    /// Run the pipeline over one source file, catching a panic as an outcome of its own.
    fn compile(
        path: &Path,
        classpath: &LoweredClasspath,
        verifier: Option<&Verifier>,
        descriptors: &mut Descriptors,
    ) -> Outcome {
        let Ok(source) = std::fs::read_to_string(path) else {
            return Outcome::ReadError;
        };
        let mut found = Descriptors::Unjudged(Unjudged::Unreadable);
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse(&source));
            if !parse.errors().is_empty() {
                return Outcome::ParseError(parse.errors().len());
            }
            let root = parse.syntax();
            let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
            // `ct.sym` only — no `with_stdlib`. Indexing the embedded stubs as well would not add to
            // the real JDK's signatures but *outrank* them (`by_fqn` keeps the first insert, and the
            // stubs are registered before the classpath), so a partial stub `System` would hide the
            // complete `java.lang.System` and this harness would score stub coverage under a
            // compiler's name — the thing the module doc above says it does not do.
            let index = jals_exec::block_on_inline(
                ProjectIndex::builder(&[(FileId(0), root)])
                    .with_classpath(classpath)
                    .build(),
            );
            let semantics = analysis.in_project(&index, FileId(0));
            let typed = jals_exec::block_on_inline(semantics.typed());
            let classes = match Compile::file(typed, MAJOR_JAVA_25) {
                Err(error) => return Outcome::LowerError(format!("{error}")),
                Ok(classes) => classes,
            };
            if classes.is_empty() {
                return Outcome::NoClasses;
            }
            for class in &classes {
                if jals_exec::block_on_inline(ClassFile::read(class.bytes.as_slice())).is_err() {
                    return Outcome::RereadError;
                }
            }
            found = Self::descriptor_agreement(path, &classes);
            if let Some(verifier) = verifier {
                verifier.stage(path, &classes);
            }
            Outcome::Unverified
        }));
        *descriptors = found;
        outcome.unwrap_or(Outcome::Panicked)
    }
}

pub use verify::Verifier;

mod verify;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn a_case_needs_its_expected_directory() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Lone.java"), "class Lone {}\n").unwrap();
        fs::write(dir.path().join("Paired.java"), "class Paired {}\n").unwrap();
        fs::create_dir(dir.path().join("Paired.expected")).unwrap();

        let cases = CompileReport::collect_cases(dir.path());
        assert_eq!(cases.len(), 1, "only the paired case is a case");
        assert_eq!(cases[0].rel, Path::new("Paired.java"));
    }

    #[test]
    fn buckets_elide_the_quoted_snippet() {
        let a = Outcome::LowerError("`names.stream()` did not resolve".to_owned());
        let b = Outcome::LowerError("`other.call()` did not resolve".to_owned());
        assert_eq!(a.bucket(), b.bucket(), "one gap, one bucket");
        assert_eq!(a.bucket().unwrap(), "`…` did not resolve");
    }

    #[test]
    fn a_defect_is_not_also_a_bucket() {
        // It is listed in full in the invariant-violation section; bucketing it as well would
        // show one rejected class file as two findings.
        assert_eq!(
            Outcome::Rejected("VerifyError: …".to_owned()).bucket(),
            None
        );
    }

    #[test]
    fn an_unreadable_file_is_not_a_parser_defect() {
        // A permission bit is a host problem. Reporting it as a syntax error on valid Java would
        // fail `--strict` for something the compiler never saw.
        assert!(!Outcome::ReadError.is_invariant_violation());
        assert!(!Outcome::ReadError.parsed());
    }

    #[test]
    fn only_defects_are_invariant_violations() {
        // An unimplemented lowering path lowers the rate; it does not fail the run.
        assert!(!Outcome::LowerError("not compiled yet".to_owned()).is_invariant_violation());
        assert!(!Outcome::NoClasses.is_invariant_violation());
        assert!(!Outcome::Unlinkable("missing type".to_owned()).is_invariant_violation());
        // A class file the JVM rejects is a defect, and so is every rung below it.
        assert!(Outcome::Rejected("VerifyError".to_owned()).is_invariant_violation());
        assert!(Outcome::RereadError.is_invariant_violation());
        assert!(Outcome::Panicked.is_invariant_violation());
        assert!(Outcome::ParseError(1).is_invariant_violation());
    }

    #[test]
    fn the_ladder_is_cumulative() {
        let report = CompileReport {
            name: "t".to_owned(),
            reference: "javac 25".to_owned(),
            root: PathBuf::new(),
            results: vec![
                result(Outcome::Verified),
                result(Outcome::DescriptorMismatch("`A.f` is `()V`".to_owned())),
                result(Outcome::Unverified),
                result(Outcome::LowerError("gap".to_owned())),
                result(Outcome::ParseError(2)),
            ],
            skipped: Vec::new(),
        };
        // parsed 4, lowered 3, re-read 3, verified 2, descriptor-equal 1 — the mismatch counts as
        // verified (a JVM linked it) and stops one rung short, which is the whole point of the rung.
        assert_eq!(report.ladder(), [4, 3, 3, 2, 1]);
    }

    /// A descriptor mismatch is a *rung*, not a defect: the bytes load and run, and only a second
    /// compiler's opinion says they are wrong. Counting it under `--strict` would fail a build for
    /// something no JVM objects to.
    #[test]
    fn a_descriptor_mismatch_is_not_an_invariant_violation() {
        assert!(!Outcome::DescriptorMismatch("`A.f` is `()V`".to_owned()).is_invariant_violation());
        assert!(Outcome::DescriptorMismatch("`A.f` is `()V`".to_owned()).verified());
        assert!(!Outcome::DescriptorMismatch("`A.f` is `()V`".to_owned()).descriptor_equal());
        assert!(Outcome::Verified.descriptor_equal());
    }

    /// A case whose descriptors could not be *judged* is not a case whose descriptors agreed.
    ///
    /// The rung has three answers and only two used to be representable, so "nothing compared"
    /// arrived as `Verified` — the top rung, reached by failing open. It counts as `verified` (a JVM
    /// linked the bytes, which is a real answer) and not as `descriptor-equal`, and it is a corpus
    /// problem rather than a defect, so `--strict` ignores it exactly as it ignores a `ReadError`.
    #[test]
    fn unjudged_descriptors_are_not_agreement() {
        let unjudged = Outcome::DescriptorsUnjudged(Unjudged::Unreadable);
        assert!(unjudged.verified());
        assert!(!unjudged.descriptor_equal());
        assert!(!unjudged.is_invariant_violation());
        assert_eq!(unjudged.label(), "descriptors-unjudged");
        // And it is *visible*: half of what separates it from agreement is that a reader can see
        // the rung went unjudged, which is the bucket.
        assert_eq!(
            unjudged.bucket().as_deref(),
            Some("javac's own class files for the case could not be read")
        );
    }

    /// Each reason the rung goes unjudged says its own thing, because only one of the three is the
    /// corpus problem the sentence used to claim for all of them.
    #[test]
    fn each_unjudged_reason_says_which_one_it_is() {
        let details: Vec<&str> = [
            Unjudged::Unreadable,
            Unjudged::NoSharedType,
            Unjudged::NoSharedMethod,
        ]
        .into_iter()
        .map(Unjudged::detail)
        .collect();
        assert_eq!(
            details,
            [
                "javac's own class files for the case could not be read",
                "jals emitted no type javac also named, so no descriptor was compared",
                "neither compiler declares a method the other does, so no descriptor was compared",
            ]
        );
        // And the reason is what the outcome prints, so the listing shows which one it was.
        assert_eq!(
            Outcome::DescriptorsUnjudged(Unjudged::NoSharedMethod).detail(),
            Some(Unjudged::NoSharedMethod.detail())
        );
    }

    /// The fold from a linked case to its rung reads all three answers, and the ladder counts what
    /// each one earns.
    #[test]
    fn the_descriptor_rung_counts_only_what_was_compared() {
        let case = |outcome: Outcome, descriptors: Descriptors| CaseResult {
            rel: PathBuf::from("A.java"),
            outcome,
            descriptors,
        };
        let mut results = vec![
            case(Outcome::Verified, Descriptors::Agreed),
            case(
                Outcome::Verified,
                Descriptors::Unjudged(Unjudged::Unreadable),
            ),
            case(
                Outcome::Verified,
                Descriptors::Disagreed("`A.f` is `()V`".to_owned()),
            ),
        ];
        CompileReport::apply_descriptor_rung(&mut results);
        assert_eq!(
            results
                .iter()
                .map(|r| r.outcome.label())
                .collect::<Vec<_>>(),
            ["descriptor-equal", "descriptors-unjudged", "verified"]
        );
        let report = CompileReport {
            name: "corpus".to_owned(),
            reference: "javac 25".to_owned(),
            root: PathBuf::from("."),
            results,
            skipped: Vec::new(),
        };
        // Three linked cases, one of which agreed and one of which was never compared.
        assert_eq!(report.ladder(), [3, 3, 3, 3, 1]);
    }

    fn result(outcome: Outcome) -> CaseResult {
        CaseResult {
            rel: PathBuf::from("A.java"),
            outcome,
            // Already applied: `Report::new` folds this into the outcome before anything reads it.
            descriptors: Descriptors::Unjudged(Unjudged::Unreadable),
        }
    }

    #[test]
    fn markdown_report_has_a_row_per_corpus() {
        let report = CompileReport {
            name: "langtools".to_owned(),
            reference: "javac 25".to_owned(),
            root: PathBuf::new(),
            results: vec![result(Outcome::Verified)],
            skipped: vec![Skipped {
                reason: "cannot find symbol".to_owned(),
            }],
        };
        let markdown = CompileReport::markdown_report(std::slice::from_ref(&report), 20);
        assert!(
            markdown.contains("| corpus |"),
            "missing header:\n{markdown}"
        );
        assert!(markdown.contains("langtools"), "missing row:\n{markdown}");
        assert!(
            markdown.contains("out of scope"),
            "the skipped set has to be visible, or the denominator reads as the whole suite:\n{markdown}"
        );
    }

    /// Every registered corpus has to be one the generator writes.
    #[test]
    fn every_corpus_is_generated_by_the_script() {
        let script = repo_file("scripts/gen-javac-corpus.sh");
        for source in COMPILE_SOURCES {
            assert!(
                script.contains(source.root_rel),
                "{}: gen-javac-corpus.sh writes no `{}`",
                source.name,
                source.root_rel
            );
        }
    }

    /// Read a file that lives beside this crate, by a path relative to its manifest dir.
    fn repo_file(rel: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        fs::read_to_string(&path).unwrap_or_else(|_| panic!("{rel} should exist"))
    }

    /// The pin is half the measurement's definition, so CI has to run the JDK it names.
    ///
    /// The rate depends on the host JDK twice: javac decides which files are in scope, and its
    /// `ct.sym` supplies the classpath the analysis resolves against. A corpus generated under
    /// one release and scored under another is two measurements wearing one number.
    #[test]
    fn the_javac_pin_matches_ci() {
        let ci = repo_file("../.github/workflows/ci.yml");
        let entry = format!("JAVAC_VERSION: \"{JAVAC_PIN}\"");
        assert!(
            ci.contains(&entry),
            "ci.yml does not pin `{entry}` — JAVAC_PIN and the workflow have drifted"
        );
    }

    /// The corpus generator has to compile with the pinned release too, not just any javac.
    #[test]
    fn the_generator_states_the_pin() {
        let script = repo_file("scripts/gen-javac-corpus.sh");
        assert!(
            script.contains(&format!("JAVAC_PIN={JAVAC_PIN}")),
            "gen-javac-corpus.sh does not state the pinned release {JAVAC_PIN}"
        );
    }
}
