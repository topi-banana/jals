//! The platform package's source list, generated from its `java/` tree.
//!
//! `jals-platform/src/sources.rs` is one `java_package!` invocation naming every `.java` the
//! platform publishes, split into the two tiers. Written out rather than globbed, and generated
//! rather than hand-maintained, so both halves of that are true at once: the macro sees literal
//! paths it can `include_str!`, and a file nobody listed is a **build failure** rather than a file
//! that silently is not part of the package.
//!
//! Moving a file between the tiers is a diff in this generated output, which is the point: it is
//! the event that says somebody implemented a type, and it should be visible in review.
//!
//! The `jals/` tree beside `java/` is deliberately not walked. `jals.io` is a package of its own
//! (`src/jals_io.rs`), not a unit of `java.base`, and a generator that swept it in would put one
//! class into the platform's `include_str!` list.
//!
//! # Which tier a file is in
//!
//! Decided here, by path, and stated in one place — [`Platform::SIGNATURE_ONLY`].
//!
//! Not by inspecting the Java. "Has a body" is not the question: an interface's methods have none
//! and interfaces are compiled, while `java.lang.Object` could be given bodies and must still never
//! be. The question is whether the *backend* can represent the type at all, which is a fact about
//! the backend and belongs in a list somebody has to edit deliberately.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

/// Where the platform's Java lives, relative to the project root.
///
/// Paths are generated relative to this directory — `java/lang/String.java`, `jals/io/Out.java` —
/// which is what `root: "../java"` in the generated invocation resolves against. The `jals/` tree
/// is walked and then filtered out: `jals.io` is a package of its own (`src/jals_io.rs`), and a
/// generator that swept it in would put one class into `java.base`'s `include_str!` list.
const JAVA_ROOT: &str = "jals-platform/java";

/// The generated file, relative to the project root.
const TARGET: &str = "jals-platform/src/sources.rs";

/// The platform package's generated source list.
pub(crate) struct Platform;

impl Platform {
    /// The units the platform declares but never compiles, by path prefix or exact path.
    ///
    /// Each is a property of the wasm backend, or of the host a module does not have, rather than
    /// of the Java:
    ///
    /// - `java/lang/Object.java` **is** the backend's own `anyref`, answered for before it consults
    ///   its struct table. A declared `Object` with fields would be one question with two answers —
    ///   a field present on some instances and not others — so it is declared and never lowered.
    /// - `java/util/`'s **containers nobody has implemented** — `Map`, `Set`, `Optional`,
    ///   `Objects` and the two hash classes — are declarations a program can still *name*, which is
    ///   what an editor needs, and not types a module can call. The list family beside them is
    ///   implemented: `Iterable`, `Collection`, `List`, `Iterator` and `ArrayList` are lowered,
    ///   because `ArrayList`'s `Vec` is a native class and an interface it implements has to exist
    ///   as a type in the module.
    /// - `java/lang/Enum.java` and `java/lang/Record.java` are the two implicit supertypes whose
    ///   members the *compiler* synthesises per declaration — a constant's `ordinal()`, a record's
    ///   accessors. There is no single body either could carry that would produce them, so they
    ///   are declared for analysis and never lowered.
    /// - `java/lang/Thread.java`, `java/lang/Runtime.java` and `java/lang/Process.java` describe a
    ///   host a module does not have — a second thread, a process to start or halt. They are the
    ///   record a `javac` build's analysis resolves those names through, and no body here could do
    ///   what they promise.
    /// - `java/lang/StringBuffer.java` would be `StringBuilder` under a second name, since a module
    ///   has one thread and nothing for its synchronization to guard, and `java/lang/Cloneable.java`
    ///   declares no member at all.
    const SIGNATURE_ONLY: &'static [&'static str] = &[
        "java/lang/Cloneable.java",
        "java/lang/Enum.java",
        "java/lang/Object.java",
        "java/lang/Process.java",
        "java/lang/Record.java",
        "java/lang/Runtime.java",
        "java/lang/StringBuffer.java",
        "java/lang/Thread.java",
        "java/util/HashMap.java",
        "java/util/HashSet.java",
        "java/util/Map.java",
        "java/util/Objects.java",
        "java/util/Optional.java",
        "java/util/Set.java",
    ];

    /// Render the list and write it; with `check`, render to memory and fail if the committed file
    /// differs.
    pub(crate) fn run(root: &Path, check: bool) -> Result<()> {
        let code = Self::generate(root)?;
        let target = root.join(TARGET);
        if check {
            // A missing file is "stale"; any other read failure is a real error and must not
            // masquerade as the out-of-date message.
            let committed = match fs::read_to_string(&target) {
                Ok(text) => text,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("failed to read {}", target.display()));
                }
            };
            ensure!(
                committed == code,
                "{} is out of date; run `cargo run -p xtask -- codegen`",
                target.display()
            );
        } else {
            fs::write(&target, code)
                .with_context(|| format!("failed to write {}", target.display()))?;
        }
        Ok(())
    }

    /// Every `.java` under the platform's tree, as package-relative paths, sorted.
    ///
    /// Sorted, and that is load-bearing rather than tidy: the order here is the order the units are
    /// indexed and lowered in, so a directory listing's order would make the output depend on the
    /// filesystem that produced it.
    fn sources(root: &Path) -> Result<Vec<String>> {
        let java = root.join(JAVA_ROOT);
        let mut found = Vec::new();
        Self::walk(&java, &java, &mut found)?;
        // `jals.io` is its own package, not a unit of `java.base`; see `JAVA_ROOT`.
        found.retain(|path| path.starts_with("java/"));
        ensure!(
            !found.is_empty(),
            "{} holds no `.java` at all",
            java.display()
        );
        found.sort();
        Ok(found)
    }

    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        let entries =
            fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?;
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in entries {
            paths.push(
                entry
                    .with_context(|| format!("failed to read {}", dir.display()))?
                    .path(),
            );
        }
        paths.sort();
        for path in paths {
            if path.is_dir() {
                Self::walk(base, &path, out)?;
            } else if path.extension().is_some_and(|ext| ext == "java") {
                let relative = path
                    .strip_prefix(base)
                    .with_context(|| format!("{} is outside {}", path.display(), base.display()))?;
                // Forward slashes whatever the host writes, because the output is a Rust string
                // literal `include_str!` reads and a Java package path both — and a backslash in
                // either is wrong on every platform including the one that produced it.
                out.push(
                    relative
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
        }
        Ok(())
    }

    /// Whether `path` is declared and never compiled.
    fn is_signature_only(path: &str) -> bool {
        Self::SIGNATURE_ONLY
            .iter()
            .any(|entry| path == *entry || path.starts_with(entry))
    }

    fn generate(root: &Path) -> Result<String> {
        let sources = Self::sources(root)?;
        let (signatures, implementation): (Vec<&String>, Vec<&String>) = sources
            .iter()
            .partition(|path| Self::is_signature_only(path));
        ensure!(
            !implementation.is_empty(),
            "every platform source is signature-only, which would compile to an empty module"
        );
        // An entry that matches nothing is a rename nobody followed, and it is silent: the file it
        // used to name falls into `implementation` and the generated list looks fine. A signature
        // unit reaching the compile is `java/lang/Object.java` becoming a struct, or a class of
        // bodiless methods reaching the lowering — so the stale entry is refused here, where the
        // rename is still in the diff.
        for entry in Self::SIGNATURE_ONLY {
            ensure!(
                sources
                    .iter()
                    .any(|path| path == entry || path.starts_with(entry)),
                "`{entry}` is listed as signature-only and names no source under `java/`"
            );
        }

        let mut out = String::from(HEADER);
        for path in signatures {
            out.push_str(&format!("            {path:?},\n"));
        }
        out.push_str(MIDDLE);
        for path in implementation {
            out.push_str(&format!("            {path:?},\n"));
        }
        out.push_str(FOOTER);
        Ok(out)
    }
}

