//! The agreement rung: ask javac's own class files what the module should have answered.
//!
//! A module can be perfectly well-typed and compute the wrong number, so the validator
//! structurally cannot reach this. The second opinion is the same one
//! [`compile`](crate::compile)'s descriptor rung uses — javac's output, sitting in `expected/`
//! beside every case — except that here it is *run* rather than read.
//!
//! # What is compared, and what is not
//!
//! A wasm export carries a bare method name and no owner, so a pairing is only defined where javac
//! declares that name exactly once in the case, `static`, over a parameter list this harness can
//! spell — which means primitives, because inventing a receiver or a `String` would be inventing
//! the test rather than running it. Everything else is out of scope and answers
//! [`Unjudged`](super::Unjudged) rather than agreement.
//!
//! javac's methods are read with **no visibility filter**, because the backend exports with none:
//! `is_static && !is_constructor` is the whole export rule, so a package-private `static` is on the
//! module's surface and has to be on the oracle's too. That is why the driver uses
//! `getDeclaredMethod` and `setAccessible`, not `getMethod`.
//!
//! # The two sides are made symmetric on purpose
//!
//! Three asymmetries would otherwise manufacture disagreements that say nothing about the compiler:
//!
//! - **State.** `wasmtime run --invoke` is a fresh process, so the module's globals are reinitialised
//!   for every call. The driver therefore gives every call a class loader of its own, so a `static`
//!   field a previous call wrote is not visible to the next one on either side.
//! - **Width and sign.** wasm has `i32`/`i64`/`f32`/`f64`; Java has `boolean`, `byte`, `char` and
//!   `short` besides. The comparison is on a canonical form derived from javac's *descriptor*, so a
//!   `byte` the backend forgot to narrow is a disagreement rather than a formatting difference. A
//!   floating-point result is compared as its bit pattern, with `NaN` canonicalised — the two sides
//!   print it differently (`NaN` against `NaN`, `-0` against `-0.0`) and neither spelling is the value.
//! - **Failure.** A trap and a thrown exception are *not* folded together. Collapsing them would
//!   hide the finding this rung exists for: a missing bounds check reads garbage where the JVM
//!   throws, and that is a miscompile wearing the shape of an agreement. A call that failed on both
//!   sides is counted apart from one that agreed, and a call that failed on only one is a finding.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::prelude::*;

use super::engine::{Engine, Invocation};
use super::{CaseResult, Outcome, Unjudged};

/// A Java primitive, which is the whole of what this rung can pass and read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prim {
    Boolean,
    Byte,
    Char,
    Short,
    Int,
    Long,
    Float,
    Double,
}

impl Prim {
    /// The descriptor letter, or `None` for anything that is not a primitive.
    const fn from_letter(letter: u8) -> Option<Self> {
        match letter {
            b'Z' => Some(Self::Boolean),
            b'B' => Some(Self::Byte),
            b'C' => Some(Self::Char),
            b'S' => Some(Self::Short),
            b'I' => Some(Self::Int),
            b'J' => Some(Self::Long),
            b'F' => Some(Self::Float),
            b'D' => Some(Self::Double),
            _ => None,
        }
    }

    /// The values this rung passes for a parameter of this type, in a fixed order.
    ///
    /// Deliberately small and deliberately not a product: the tuples are formed by indexing every
    /// parameter's list at the same position, so a method of six parameters costs the same six
    /// calls as one of two. A corpus of this size cannot afford a cartesian product and does not
    /// need one — these exist to catch a lowering that is wrong for *every* input, which is what a
    /// missing narrowing, an inverted comparison or a dropped operand are.
    ///
    /// Every value is exact in both `f32` and `f64`, so a decimal that both a Rust parser and
    /// `Float.parseFloat` read gives the same bits on both sides.
    const fn samples(self) -> &'static [&'static str] {
        match self {
            Self::Boolean => &["0", "1", "0", "1", "0", "1"],
            Self::Byte => &["0", "1", "-1", "127", "-128", "42"],
            Self::Char => &["0", "1", "65", "65535", "32", "97"],
            Self::Short => &["0", "1", "-1", "32767", "-32768", "7"],
            Self::Int => &["0", "1", "-1", "7", "2147483647", "-2147483648"],
            Self::Long => &[
                "0",
                "1",
                "-1",
                "7",
                "9223372036854775807",
                "-9223372036854775808",
            ],
            Self::Float | Self::Double => &["0", "1", "-1", "0.5", "-2.25", "3.5"],
        }
    }
}

