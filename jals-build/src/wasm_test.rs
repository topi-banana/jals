//! Running a project's tests on the WebAssembly engine compiled into this binary.
//!
//! The counterpart to [`TestLauncher`](crate::TestLauncher), and it shares everything about a test
//! run that is not *how a test is reached*: the same [`TestCase`], the same [`TestFilter`]
//! selection, the same [`RunOptions`], the same [`TestEvent`] stream and the same [`TestOutcome`].
//! Sharing the plan is not a convenience — a selection that differed between the two runners would
//! make `--partition count:2/3` mean two things.
//!
//! # The verdict moved
//!
//! On a JVM the generated harness owns the verdict, because the runner only ever sees text: it
//! prints a sentinel and the runner looks for the line. Here the runner holds a typed
//! `Result<WasmRunOutcome, WasmRunError>`, so it decides — and it has to, because the other half of
//! the JVM harness's job is impossible on this target. Inverting `#[should_fail]` needs
//! `catch (Throwable)`, and a `catch` type has to be a class the module declares.
//!
//! # One instantiation per test, and one before them all
//!
//! [`WasmRunner::run`] builds a fresh `Store` per call, so isolation is free: no static state
//! survives from one test into the next, which is what one JVM per test buys on the other side at
//! a far higher price.
//!
//! It also means every call runs the module's start function — the lowering of every `static`
//! initialiser the project declares. A `static {}` that traps therefore fails *every* test
//! identically, and at the call site that trap is indistinguishable from one the test body caused.
//! Left there, a project with a trapping initialiser would report every `#[should_fail]` test as
//! **passed**. [`WasmTestLauncher::run`] instantiates once before the first test and fails the
//! whole run instead, which is what makes a trap seen later mean the body and only the body — the
//! same job the JVM path's `JalsTestHarness.class` probe does.
//!
//! In [`run`](WasmTestLauncher::run) and not in [`resolve`](WasmTestLauncher::resolve), because
//! the probe *executes the project's code* and only a command that runs a test may do that.
//! `jals test --list` builds a launcher and never runs one: the JVM path answers it from the
//! harness's own listing arm, loading no test class and running no `<clinit>`, and a `--list` that
//! instantiated here would fail — or, on a `static {}` that never returns, hang — over a command
//! that executes nothing. The two runners have to answer `--list` alike.

use alloc::borrow::ToOwned as _;
use alloc::format;
use alloc::string::{String, ToString as _};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use jals_exec::Exec;
use jals_native::NativeBindings;
use jals_progress::Progress;

use crate::test_plan::TestCase;
use crate::test_runner::{Permits, RunOptions, TestEvent, TestOutcome, TestVerdict};
use crate::wasm_run::{ParsedModule, WasmRunError, WasmRunOutcome, WasmRunner};

/// One test, as this runner addresses it.
///
/// Passed in rather than derived here: which export runs a test is decided by the harness
/// `jals-frontend` generates, which owns that name, and re-deriving the mangling in this crate
/// would be a second place for it to live — and the two drifting apart is a run that lists a test
/// and then cannot find it. `jals-build` does not depend on `jals-frontend`, and must not; a host
/// reads each field from there and hands it over without interpreting any of it.
#[derive(Debug, Clone)]
pub struct WasmTestEntry {
    /// `com.example.MathTest#adds`.
    pub id: String,
    /// The exported function that runs it.
    pub export: String,
    /// Declared `#[ignore]`.
    pub ignore: bool,
    /// Declared `#[should_fail]`.
    pub should_fail: bool,
}

/// A module and the tests exported from it, ready to run.
#[derive(Debug)]
pub struct WasmTestLauncher {
    /// The module, decoded and validated once. Decoding is linear in the module's size and by far
    /// the most expensive part of a call — instantiating it is microseconds — so a suite that runs
    /// one module once per test pays it here rather than N times. It is also where a module that
    /// is not one has to fail: [`resolve`](Self::resolve) is already the step that refuses a
    /// module the whole run could not have used.
    module: ParsedModule,
    entries: Vec<WasmTestEntry>,
    /// The implementations of every `native` method the module imports.
    ///
    /// Empty for a project that selected no package, which is what makes the fan-out below the
    /// ordinary path rather than a special case — see [`run`](Self::run).
    bindings: NativeBindings,
}

