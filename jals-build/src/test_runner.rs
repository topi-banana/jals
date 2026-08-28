//! Running a compiled test harness: one JVM per test, in parallel, with output captured.
//!
//! Here rather than in `jals-cli` because this crate already owns every host effect the build
//! front end has — tool resolution, argument-file spilling, process spawning — and
//! [`Invocation`] is crate-internal precisely so a host never assembles a command line itself.
//! What stays with the CLI is the part that is presentation: the progress bar, the status lines,
//! and the summary.
//!
//! Three decisions worth stating, because each replaces something more obvious that does not work:
//!
//! - **Output goes to files, not pipes.** Reading two pipes in sequence deadlocks the moment the
//!   unread one fills, and reading both needs a thread each — two more threads per concurrent
//!   test. Redirecting to a file in the test's own scratch directory needs none, cannot deadlock,
//!   and leaves the bytes on disk for `--failure-output` to replay.
//! - **Every test gets a scratch directory of its own.** The shared argument file
//!   `target/jals/build/java-args` is one path; N tests spilling a long classpath into it at once
//!   would overwrite each other's arguments.
//! - **A pass is stated, never inferred.** Exit status 1 is also "could not find or load main
//!   class", and a body calling `System.exit(0)` would otherwise report success. The harness
//!   prints a sentinel line, and only that line is a pass. `--no-capture` is the one exception,
//!   and it is a stated trade: the output goes to the reader's terminal instead of to a file this
//!   runner can read, so the exit status is all that is left.

use alloc::sync::Arc;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use jals_config::Manifest;
use jals_exec::Exec;
use jals_exec::tokio_rt::on_blocking_pool;
use jals_storage::RelativePath;

use crate::invocation::Invocation;
use crate::request::RunRequest;
use crate::test_plan::TestCase;
use crate::toolchain::ToolchainError;

/// The parent of every test run's scratch, relative to the project root. A run owns the
/// subdirectory named by its own process id.
///
/// Under `target/jals/build` rather than beside it, which is what makes `jals clean` remove it:
/// `CleanTargets::keys` returns that root, and a sibling directory would have needed its own entry
/// there — a second place to remember, and one nothing would have failed without.
const TEST_RUN_DIR: &str = "target/jals/build/test-run";

/// `-ea`: enable assertions in the project's own classes.
///
/// Prepended by the launcher rather than asked of the caller, because Java disables `assert`
/// by default and a test suite written with it would otherwise pass without executing a single
/// check — the failure mode that looks exactly like success.
const ENABLE_ASSERTIONS: &str = "-ea";

/// `-esa`: the same for the JDK's own classes.
const ENABLE_SYSTEM_ASSERTIONS: &str = "-esa";

/// How one test ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestVerdict {
    /// The harness printed the sentinel: the body returned normally (or threw, for a
    /// `#[should_fail]` test).
    Passed,
    /// The process ended without printing the sentinel.
    Failed {
        /// The exit status, absent when a signal ended the process.
        code: Option<i32>,
    },
    /// The process outlived `--timeout` and was killed.
    TimedOut,
    /// Never started: an earlier failure ended the run.
    Skipped,
}

impl TestVerdict {
    /// Whether this verdict counts against the run.
    pub const fn is_failure(self) -> bool {
        matches!(self, Self::Failed { .. } | Self::TimedOut)
    }
}

/// What one test did.
#[derive(Debug, Clone)]
pub struct TestOutcome {
    /// The test's id.
    pub id: String,
    pub verdict: TestVerdict,
    /// Wall time of the final attempt.
    pub duration: Duration,
    /// How many times the test ran. `1` unless `--retries` bought it another go.
    pub attempts: u32,
    /// The captured standard output, absent when the run did not capture.
    pub stdout: Option<PathBuf>,
    /// The captured standard error, absent when the run did not capture.
    pub stderr: Option<PathBuf>,
}

impl TestOutcome {
    /// Whether the test ran slower than the run's slow threshold.
    pub fn is_slow(&self, threshold: Option<Duration>) -> bool {
        threshold.is_some_and(|threshold| self.duration >= threshold)
    }
}