/// Descriptor arithmetic: what a method takes, what it returns, and how a result is spelled.
///
/// Reachable from [`super`] because the pairing there needs [`is_callable`](Self::is_callable) to
/// decide which of javac's methods can enter the rung at all.
pub(super) struct Signature;

impl Signature {
    /// How many argument tuples one method is called with.
    const TUPLES: usize = 6;

    /// Whether javac's descriptor names a method this rung can call: primitives throughout, and a
    /// primitive or `void` result.
    pub(super) fn is_callable(descriptor: &str) -> bool {
        Self::split(descriptor).is_some()
    }

    /// `(parameters, result)` for a callable descriptor; `None` result means `void`.
    fn split(descriptor: &str) -> Option<(Vec<Prim>, Option<Prim>)> {
        let (params, result) = descriptor.strip_prefix('(')?.split_once(')')?;
        let params: Option<Vec<Prim>> = params
            .bytes()
            .map(Prim::from_letter)
            .collect::<Option<Vec<_>>>();
        let params = params?;
        let result = match result.as_bytes() {
            b"V" => None,
            [letter] => Some(Prim::from_letter(*letter)?),
            _ => return None,
        };
        Some((params, result))
    }

    /// The argument tuples one method is called with, as the decimal strings both sides parse.
    ///
    /// A method of no parameters is called once — there is one call to make and repeating it six
    /// times would repeat one answer six times.
    fn arguments(descriptor: &str) -> Vec<Vec<String>> {
        let Some((params, _)) = Self::split(descriptor) else {
            return Vec::new();
        };
        if params.is_empty() {
            return vec![Vec::new()];
        }
        (0..Self::TUPLES)
            .map(|tuple| {
                params
                    .iter()
                    .map(|param| {
                        let samples = param.samples();
                        (*samples.get(tuple % samples.len()).unwrap_or(&"0")).to_owned()
                    })
                    .collect()
            })
            .collect()
    }

    /// Whether the method returns a value, which is what separates the two kinds of comparison.
    fn returns_value(descriptor: &str) -> bool {
        Self::split(descriptor).is_some_and(|(_, result)| result.is_some())
    }

    /// One engine-printed result as the canonical form the JVM side also produces.
    ///
    /// `wasmtime` prints an `i32`/`i64` in decimal and a float in its own shortest form (`-0`,
    /// `NaN`), so an integer is re-read and re-printed and a float is compared as its bits — the
    /// same value the driver prints through `floatToIntBits` / `doubleToLongBits`, which canonicalise
    /// `NaN` for the same reason.
    fn canonical(descriptor: &str, printed: &str) -> Option<String> {
        let (_, result) = Self::split(descriptor)?;
        let Some(result) = result else {
            // `void`: the engine prints nothing, and completing is the whole of the answer.
            return Some(String::new());
        };
        let printed = printed.trim();
        match result {
            Prim::Float => Some(format!("0x{:08x}", printed.parse::<f32>().ok()?.to_bits())),
            Prim::Double => Some(format!("0x{:016x}", printed.parse::<f64>().ok()?.to_bits())),
            // Every other primitive is an `i32`/`i64` on the wasm side; re-reading and re-printing
            // makes `+0` and `-0` one string without asserting anything about the value.
            _ => Some(printed.parse::<i64>().ok()?.to_string()),
        }
    }
}

/// What one call answered on each side.
#[derive(Debug, Clone)]
enum Verdict {
    /// Both sides completed and said the same thing.
    Same,
    /// Both sides completed and said different things — a miscompile.
    Differs(String),
    /// One side failed where the other completed — also a miscompile, and the shape a missing
    /// check takes.
    OneFailed(String),
    /// Both sides failed. Counted apart from agreement: a trap and a thrown exception are not the
    /// same event, and this rung does not yet claim they are.
    BothFailed,
    /// The call never happened — the driver could not load the class or find the method. A harness
    /// problem, kept apart from every finding.
    Undone,
}