impl WasmTestLauncher {
    /// Take the module the backend just produced and the tests the frontend found.
    ///
    /// Decodes and validates the module — bytes that are not one are refused here, before
    /// anything else — and executes none of it. Running the start function is
    /// [`run`](Self::run)'s, for the reason the module docs give: it is the project's own code,
    /// and `jals test --list` reaches this constructor while running no test at all.
    pub fn resolve(
        module: &[u8],
        entries: Vec<WasmTestEntry>,
        bindings: NativeBindings,
    ) -> Result<Self, WasmRunError> {
        Ok(Self {
            module: WasmRunner::parse(module)?,
            entries,
            bindings,
        })
    }

    /// The tests this module holds, in the order the frontend emitted them.
    ///
    /// The mirror of [`TestLauncher::list`](crate::TestLauncher::list), and the reason it needs no
    /// module of its own: a `--list` spawn asks the compiled harness because printing is the only
    /// way it can answer, and here the answer came with the entries.
    #[must_use]
    pub fn list(&self) -> Vec<TestCase> {
        self.entries
            .iter()
            .map(|entry| TestCase::from_parts(entry.id.clone(), entry.ignore, entry.should_fail))
            .collect()
    }

    /// Run `cases`, reporting each start and finish through `observe`.
    ///
    /// Results come back in the order `cases` were given — the order a summary is printed in —
    /// while `observe` fires in completion order, which is what a live progress display needs.
    /// Both properties are `Exec::fan_out`'s, exactly as on the JVM path.
    ///
    /// A run that links a native package runs its cases **in order on this task** rather than
    /// fanning out: a package's implementations capture the host's own state and are `!Send`, and
    /// two workers writing one console would interleave the suite's output in any case.
    ///
    /// Instantiates once before the first test and returns that failure whole rather than running
    /// anything — the precondition the `#[should_fail]` inversion rests on, described in the
    /// module docs. Silent: the probe is machinery rather than work the caller asked about, and a
    /// `Run` event here reads as the suite starting when nothing has run yet.
    pub async fn run(
        &self,
        cases: &[TestCase],
        options: RunOptions,
        observe: Arc<dyn Fn(TestEvent) + Send + Sync>,
        exec: &Exec,
    ) -> Result<Vec<TestOutcome>, WasmRunError> {
        WasmRunner::run_parsed(&self.module, None, &[], &self.bindings, &Progress::SILENT)?;
        let shared = Arc::new(SharedWasmRun {
            module: self.module.clone(),
            exports: self
                .entries
                .iter()
                .map(|entry| (entry.id.clone(), entry.export.clone()))
                .collect(),
            permits: Permits::new(options.threads.max(1)),
            failures: AtomicUsize::new(0),
            max_fail: options.max_fail,
            observe,
        });
        // A native package's implementations are host closures over the host's own state — a
        // console buffer, a counter — so they are `!Send` by construction and cannot reach a
        // fan-out worker. That is not a limitation to route around: two workers writing one
        // console would interleave a suite's output anyway. So a run that links a package runs its
        // cases in order on this task, and one that links none fans out exactly as before.
        if self.bindings.is_empty() {
            let jobs: Vec<_> = cases
                .iter()
                .map(|case| (case.clone(), Arc::clone(&shared)))
                .collect();
            return Ok(exec
                .fan_out(jobs, |(case, shared)| async move {
                    shared.run_one(&case, &NativeBindings::new())
                })
                .await);
        }
        let mut outcomes = Vec::with_capacity(cases.len());
        for case in cases {
            outcomes.push(shared.run_one(case, &self.bindings));
        }
        Ok(outcomes)
    }
}

