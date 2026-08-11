//! The JVM rung: hand every emitted class file to a real bytecode verifier.
//!
//! Nothing upstream of this can tell a well-formed class file from a plausible one. The assembler
//! computes its own `max_stack`, `max_locals` and `StackMapTable`, and `jals-classfile` reads back
//! whatever those say — so a frame that describes the wrong type round-trips perfectly and is
//! still a class no JVM will load. Only the verifier has an opinion, and it is the authority.
//!
//! # Why the driver initializes the classes
//!
//! Verification is part of *linking*, and a JVM is free to defer linking until first use
//! (JVMS §5.4), so asking for less than initialization checks nothing. This harness first tried
//! `ClassLoader.resolveClass`, whose javadoc says it links the class, and it passed class files
//! that the very same driver rejects once it initializes them — the three known-bad cases in the
//! sample all scored clean. [`Verify.java`](../../scripts/verify/Verify.java) therefore calls
//! `Class.forName(name, true, loader)`, which is what actually runs the verifier.
//!
//! That means running the corpus's static initializers, which in a corpus of *compiler tests* is
//! arbitrary code, and the driver's timeout and shutdown-hook flush are there for that reason
//! rather than as hygiene.
//!
//! The loader's parent is the **platform** loader: the JDK's own modules are visible, since the
//! verifier resolves the types an instruction names against them, but the harness's class path is
//! not — a generated class must never be shadowed by one that happens to be loaded already, which
//! would score a class file as accepted without its bytes ever being read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use jals_javac::lower::CompiledClass;

use super::{CaseResult, Outcome};

/// Stages emitted class files and links them on a real JVM.
pub struct Verifier {
    /// The corpus root, so a staged case is keyed by the same relative path a [`CaseResult`] uses.
    root: PathBuf,
    /// Scratch directory holding one subdirectory per case.
    staging: tempfile::TempDir,
    /// Relative case path → (its staging subdirectory, the binary names it emitted).
    staged: Mutex<BTreeMap<PathBuf, (PathBuf, Vec<String>)>>,
}

/// What the JVM said about one class.
enum Verdict {
    /// The verifier accepted it.
    Ok,
    /// The verifier rejected it: the class file is wrong.
    Bad(String),
    /// Linking failed for a reason that is not about this class file's shape.
    Error(String),
}

impl Verifier {
    /// A verifier staging into a fresh scratch directory.
    ///
    /// # Errors
    /// If the scratch directory cannot be created.
    pub fn new(root: &Path) -> Result<Self, String> {
        Ok(Self {
            root: root.to_path_buf(),
            staging: tempfile::tempdir().map_err(|e| format!("scratch directory: {e}"))?,
            staged: Mutex::new(BTreeMap::new()),
        })
    }

    /// Whether a JVM able to run the driver is on this host.
    ///
    /// Says so out loud when it is not: the JVM is the only authority on whether an emitted class
    /// file is correct, and a quiet stand-down would let a run of nothing read as a clean sheet.
    pub fn jvm_available() -> bool {
        let present = Command::new("java")
            .arg("-version")
            .output()
            .is_ok_and(|output| output.status.success());
        if !present {
            eprintln!(
                "warning: no `java` on this host — the corpus will be compiled but never verified,\n\
                 warning: which is the only rung that proves a class file is right."
            );
        }
        present
    }