/// What a runner reports as it goes.
///
/// A callback rather than a channel: the events cross from a fan-out worker back to whatever is
/// drawing, and a `Fn` is the smallest thing that boundary accepts (`Send + Sync + 'static`).
#[derive(Debug, Clone)]
pub enum TestEvent {
    /// A test's process is about to start.
    Started(String),
    /// A test finished, in completion order.
    Finished(TestOutcome),
}

/// How a run executes what the filters selected.
#[derive(Debug, Clone, Copy)]
pub struct RunOptions {
    /// Maximum tests in flight. Capped by the fan-out's own worker count, which is the machine's
    /// parallelism.
    pub threads: usize,
    /// Extra attempts a failing test is given before it is reported as failed.
    pub retries: u32,
    /// Kill a test that runs longer than this.
    pub timeout: Option<Duration>,
    /// Report a test that ran longer than this as slow. Never kills.
    pub slow_timeout: Option<Duration>,
    /// Stop starting tests once this many have failed. `None` runs everything.
    ///
    /// Bounds what is *started*, not what is running: a job that already holds a permit sees the
    /// count only when it next checks, so a run at `-j N` can report up to `N` failures past the
    /// limit. Narrowing that further would mean killing tests mid-flight, which turns a reported
    /// failure into no result at all.
    pub max_fail: Option<usize>,
    /// Capture the tests' output into files. Off, the tests inherit this process's streams and the
    /// sentinel cannot be read, so the exit status becomes the verdict.
    pub capture: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            threads: 1,
            retries: 0,
            timeout: None,
            slow_timeout: None,
            max_fail: None,
            capture: true,
        }
    }
}

/// The three arguments-and-lines the generated harness and this runner have to agree on.
///
/// Not the harness *class*: that is the main class of the invocation and reaches the launcher on
/// the [`RunRequest`](crate::RunRequest), which is where every other `java` spawn names one.
///
/// Passed in rather than defined here: the harness is generated by `jals-frontend`, which owns the
/// wording, and a second copy in this crate would be a second place for it to drift. A host reads
/// each from there and hands them over without interpreting any of them.
#[derive(Debug, Clone)]
pub struct HarnessContract {
    /// The argument that makes the harness enumerate rather than run.
    pub list_argument: String,
    /// The line the harness prints for a test that passed, before a TAB and the test's id.
    pub ok_sentinel: String,
    /// The argument that suppresses that line, passed when the run is not capturing.
    pub quiet_argument: String,
}

/// A resolved `java` command that runs the project's compiled harness.
pub struct TestLauncher {
    /// The command up to and including the main class: everything but the test ids.
    base: Invocation,
    scratch_root: PathBuf,
    contract: HarnessContract,
}

impl TestLauncher {
    /// Resolve `[toolchain] runtime` and build the command that runs `request`'s main class.
    ///
    /// `request.program_args` is ignored: the arguments of a test run are its test ids, supplied
    /// per process.
    ///
    /// # Errors
    /// [`ToolchainError`] when the scratch directory cannot be prepared.
    pub async fn resolve(
        manifest: &Manifest,
        request: &RunRequest<'_>,
        contract: HarnessContract,
    ) -> Result<Self, ToolchainError> {
        let toolchain = crate::native::SubprocessToolchain::from_manifest(manifest).await;
        // Assertions on, ahead of anything the project asked for, so a project that sets its own
        // `-da` still has the last word.
        let mut jvm_args = vec![
            ENABLE_ASSERTIONS.to_owned(),
            ENABLE_SYSTEM_ASSERTIONS.to_owned(),
        ];
        jvm_args.extend_from_slice(request.jvm_args);
        let planned = RunRequest {
            jvm_args: &jvm_args,
            program_args: &[],
            ..*request
        };
        let invocation = toolchain.plan_run(&planned).await;
        // One directory per *process*, not one shared by every run in the checkout. `resolve`
        // clears what it is about to use, and a shared root would mean a second `jals test` in the
        // same tree deleting the captures the first one is still writing into — every one of its
        // tests then reported as failed. `jals clean` still reaps the parent.
        let scratch_root = request
            .project_root
            .join(
                RelativePath::parse(TEST_RUN_DIR)
                    .expect("the test scratch root is a valid relative path")
                    .to_host_path(Path::new("")),
            )
            .join(std::process::id().to_string());
        let root = scratch_root.clone();
        on_blocking_pool(move || Self::reset_scratch(&root))
            .await
            .map_err(|source| ToolchainError::ArgumentFile {
                path: scratch_root.display().to_string(),
                source,
            })?;
        Ok(Self {
            base: invocation,
            scratch_root,
            contract,
        })
    }