const HEADER: &str = r#"//! The source list, generated from the `java/` tree.
//!
//! `cargo run -p xtask -- codegen` writes this file, and CI runs the same command with `--check`.
//! That is what makes a `.java` file nobody listed a build failure rather than a file that silently
//! is not part of the package — and it makes moving one between the two tiers a visible diff that
//! says somebody implemented it.
//!
//! Which tier a file is in is decided by `xtask`'s own list, not by looking at the Java: "has a
//! body" is the wrong question, because an interface's methods have none and interfaces *are*
//! compiled, while `java.lang.Object` could be given bodies and must still never be lowered.

use jals_native::java_package;

use crate::bindings::Bindings;
use crate::host::PlatformHost;

java_package! {
    /// The Java platform library `jals` ships: `java.lang`, `java.util` and `java.io`, behind the
    /// host functions the operations Java cannot express bind to.
    ///
    /// See the crate docs for what is here and what deliberately is not.
    pub JavaBase {
        name: "java.base",
        version: 1,
        host: dyn PlatformHost,
        root: "../java",
        signatures: [
"#;

const MIDDLE: &str = r"        ],
        implementation: [
";

const FOOTER: &str = r#"        ],
        bind: Bindings::install,
    }
}

impl JavaBase {
    /// Every unit paired with the fidelity a build that does or does not link it reads it at.
    ///
    /// The tier rule, in one place, as a value: an implementation unit is the running code only
    /// where it is compiled into the artifact, and a signature unit is a record wherever it is.
    /// `links` is `jals_config::Manifest::links_packages`.
    ///
    /// A convenience over [`SOURCES`](Self::SOURCES), for a consumer that wants the platform and
    /// nothing else — every test in this workspace, and a host with no manifest to resolve from.
    /// A host that has one goes through `PackageSelection`, which answers the same way for the
    /// same reason.
    #[must_use]
    pub fn tiers(links: bool) -> alloc::vec::Vec<(&'static str, bool)> {
        Self::SOURCES
            .iter()
            .map(|source| {
                let running =
                    links && matches!(source.kind, jals_native::SourceKind::Implementation);
                (source.text.as_ref(), running)
            })
            .collect()
    }
}
"#;