    /// Write one case's class files into the staging area, ready to be linked.
    ///
    /// Called from the parallel compile pass, so it takes `&self`.
    pub(super) fn stage(&self, source: &Path, classes: &[CompiledClass]) {
        let rel = source
            .strip_prefix(&self.root)
            .unwrap_or(source)
            .to_path_buf();
        let mut staged = self.staged.lock().expect("staging map");
        let directory = self.staging.path().join(staged.len().to_string());
        let mut names = Vec::with_capacity(classes.len());
        for class in classes {
            let path = directory.join(format!("{}.class", class.internal_name));
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, &class.bytes));
            if written.is_err() {
                // A case that could not be staged is left unverified rather than dropped: the
                // report's `unverified` column is where a host problem belongs, not the pass rate.
                return;
            }
            names.push(class.internal_name.replace('/', "."));
        }
        staged.insert(rel, (directory, names));
    }

    /// Link every staged case on one JVM and fold the verdicts into `results`.
    ///
    /// One JVM for the whole corpus: starting one per case would dominate the run, and the driver
    /// gives each case a class loader of its own anyway, which is what actually isolates them.
    pub(super) fn link_all(&self, results: &mut [CaseResult]) {
        let staged = self.staged.lock().expect("staging map");
        if staged.is_empty() {
            return;
        }
        let list = self.staging.path().join("cases.tsv");
        let mut text = String::new();
        for (rel, (directory, names)) in staged.iter() {
            text.push_str(&format!(
                "{}\t{}\t{}\n",
                directory.display(),
                rel.display(),
                names.join(",")
            ));
        }
        if std::fs::write(&list, text).is_err() {
            return;
        }
        let Some(stdout) = Self::run_driver(&list) else {
            return;
        };

        let verdicts = Self::parse(&stdout);
        for result in results.iter_mut() {
            // Only a staged case can have a verdict; everything else is at a lower rung already.
            if !matches!(result.outcome, Outcome::Unverified) {
                continue;
            }
            // No line for a staged case means the JVM never reached it — a driver crash, not a
            // judgment on these bytes, so the case stays unverified.
            let Some(verdict) = verdicts.get(&result.rel) else {
                continue;
            };
            result.outcome = match verdict {
                Verdict::Ok => Outcome::Verified,
                Verdict::Bad(message) => Outcome::Rejected(message.clone()),
                Verdict::Error(message) => Outcome::Unlinkable(message.clone()),
            };
        }
    }

    /// Run the verifier driver over a case list and return its stdout.
    ///
    /// A driver that could not be started leaves every case unverified rather than failing the run:
    /// the JVM rung is a measurement, and a host that cannot supply one has said nothing about the
    /// bytes. A driver that ran and exited non-zero still had its verdicts read — it prints them as
    /// it goes, so a crash partway through costs the cases after it and no others.
    fn run_driver(list: &Path) -> Option<String> {
        let driver = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("verify")
            .join("Verify.java");
        // Source-file mode (JEP 330): the driver is compiled in memory on each run, so nothing
        // has to build or vendor a jar for it.
        let output = Command::new("java")
            .arg("-XX:-UsePerfData")
            .arg(&driver)
            .arg(list)
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                eprintln!("warning: could not run the verifier driver: {error}");
                return None;
            }
        };
        if !output.status.success() {
            eprintln!(
                "warning: the verifier driver exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// One verdict per case, worst-wins: a rejected class condemns the case even when its siblings
    /// linked, because the case is one compilation and it produced a class no JVM will load.
    fn parse(stdout: &str) -> BTreeMap<PathBuf, Verdict> {
        let mut verdicts: BTreeMap<PathBuf, Verdict> = BTreeMap::new();
        for line in stdout.lines() {
            let mut fields = line.splitn(4, '\t');
            let (Some(kind), Some(rel)) = (fields.next(), fields.next()) else {
                continue;
            };
            let _class = fields.next();
            let message = fields.next().unwrap_or_default().to_owned();
            let verdict = match kind {
                "OK" => Verdict::Ok,
                "BAD" => Verdict::Bad(message),
                "ERR" => Verdict::Error(message),
                _ => continue,
            };
            let key = PathBuf::from(rel);
            match (verdicts.get(&key), &verdict) {
                // A worse verdict replaces a better one; a better one never replaces a worse.
                (Some(Verdict::Bad(_)), _) | (Some(Verdict::Error(_)), Verdict::Ok) => {}
                _ => {
                    verdicts.insert(key, verdict);
                }
            }
        }
        verdicts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_class_condemns_its_case() {
        let verdicts = Verifier::parse(
            "OK\tT.java\tT\n\
             BAD\tT.java\tT$1\tVerifyError: Bad type on operand stack\n\
             OK\tT.java\tT$2\n",
        );
        let verdict = verdicts.get(Path::new("T.java")).expect("a verdict");
        assert!(
            matches!(verdict, Verdict::Bad(message) if message.contains("VerifyError")),
            "one bad class makes the case bad — it is one compilation"
        );
    }

    #[test]
    fn an_unlinkable_class_does_not_mask_a_rejected_one() {
        let verdicts = Verifier::parse(
            "BAD\tT.java\tT\tClassFormatError: duplicate method\n\
             ERR\tT.java\tT$1\tNoClassDefFoundError: javax/tools/JavaFileManager\n",
        );
        assert!(matches!(
            verdicts.get(Path::new("T.java")),
            Some(Verdict::Bad(_))
        ));
    }

    #[test]
    fn a_case_with_only_ok_lines_is_verified() {
        let verdicts = Verifier::parse("OK\tA.java\tA\nOK\tA.java\tA$Inner\n");
        assert!(matches!(
            verdicts.get(Path::new("A.java")),
            Some(Verdict::Ok)
        ));
    }
}