/// One call this rung makes on both sides.
struct Call {
    /// Which case it belongs to, by index into the results.
    case: usize,
    /// The case's path, so the engine can find the staged module.
    rel: PathBuf,
    /// Where javac's class files for the case live.
    expected: PathBuf,
    /// The binary name of the class javac declared the method in.
    owner: String,
    /// The method name, which is also the wasm export name.
    name: String,
    /// javac's descriptor.
    descriptor: String,
    /// The arguments, as the decimal strings both sides parse.
    args: Vec<String>,
}

/// Runs javac's own class files on a real JVM and compares what they answer.
pub struct Oracle {
    /// Scratch directory for the driver's case list.
    scratch: tempfile::TempDir,
}

impl Oracle {
    /// An oracle with a scratch directory of its own.
    ///
    /// # Errors
    /// If the scratch directory cannot be created.
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            scratch: tempfile::tempdir().map_err(|e| format!("scratch directory: {e}"))?,
        })
    }

    /// Whether a JVM able to run the driver is on this host, saying so when it is not.
    pub fn jvm_available() -> bool {
        let present = Command::new("java")
            .arg("-version")
            .output()
            .is_ok_and(|output| output.status.success());
        if !present {
            eprintln!(
                "warning: no `java` on this host — the modules will be validated and run, but\n\
                 warning: nothing will say whether they compute what javac's own output computes."
            );
        }
        present
    }

    /// Run every jointly-callable method on both sides and fold the verdicts into `results`.
    pub(super) fn judge_all(&self, root: &Path, results: &mut [CaseResult], engine: &Engine) {
        let calls = Self::plan(root, results);
        if calls.is_empty() {
            Self::decline(results);
            return;
        }
        let ours: Vec<Invocation> = calls
            .par_iter()
            .map(|call| engine.invoke(&call.rel, &call.name, &call.args))
            .collect();
        let theirs = self.ask_jvm(&calls);

        // Kept per case with the call beside it: the two comparison counts the report separates
        // are a property of the call's descriptor, not of the verdict.
        let mut verdicts: BTreeMap<usize, Vec<(usize, Verdict)>> = BTreeMap::new();
        for (index, call) in calls.iter().enumerate() {
            let verdict = Self::compare(call, &ours[index], theirs.get(&index));
            verdicts
                .entry(call.case)
                .or_default()
                .push((index, verdict));
        }
        Self::apply(results, &calls, &verdicts);
    }

    /// Every call the rung will make, and the cases that offer none.
    fn plan(root: &Path, results: &[CaseResult]) -> Vec<Call> {
        let mut calls = Vec::new();
        for (case, result) in results.iter().enumerate() {
            if result.outcome != Outcome::NotRun {
                continue;
            }
            for callable in &result.callable {
                if callable.descriptor.is_empty() {
                    continue;
                }
                let expected = super::Case::expected_dir(&root.join(&result.rel));
                for args in Signature::arguments(&callable.descriptor) {
                    calls.push(Call {
                        case,
                        rel: result.rel.clone(),
                        expected: expected.clone(),
                        owner: callable.owner.clone(),
                        name: callable.name.clone(),
                        descriptor: callable.descriptor.clone(),
                        args,
                    });
                }
            }
        }
        calls
    }

    /// Every case that reached this rung and offered it nothing keeps its own reason for that.
    fn decline(results: &mut [CaseResult]) {
        for result in results.iter_mut() {
            if result.outcome == Outcome::NotRun {
                let reason = result.why_unjudged();
                result.outcome = Outcome::Unjudged(reason);
            }
        }
    }

    /// Ask the driver to make every call on a real JVM, keyed by the call's index.
    fn ask_jvm(&self, calls: &[Call]) -> BTreeMap<usize, Result<String, String>> {
        let list = self.scratch.path().join("calls.tsv");
        let mut text = String::new();
        for (index, call) in calls.iter().enumerate() {
            text.push_str(&format!(
                "{index}\t{}\t{}\t{}\t{}\t{}\n",
                call.expected.display(),
                call.owner,
                call.name,
                call.descriptor,
                call.args.join(",")
            ));
        }
        if std::fs::write(&list, text).is_err() {
            return BTreeMap::new();
        }
        let Some(stdout) = Self::run_driver(&list) else {
            return BTreeMap::new();
        };
        Self::parse(&stdout)
    }

    /// Run the invocation driver over a call list and return its stdout.
    ///
    /// A driver that could not be started leaves every call [`Undone`](Verdict::Undone) rather than
    /// failing the run: this rung is a measurement, and a host that cannot supply a JVM has said
    /// nothing about the modules.
    fn run_driver(list: &Path) -> Option<String> {
        let driver = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("invoke")
            .join("Invoke.java");
        // Source-file mode (JEP 330), as the verifier driver is: nothing to build or vendor.
        let output = Command::new("java")
            .arg("-XX:-UsePerfData")
            .arg(&driver)
            .arg(list)
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                eprintln!("warning: could not run the invocation driver: {error}");
                return None;
            }
        };
        if !output.status.success() {
            eprintln!(
                "warning: the invocation driver exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// `VAL` becomes the canonical value, `EXC` the exception, and `ERR` no entry at all.
    fn parse(stdout: &str) -> BTreeMap<usize, Result<String, String>> {
        let mut out = BTreeMap::new();
        for line in stdout.lines() {
            let mut fields = line.splitn(3, '\t');
            let (Some(kind), Some(key)) = (fields.next(), fields.next()) else {
                continue;
            };
            let Ok(index) = key.parse::<usize>() else {
                continue;
            };
            let detail = fields.next().unwrap_or_default().to_owned();
            match kind {
                "VAL" => out.insert(index, Ok(detail)),
                "EXC" => out.insert(index, Err(detail)),
                _ => None,
            };
        }
        out
    }

    /// What one call says once both sides have answered.
    fn compare(call: &Call, ours: &Invocation, theirs: Option<&Result<String, String>>) -> Verdict {
        let Some(theirs) = theirs else {
            return Verdict::Undone;
        };
        let what = format!("{}.{}{}", call.owner, call.name, call.descriptor);
        let args = call.args.join(", ");
        match (ours, theirs) {
            (Invocation::Returned(printed), Ok(expected)) => {
                let Some(ours) = Signature::canonical(&call.descriptor, printed) else {
                    return Verdict::Differs(format!(
                        "`{what}` on ({args}) returned `{printed}`, which is not a result this \
                         harness can read"
                    ));
                };
                if &ours == expected {
                    Verdict::Same
                } else {
                    Verdict::Differs(format!(
                        "`{what}` on ({args}) answered `{ours}` where javac's own class file \
                         answers `{expected}`"
                    ))
                }
            }
            (Invocation::Trapped(trap), Ok(expected)) => Verdict::OneFailed(format!(
                "`{what}` on ({args}) trapped where javac's own class file answers `{expected}`: \
                 {trap}"
            )),
            (Invocation::Returned(printed), Err(thrown)) => Verdict::OneFailed(format!(
                "`{what}` on ({args}) answered `{printed}` where javac's own class file throws \
                 {thrown}"
            )),
            (Invocation::Trapped(_), Err(_)) => Verdict::BothFailed,
        }
    }

    /// Fold every call's verdict back onto its case.
    ///
    /// Worst-wins, and a disagreement is the worst: one method that computes something else is a
    /// miscompile whatever its siblings did.
    fn apply(
        results: &mut [CaseResult],
        calls: &[Call],
        verdicts: &BTreeMap<usize, Vec<(usize, Verdict)>>,
    ) {
        for (case, result) in results.iter_mut().enumerate() {
            if result.outcome != Outcome::NotRun {
                continue;
            }
            let Some(list) = verdicts.get(&case) else {
                result.outcome = Outcome::Unjudged(result.why_unjudged());
                continue;
            };
            if let Some(finding) = list.iter().find_map(|(_, verdict)| match verdict {
                Verdict::Differs(message) | Verdict::OneFailed(message) => Some(message.clone()),
                _ => None,
            }) {
                result.outcome = Outcome::Disagreed(finding);
                continue;
            }
            for (index, verdict) in list {
                if !matches!(verdict, Verdict::Same) {
                    continue;
                }
                if Signature::returns_value(&calls[*index].descriptor) {
                    result.valued += 1;
                } else {
                    result.completions += 1;
                }
            }
            if result.valued + result.completions == 0 {
                result.outcome = Outcome::Unjudged(
                    if list.iter().any(|(_, v)| matches!(v, Verdict::BothFailed)) {
                        Unjudged::BothFailed
                    } else {
                        Unjudged::Unreadable
                    },
                );
                continue;
            }
            result.outcome = Outcome::Agreed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a signature this rung can actually spell enters the pairing. A reference anywhere —
    /// `String[] args` most of all — takes the method out, because inventing a receiver would be
    /// inventing the test.
    #[test]
    fn only_primitive_signatures_are_callable() {
        assert!(Signature::is_callable("()I"));
        assert!(Signature::is_callable("(IJ)Z"));
        assert!(Signature::is_callable("(I)V"), "a void result is callable");
        assert!(!Signature::is_callable("([Ljava/lang/String;)V"));
        assert!(!Signature::is_callable("(Ljava/lang/Object;)I"));
        assert!(!Signature::is_callable("()Ljava/lang/String;"));
        assert!(!Signature::is_callable("()[I"));
    }

    /// The tuples index every parameter's list at the same position rather than crossing them, so
    /// a method of six parameters costs six calls and not six to the sixth.
    #[test]
    fn argument_tuples_are_one_per_index_and_not_a_product() {
        let tuples = Signature::arguments("(III)I");
        assert_eq!(tuples.len(), Signature::TUPLES);
        for tuple in &tuples {
            assert_eq!(tuple.len(), 3);
            assert!(
                tuple.windows(2).all(|pair| pair[0] == pair[1]),
                "one index across every parameter of one type"
            );
        }
        // A method of no parameters has one call to make; repeating it would repeat one answer.
        assert_eq!(Signature::arguments("()I").len(), 1);
        assert!(Signature::arguments("()I")[0].is_empty());
    }

    /// Every sample is a decimal both a Rust parser and `Float.parseFloat` read to the same bits.
    #[test]
    fn every_sample_parses_on_both_sides() {
        for tuple in Signature::arguments("(ZBCSIJFD)V") {
            for (index, text) in tuple.iter().enumerate() {
                match index {
                    6 | 7 => assert!(text.parse::<f64>().is_ok(), "{text} is not a number"),
                    _ => assert!(text.parse::<i64>().is_ok(), "{text} is not an integer"),
                }
            }
        }
    }

    /// An integer result is compared as a number and not as the engine's spelling of one.
    #[test]
    fn an_integer_result_is_read_back_as_a_number() {
        assert_eq!(Signature::canonical("()I", "-1").unwrap(), "-1");
        assert_eq!(Signature::canonical("()Z", "1").unwrap(), "1");
        assert_eq!(Signature::canonical("()C", "65535").unwrap(), "65535");
        assert_eq!(
            Signature::canonical("()V", "").unwrap(),
            "",
            "completing is the whole of a void answer"
        );
    }

    /// A float is compared as its bits, because neither side's printed decimal is the value:
    /// `wasmtime` prints `-0` where Java prints `-0.0`, and a NaN payload is free to differ.
    #[test]
    fn a_float_is_compared_as_its_bits() {
        assert_eq!(Signature::canonical("()F", "-0").unwrap(), "0x80000000");
        assert_eq!(Signature::canonical("()F", "0").unwrap(), "0x00000000");
        assert_ne!(
            Signature::canonical("()F", "-0").unwrap(),
            Signature::canonical("()F", "0").unwrap(),
            "a signed zero is not the same value"
        );
        // `Float.floatToIntBits` collapses every NaN onto this one, and so does Rust's `f32::NAN`.
        assert_eq!(Signature::canonical("()F", "NaN").unwrap(), "0x7fc00000");
        assert_eq!(
            Signature::canonical("()D", "NaN").unwrap(),
            "0x7ff8000000000000"
        );
    }

    /// Which of the two comparison counts a call feeds is a property of javac's descriptor.
    #[test]
    fn a_void_method_is_a_completion_and_not_a_value() {
        assert!(Signature::returns_value("()I"));
        assert!(!Signature::returns_value("(I)V"));
    }
}