    /// Clear *this process's* scratch so a stale capture can never be read as this run's. A
    /// concurrent run owns a directory of its own and is left alone.
    fn reset_scratch(root: &Path) -> std::io::Result<()> {
        match std::fs::remove_dir_all(root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::create_dir_all(root)
    }

    /// Ask the harness which tests it holds, returning its `--list` lines.
    ///
    /// # Errors
    /// [`ToolchainError::Spawn`] when the JVM cannot be started,
    /// [`ToolchainError::ArgumentFile`] when its output cannot be read, and
    /// [`ToolchainError::HarnessList`] when it started but did not enumerate.
    pub async fn list(&self) -> Result<Vec<TestCase>, ToolchainError> {
        let invocation = Self::invocation_with(
            &self.base,
            core::slice::from_ref(&self.contract.list_argument),
        );
        let directory = self.scratch_root.join("list");
        let outcome = on_blocking_pool(move || Self::execute(&invocation, &directory, None, true))
            .await
            .map_err(|source| ToolchainError::Spawn {
                program: "java".to_owned(),
                source,
            })?;
        let text = std::fs::read_to_string(&outcome.stdout).map_err(|source| {
            ToolchainError::ArgumentFile {
                path: outcome.stdout.display().to_string(),
                source,
            }
        })?;
        // The status is the difference between "this project declares no test" and "the JVM never
        // reached the harness", which produce the same empty standard output. The caller has
        // already established that the harness class exists, so a non-zero status here is the
        // second one and carries the harness's own stderr with it.
        if outcome.status != Some(0) {
            return Err(ToolchainError::HarnessList {
                status: outcome.status,
                stderr: std::fs::read_to_string(&outcome.stderr)
                    .unwrap_or_default()
                    .trim_end()
                    .to_owned(),
            });
        }
        Ok(text.lines().filter_map(TestCase::parse).collect())
    }

    /// Run every case, at most [`RunOptions::threads`] at a time, reporting each through
    /// `observe`.
    ///
    /// Results come back in the order `cases` were given — the order a summary is printed in —
    /// while `observe` fires in completion order, which is what a live progress display needs.
    pub async fn run(
        &self,
        cases: &[TestCase],
        options: RunOptions,
        observe: Arc<dyn Fn(TestEvent) + Send + Sync>,
        exec: &Exec,
    ) -> Vec<TestOutcome> {
        let shared = Arc::new(SharedRun {
            base: self.base.clone(),
            scratch_root: self.scratch_root.clone(),
            options,
            permits: Permits::new(options.threads.max(1)),
            failures: AtomicUsize::new(0),
            observe,
            sentinel: self.contract.ok_sentinel.clone(),
            quiet_argument: self.contract.quiet_argument.clone(),
        });
        let jobs: Vec<_> = cases
            .iter()
            .enumerate()
            .map(|(index, case)| (index, case.clone(), Arc::clone(&shared)))
            .collect();
        exec.fan_out(jobs, |(index, case, shared)| async move {
            shared.run_one(index, &case)
        })
        .await
    }

    /// The base command with `extra` appended — the per-process arguments, which are the test
    /// ids and, when the run is not capturing, the harness's quiet flag.
    fn invocation_with(base: &Invocation, extra: &[String]) -> Invocation {
        let mut invocation = base.clone();
        invocation.args.extend_from_slice(extra);
        invocation
    }

    /// Spawn one invocation with its output redirected into `directory`, waiting at most
    /// `timeout`.
    fn execute(
        invocation: &Invocation,
        directory: &Path,
        timeout: Option<Duration>,
        capture: bool,
    ) -> std::io::Result<RawOutcome> {
        std::fs::create_dir_all(directory)?;
        let invocation = Self::spill(invocation, directory)?;
        let stdout = directory.join("stdout");
        let stderr = directory.join("stderr");
        let mut command = Command::new(&invocation.program);
        command
            .args(&invocation.args)
            .current_dir(&invocation.working_dir)
            .envs(&invocation.environment)
            // A test that reads stdin would otherwise block forever on a terminal it shares with
            // the progress display.
            .stdin(Stdio::null());
        if capture {
            command
                .stdout(File::create(&stdout)?)
                .stderr(File::create(&stderr)?);
        }
        let started = Instant::now();
        let mut child = command.spawn()?;
        let status = Self::wait(&mut child, timeout)?;
        Ok(RawOutcome {
            status,
            duration: started.elapsed(),
            stdout,
            stderr,
            captured: capture,
        })
    }

    /// Spill an over-long command line into this test's own argument file.
    fn spill(invocation: &Invocation, directory: &Path) -> std::io::Result<Invocation> {
        if !invocation.needs_argument_file() {
            return Ok(invocation.clone());
        }
        let Some(body) = invocation.argument_file() else {
            return Ok(invocation.clone());
        };
        let path = directory.join("java-args");
        std::fs::write(&path, body)?;
        Ok(invocation.clone().with_argument_file(&path))
    }

    /// Wait for `child`, killing it once `timeout` elapses.
    ///
    /// With a deadline, polls with a bounded backoff rather than blocking in `wait()`: the
    /// deadline has to be observable, and this runs on a fan-out worker thread where sleeping
    /// costs nothing the executor can feel. Without one — the default, since `--timeout` has no
    /// value unless asked for — it blocks, because there is nothing left to observe and a poll
    /// interval would be added to every test's reported wall time.
    fn wait(child: &mut Child, timeout: Option<Duration>) -> std::io::Result<Option<i32>> {
        const FIRST: Duration = Duration::from_millis(1);
        const LONGEST: Duration = Duration::from_millis(20);
        // No deadline to observe, so there is nothing to poll for: block until the JVM exits and
        // notice it the instant it does. Polling here would cost every test up to one backoff
        // interval of wall time it did not spend running.
        let Some(timeout) = timeout else {
            return Ok(Some(child.wait()?.code().unwrap_or(-1)));
        };
        let started = Instant::now();
        let mut backoff = FIRST;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(Some(status.code().unwrap_or(-1)));
            }
            if started.elapsed() >= timeout {
                // The child's own children are not reached: killing a process group needs a
                // platform API this crate does not take. A test that spawns is a test that
                // cleans up after itself.
                child.kill()?;
                child.wait()?;
                return Ok(None);
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(LONGEST);
        }
    }
}