/// What every worker shares for one run.
struct SharedWasmRun {
    module: ParsedModule,
    /// Test id to the export that runs it. A `Vec` rather than a map: a suite is small enough that
    /// the scan is free, and it keeps the frontend's order visible.
    exports: Vec<(String, String)>,
    permits: Permits,
    failures: AtomicUsize,
    max_fail: Option<usize>,
    observe: Arc<dyn Fn(TestEvent) + Send + Sync>,
}

impl SharedWasmRun {
    /// Whether `--max-fail` has already been reached.
    fn exhausted(&self) -> bool {
        self.max_fail
            .is_some_and(|limit| self.failures.load(Ordering::Relaxed) >= limit)
    }

    fn run_one(&self, case: &TestCase, bindings: &NativeBindings) -> TestOutcome {
        if self.exhausted() {
            return Self::never_started(case);
        }
        let _permit = self.permits.acquire();
        // Re-checked with the permit in hand: the limit may have been reached while this job was
        // waiting for one.
        if self.exhausted() {
            return Self::never_started(case);
        }
        (self.observe)(TestEvent::Started(case.id().to_owned()));

        let started = Instant::now();
        let (verdict, detail) = self.judge(case, bindings);
        let outcome = TestOutcome {
            id: case.id().to_owned(),
            verdict,
            duration: started.elapsed(),
            // Always one: a wasm run has no clock, no network, no threads and no filesystem, and a
            // fresh store per test — so a second attempt recomputes the identical answer, which is
            // why `--retries` is refused rather than honoured.
            attempts: 1,
            // A module has no standard output to capture. `--failure-output` therefore has nothing
            // to replay, and `detail` below is the whole account of a failure.
            stdout: None,
            stderr: None,
            detail,
        };
        if outcome.verdict.is_failure() {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
        (self.observe)(TestEvent::Finished(outcome.clone()));
        outcome
    }

    /// Call the test's export and read the verdict off what the call did.
    ///
    /// A trap and an uncaught throw are one verdict and two messages: they are different things to
    /// tell a reader, and identical as an answer — on a JVM both are `Throwable`s the shim's
    /// `catch (Throwable)` catches, which is what keeps a `#[should_fail]` test that divides by
    /// zero passing here too.
    ///
    /// Everything else the engine can return is this runner failing to *reach* the test, and is
    /// never inverted: a missing export reported as a pass because the test was expected to fail
    /// is a test that did not run claiming it did.
    fn judge(&self, case: &TestCase, bindings: &NativeBindings) -> (TestVerdict, Option<String>) {
        let Some((_, export)) = self.exports.iter().find(|(id, _)| id == case.id()) else {
            return (
                TestVerdict::Failed { code: None },
                Some(format!("no export was generated for `{}`", case.id())),
            );
        };
        // Silent: a test run reports through `observe`, which is what the reporter draws from, and
        // a second `Run` event per test would put the engine's own activity beside it saying the
        // same thing in another vocabulary.
        let result =
            WasmRunner::run_parsed(&self.module, Some(export), &[], bindings, &Progress::SILENT);
        match result {
            Ok(WasmRunOutcome::Returned(_) | WasmRunOutcome::Instantiated) => {
                if case.should_fail() {
                    (
                        TestVerdict::Failed { code: None },
                        Some("the test returned normally and was expected to fail".to_owned()),
                    )
                } else {
                    (TestVerdict::Passed, None)
                }
            }
            Err(error) if error.is_execution_failure() => {
                if case.should_fail() {
                    (TestVerdict::Passed, None)
                } else {
                    (TestVerdict::Failed { code: None }, Some(error.to_string()))
                }
            }
            Err(error) => (TestVerdict::Failed { code: None }, Some(error.to_string())),
        }
    }

    /// The outcome of a test `--max-fail` stopped before it ever ran.
    fn never_started(case: &TestCase) -> TestOutcome {
        TestOutcome {
            id: case.id().to_owned(),
            verdict: TestVerdict::Skipped,
            duration: core::time::Duration::ZERO,
            attempts: 0,
            stdout: None,
            stderr: None,
            detail: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, BackendOptions, BackendRequest, BackendSource};
    use crate::jals_backend::JalsBackend;
    use crate::test_runner::RunOptions;
    use jals_storage::{CacheKey, CacheNamespace, ContentDigest, RelativePath};

    /// Compile one Java source with the wasm backend, assertions armed, and hand back the module.
    ///
    /// The whole fixture is in-crate, exactly as `wasm_run`'s is: the backend that produced the
    /// bytes lives here, so a test needs no external tool and no committed binary.
    ///
    /// The wrapper class a test declares is **hand-written** in each fixture rather than generated,
    /// and that is deliberate. This crate has no `jals-frontend` dependency and must not grow one,
    /// so writing the wrapper out pins the *contract* between the two crates — a wrapper is a
    /// `static void` with a project-unique name that calls the test and nothing else — rather than
    /// pinning one generator against itself.
    fn module(text: &str) -> Vec<u8> {
        let bytes = text.as_bytes().to_vec();
        let tree = [BackendSource {
            path: RelativePath::parse("Main.java").expect("a valid path"),
            key: CacheKey::new(
                CacheNamespace::FrontendOutput,
                ContentDigest::of(b"wasm-test"),
                ContentDigest::of(&bytes),
            ),
            bytes,
        }];
        let options = BackendOptions::default();
        let request = BackendRequest {
            tree: &tree,
            classpath: &[],
            options: &options,
            progress: &Progress::SILENT,
        };
        let backend = JalsBackend::wasm(
            crate::Assertions::Enabled,
            jals_native::PackageSelection::empty(),
        );
        let outcome =
            jals_exec::block_on_inline(backend.compile(&request)).expect("the backend ran");
        assert!(
            outcome.success(),
            "the wasm backend refused the fixture: {:?}",
            outcome.messages
        );
        outcome.artifacts.into_iter().next().expect("one module").1
    }

    fn entry(id: &str, export: &str, should_fail: bool) -> WasmTestEntry {
        WasmTestEntry {
            id: id.to_owned(),
            export: export.to_owned(),
            ignore: false,
            should_fail,
        }
    }

    /// Run every entry and return `(id, verdict, detail)` in the order given.
    fn verdicts(
        module: &[u8],
        entries: Vec<WasmTestEntry>,
    ) -> Vec<(String, TestVerdict, Option<String>)> {
        let launcher = WasmTestLauncher::resolve(module, entries, NativeBindings::new())
            .expect("the module parses");
        let cases = launcher.list();
        let outcomes = jals_exec::block_on_inline(launcher.run(
            &cases,
            RunOptions::default(),
            Arc::new(|_| {}),
            &Exec::inline(),
        ))
        .expect("the module instantiates");
        outcomes
            .into_iter()
            .map(|outcome| (outcome.id, outcome.verdict, outcome.detail))
            .collect()
    }

    /// A returning export is a pass, and an armed `assert` that does not hold is a failure — which
    /// is the whole reason the compile arms them: without it the second test passes too.
    #[test]
    fn a_returning_test_passes_and_a_failing_assertion_does_not() {
        let module = module(
            "public class T {\n\
             \x20   static void holds() { assert 1 + 1 == 2; }\n\
             \x20   static void fails() { assert 1 + 1 == 3; }\n\
             \x20   public static void JalsTest$T$holds() { T.holds(); }\n\
             \x20   public static void JalsTest$T$fails() { T.fails(); }\n\
             }\n",
        );
        let results = verdicts(
            &module,
            alloc::vec![
                entry("T#holds", "JalsTest$T$holds", false),
                entry("T#fails", "JalsTest$T$fails", false),
            ],
        );
        assert_eq!(results[0].1, TestVerdict::Passed);
        assert_eq!(results[1].1, TestVerdict::Failed { code: None });
        assert!(
            results[1].2.as_ref().is_some_and(|d| d.contains("trap")),
            "a failed assertion is reported as the trap it is: {:?}",
            results[1].2
        );
    }

    /// `#[should_fail]` is inverted by the runner, in both directions.
    ///
    /// It cannot be inverted in the generated Java the way the JVM shim does it: that needs
    /// `catch (Throwable)`, and a catch type has to be a class the module declares.
    #[test]
    fn should_fail_inverts_the_verdict_both_ways() {
        let module = module(
            "public class T {\n\
             \x20   static void boom() { assert false; }\n\
             \x20   static void quiet() {}\n\
             \x20   public static void JalsTest$T$boom() { T.boom(); }\n\
             \x20   public static void JalsTest$T$quiet() { T.quiet(); }\n\
             }\n",
        );
        let results = verdicts(
            &module,
            alloc::vec![
                entry("T#boom", "JalsTest$T$boom", true),
                entry("T#quiet", "JalsTest$T$quiet", true),
            ],
        );
        assert_eq!(results[0].1, TestVerdict::Passed);
        assert_eq!(results[1].1, TestVerdict::Failed { code: None });
        assert!(
            results[1]
                .2
                .as_ref()
                .is_some_and(|d| d.contains("expected to fail")),
            "the reason a passing body failed the run: {:?}",
            results[1].2
        );
    }

    /// An uncaught `throw` is a failure and is inverted like a trap: on a JVM both are `Throwable`s
    /// the shim's `catch (Throwable)` catches, so a suite must not tell them apart either.
    #[test]
    fn an_uncaught_throw_fails_like_a_trap() {
        let module = module(
            "public class T {\n\
             \x20   static class Boom extends RuntimeException {}\n\
             \x20   static void throws_() { throw new Boom(); }\n\
             \x20   public static void JalsTest$T$throws_() { T.throws_(); }\n\
             }\n",
        );
        let plain = verdicts(
            &module,
            alloc::vec![entry("T#throws_", "JalsTest$T$throws_", false)],
        );
        assert_eq!(plain[0].1, TestVerdict::Failed { code: None });
        let inverted = verdicts(
            &module,
            alloc::vec![entry("T#throws_", "JalsTest$T$throws_", true)],
        );
        assert_eq!(inverted[0].1, TestVerdict::Passed);
    }

    /// An export that is not there fails the test and is **never** inverted, even under
    /// `#[should_fail]`: a test the runner could not reach, reported as a pass because it was
    /// expected to fail, is a missing test claiming to have run.
    #[test]
    fn a_missing_export_is_a_failure_that_should_fail_does_not_invert() {
        let module = module("public class T { public static void JalsTest$T$here() {} }");
        let results = verdicts(
            &module,
            alloc::vec![entry("T#gone", "JalsTest$T$gone", true)],
        );
        assert_eq!(results[0].1, TestVerdict::Failed { code: None });
        assert!(
            results[0]
                .2
                .as_ref()
                .is_some_and(|d| d.contains("no function named")),
            "the report names what was not found: {:?}",
            results[0].2
        );
    }

    /// A `static` initialiser that traps fails the whole run, before a single test runs — and
    /// leaves `list` answerable, because listing executes nothing.
    ///
    /// The precondition the whole `#[should_fail]` inversion rests on. Every call instantiates the
    /// module, so a trapping initialiser would trap in every test — and at the call site that is
    /// indistinguishable from a trap the body caused, which would report every `#[should_fail]`
    /// test as passed. The second half is the other side of the same rule: the probe is the
    /// project's code running, so a command that runs no test must not reach it.
    #[test]
    fn a_trapping_static_initialiser_fails_the_run_rather_than_every_test() {
        let module = module(
            "public class T {\n\
             \x20   static int n;\n\
             \x20   static { n = 1 / 0; }\n\
             \x20   public static void JalsTest$T$t() {}\n\
             }\n",
        );
        let launcher = WasmTestLauncher::resolve(
            &module,
            alloc::vec![entry("T#t", "JalsTest$T$t", true)],
            NativeBindings::new(),
        )
        .expect("the module parses");
        let cases = launcher.list();
        assert_eq!(cases.len(), 1, "listing runs none of the module");
        let error = jals_exec::block_on_inline(launcher.run(
            &cases,
            RunOptions::default(),
            Arc::new(|_| {}),
            &Exec::inline(),
        ))
        .expect_err("the module does not instantiate");
        assert!(error.is_execution_failure(), "reported as a trap: {error}");
    }
}