/// What one spawn produced, before it is judged.
struct RawOutcome {
    /// The exit status, or `None` when the deadline killed the process.
    status: Option<i32>,
    duration: Duration,
    stdout: PathBuf,
    stderr: PathBuf,
    captured: bool,
}

/// Everything a fan-out job needs, shared by every job in one run.
struct SharedRun {
    base: Invocation,
    scratch_root: PathBuf,
    options: RunOptions,
    permits: Permits,
    failures: AtomicUsize,
    observe: Arc<dyn Fn(TestEvent) + Send + Sync>,
    sentinel: String,
    quiet_argument: String,
}

impl SharedRun {
    /// Run one test, honouring the concurrency limit, the retry count, and `--max-fail`.
    fn run_one(&self, index: usize, case: &TestCase) -> TestOutcome {
        if self.exhausted() {
            return Self::never_started(case);
        }
        let _permit = self.permits.acquire();
        // Checked again with the permit in hand: the run may have ended while this job waited.
        if self.exhausted() {
            return Self::never_started(case);
        }
        (self.observe)(TestEvent::Started(case.id().to_owned()));
        let mut outcome = self.attempt(index, case, 0);
        let mut attempt = 0;
        while outcome.verdict.is_failure() && attempt < self.options.retries {
            attempt += 1;
            outcome = self.attempt(index, case, attempt);
            outcome.attempts = attempt + 1;
        }
        if outcome.verdict.is_failure() {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
        (self.observe)(TestEvent::Finished(outcome.clone()));
        outcome
    }

    /// The outcome of a test `--max-fail` stopped before it ever ran.
    fn never_started(case: &TestCase) -> TestOutcome {
        TestOutcome {
            id: case.id().to_owned(),
            verdict: TestVerdict::Skipped,
            duration: Duration::ZERO,
            attempts: 0,
            stdout: None,
            stderr: None,
        }
    }

    /// Whether enough tests have failed that the run should stop starting more.
    fn exhausted(&self) -> bool {
        self.options
            .max_fail
            .is_some_and(|limit| self.failures.load(Ordering::Relaxed) >= limit)
    }

    /// One attempt at one test.
    fn attempt(&self, index: usize, case: &TestCase, attempt: u32) -> TestOutcome {
        let directory = self
            .scratch_root
            .join("run")
            .join(format!("{index}-{attempt}"));
        let mut args = Vec::new();
        // With capture off the sentinel would land in the terminal the tests are writing to, and
        // it is machinery rather than anything a test said. The verdict comes from the exit status
        // there, so suppressing it costs nothing.
        if !self.options.capture {
            args.push(self.quiet_argument.clone());
        }
        args.push(case.id().to_owned());
        let invocation = TestLauncher::invocation_with(&self.base, &args);
        let capture = self.options.capture;
        match TestLauncher::execute(&invocation, &directory, self.options.timeout, capture) {
            Ok(raw) => {
                let verdict = self.judge(&raw, case);
                TestOutcome {
                    id: case.id().to_owned(),
                    verdict,
                    duration: raw.duration,
                    attempts: 1,
                    stdout: raw.captured.then(|| raw.stdout.clone()),
                    stderr: raw.captured.then(|| raw.stderr.clone()),
                }
            }
            // A JVM that could not be started is the test's failure to report: the alternative is
            // aborting the whole run over one process, which loses every other result.
            Err(_) => TestOutcome {
                id: case.id().to_owned(),
                verdict: TestVerdict::Failed { code: None },
                duration: Duration::ZERO,
                attempts: 1,
                stdout: None,
                stderr: None,
            },
        }
    }

    /// Read the verdict out of what the process did.
    ///
    /// With capture on this is the sentinel and nothing else. With capture off there is no output
    /// to read — it went to the terminal — so the exit status has to stand in, and a test that
    /// exits successfully on its own is indistinguishable from one that passed.
    fn judge(&self, raw: &RawOutcome, case: &TestCase) -> TestVerdict {
        let Some(code) = raw.status else {
            return TestVerdict::TimedOut;
        };
        if !raw.captured {
            return if code == 0 {
                TestVerdict::Passed
            } else {
                TestVerdict::Failed { code: Some(code) }
            };
        }
        let sentinel = format!("{}\t{}", self.sentinel, case.id());
        // Read as bytes, not as a `String`: a test is free to put arbitrary bytes on its own
        // standard output (`System.out.write`, a platform charset that is not UTF-8), and
        // `read_to_string` fails on the *whole file* for one of them — which would report a test
        // that passed as failed. The sentinel is ASCII, so the search never needs the decode.
        //
        // Matched as a line *suffix* rather than as a whole line: the harness prints it with
        // `println`, so a body whose last write was a `System.out.print` with no trailing newline
        // leaves the sentinel sharing that line. The id is on the line either way.
        let passed = std::fs::read(&raw.stdout).is_ok_and(|bytes| {
            bytes.split(|byte| *byte == b'\n').any(|line| {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                line.ends_with(sentinel.as_bytes())
            })
        });
        if passed {
            TestVerdict::Passed
        } else {
            TestVerdict::Failed { code: Some(code) }
        }
    }
}

/// A counting semaphore over the fan-out's worker threads.
///
/// `-j` can only ever *narrow* what the fan-out already provides: its worker pool is sized to the
/// machine's parallelism and is not resizable, so a larger `-j` has nothing to widen.
struct Permits {
    free: Mutex<usize>,
    released: Condvar,
}

impl Permits {
    const fn new(count: usize) -> Self {
        Self {
            free: Mutex::new(count),
            released: Condvar::new(),
        }
    }

    /// Take a permit, waiting for one when none is free.
    fn acquire(&self) -> Permit<'_> {
        // A poisoned mutex is a job that panicked while holding a permit. The count it guards is
        // an integer, so it cannot be left half-written, and refusing to run the rest of the
        // suite over one panicked test would lose every result still to come.
        {
            let mut free = self.free.lock().unwrap_or_else(PoisonError::into_inner);
            while *free == 0 {
                free = self
                    .released
                    .wait(free)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            *free -= 1;
        }
        Permit { permits: self }
    }
}

/// One held permit, returned on drop.
struct Permit<'a> {
    permits: &'a Permits,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut free = self
            .permits
            .free
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *free += 1;
        drop(free);
        self.permits.released.notify_one();
    }
}
