//! Compiling the in-browser workspace: frontend seam in, downloadable artifact out.
//!
//! The same pipeline `jals build` runs, minus the filesystem, and reached through the same two
//! selections it uses. Sources go through whichever frontend
//! [`jals_frontend::FrontendSelection::for_manifest`] hands back, so the backend only ever sees what
//! a frontend emitted, then through whichever backend
//! [`jals_build::BackendSelection::in_process`] hands back — the in-process compiler, portable by
//! construction, which is why a browser tab can run it at all. That second entry point is also how
//! `javac` is *declined* rather than attempted: choosing it says this host has no process to spawn,
//! so the answer comes back as an absence carrying its reason. Class files are packaged into a jar
//! here; a WebAssembly module is already one artifact and passes straight through.
//!
//! Deliberately free of the workspace lock, Monaco, and the DOM: sources arrive as `(path, text)`
//! and the result is bytes. That keeps the whole thing testable on the host and makes it impossible
//! for the tested path to reach a browser API.
//!
//! What this does *not* do: feed the resolved `[dependencies]` classpath to the compiler. Library
//! signatures come from `jals-hir`'s embedded stubs, so a downloaded jar is on the *editor's*
//! classpath but not the compiler's — the same limitation `jals build` has today.

use std::fmt;

use jals_build::{
    BackendAbsence, BackendOptions, BackendRequest, BackendSelection, BackendSource, RunTarget,
    WasmRunOutcome, WasmRunRequest, WasmRunner,
};
use jals_classpath::JarPackage;
use jals_config::{BackendKind, Manifest};
use jals_frontend::{FrontendSelection, IrFile};
use jals_storage::{ArtifactCache, MemoryCache, RelativePath};

/// The name a project with no usable `[package] name` is packaged under.
const FALLBACK_JAR: &str = "project.jar";

/// The whole-project WebAssembly module's name.
///
/// The backend's own constant rather than a literal that matches it: the same name is what `jals
/// build` writes to disk and what `jals run` reads back, and three copies of it is two chances to
/// disagree.
const WASM_ARTIFACT: &str = jals_build::JalsBackend::WASM_MODULE;

/// What one compile produced: a downloadable file plus a line describing it.
pub struct CompileArtifact {
    /// The download file name — `{package.name}.jar` or `project.wasm`.
    pub name: String,
    pub bytes: Vec<u8>,
    /// One human-readable line about what was produced, shown in the Build output pane.
    pub summary: String,
    /// Whether this host can execute what it just produced, which decides whether the pane offers
    /// to. A module can be: the engine is compiled into this binary. A jar cannot — running one
    /// needs a JVM, and a browser tab has no process to start one in.
    ///
    /// A field rather than a test on [`name`](Self::name), so that what decides it is the backend
    /// that produced the artifact and not a string comparison a second reader could get wrong.
    pub runnable: bool,
}

/// Why a compile produced no artifact. The [`Display`](fmt::Display) is what the pane shows.
#[derive(Debug)]
pub enum CompileFailure {
    /// The selected `[build] backend` does not exist on this host — `javac` in a browser tab, which
    /// has no process to spawn. Carries the selection's own verdict rather than restating it.
    BackendUnavailable {
        /// The `[build] backend` tag that was selected.
        id: &'static str,
        /// Why this host does not have it.
        reason: BackendAbsence,
    },
    /// A workspace path that is not a portable project-relative path.
    InvalidPath(String),
    /// The frontend rejected its input or could not publish its output.
    Lower(String),
    /// The backend could not be driven at all (as opposed to compiling and reporting).
    Backend(String),
    /// The compiler ran and reported source it cannot compile yet. Not "your code is wrong":
    /// `jals-javac` reports every construct it has no lowering for rather than mis-emitting it.
    NotCompiled(Vec<String>),
    /// The class files compiled but could not be packaged into a jar.
    Package(String),
    /// There is nothing to compile.
    NoSources,
    /// The manifest declares `[build] remap`, which this host cannot run.
    ///
    /// A refusal rather than a jar built without it. This host releases its workspace lock before
    /// compiling — deliberately, because a compile is the longest thing it does — so the storage a
    /// remap resolves its mapping set through is not held here, and its in-process backend compiles
    /// against embedded stubs rather than a classpath, which is where the class hierarchy a
    /// reobfuscation needs would come from. Handing back an unremapped jar under the name the
    /// manifest asked for would be indistinguishable from success.
    ///
    /// The refusal covers a declared step whose mapping set no selection activates, too. Elsewhere
    /// that case packages a jar and rewrites nothing, but what blocks this host is the storage and
    /// the fetcher rather than the mapping — and `[build] remap`'s `jar` is a project path, which
    /// is not a name a browser download has.
    RemapUnsupported,
}

impl fmt::Display for CompileFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The absence is the selection's own wording; the rest points at the two backends that
            // *do* run here, since the manifest is one edit away.
            Self::RemapUnsupported => f.write_str(
                "`[build] remap` is declared, but the browser build cannot reobfuscate: it holds \
                 no project storage while compiling and compiles against embedded stubs rather \
                 than a classpath",
            ),
            Self::BackendUnavailable { id, reason } => write!(
                f,
                "{id} needs a host process, and {reason}.\n\
                 Set `[build] backend = {{ type = \"jals\" }}` in jals.toml for a downloadable \
                 .jar, or `{{ type = \"jals-wasm\" }}` for a WebAssembly module."
            ),
            Self::InvalidPath(path) => write!(f, "`{path}` is not a project-relative path"),
            Self::Lower(message) => write!(f, "the frontend failed: {message}"),
            Self::Backend(message) => write!(f, "the compiler could not run: {message}"),
            Self::NotCompiled(messages) => {
                write!(f, "{} construct(s) not compiled yet:", messages.len())?;
                for message in messages {
                    write!(f, "\n  {message}")?;
                }
                Ok(())
            }
            Self::Package(message) => write!(f, "packaging the jar failed: {message}"),
            Self::NoSources => f.write_str("there is nothing to compile"),
        }
    }
}

/// Running the module a `jals-wasm` compile produced, in the tab that compiled it.
///
/// The browser cannot spawn `java`, which is why the *Build* button offers a `.jar` as a download
/// rather than running it. A WebAssembly module is the case where that changes: `jals-build`'s
/// engine is an interpreter over `core + alloc` with no host in it, so it compiles to `wasm32` like
/// everything else here and the module runs where it was built. What `jals run --invoke` reaches
/// and what this reaches are the same code.
///
/// Beside [`Compile`] and free of the DOM for the same reason it is: a compile is `(path, text)` in
/// and bytes out, and a run is bytes in and a line out — both host-testable, neither able to touch
/// a browser API.
pub struct Execute;

impl Execute {
    /// Instantiate `module`, and call an export when `command` names one.
    ///
    /// `command` is what the user typed: an export name followed by its arguments, or nothing at
    /// all. Split here rather than in the view, so what the pane holds is a string and what decides
    /// its meaning is testable without one.
    ///
    /// An empty command is still a run — instantiating executes the module's start function, which
    /// is where the backend lowers every `static` initialiser the project declares.
    ///
    /// The answer is a line rather than a value because that is what the caller does with it: the
    /// Build output pane is text, and nothing downstream computes with a returned `i32`.
    pub fn run(module: &[u8], command: &str) -> Result<String, String> {
        let mut parts = command.split_whitespace();
        let invoke = parts.next();
        let args: Vec<String> = parts.map(ToOwned::to_owned).collect();
        let request = WasmRunRequest {
            module,
            invoke,
            args: &args,
            progress: &jals_progress::Progress::SILENT,
        };
        let outcome = WasmRunner::run(&request).map_err(|error| error.to_string())?;
        Ok(match outcome {
            WasmRunOutcome::Instantiated => format!(
                "instantiated {WASM_ARTIFACT}: its static initialisers ran. Name an exported \
                 `static` method to call one."
            ),
            // Only an export can return, so `invoke` is the name here; the fallback keeps the arm
            // total rather than asserting that, since a wrong line beats a panicking pane.
            WasmRunOutcome::Returned(values) if values.is_empty() => {
                format!(
                    "`{}` returned (it is void)",
                    invoke.unwrap_or(WASM_ARTIFACT)
                )
            }
            WasmRunOutcome::Returned(values) => format!(
                "`{}` returned {}",
                invoke.unwrap_or(WASM_ARTIFACT),
                values
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        })
    }
}

/// Namespace for compiling the in-browser workspace.
pub struct Compile;

impl Compile {
    /// Compile `files` with the backend `manifest` selects, in this process.
    ///
    /// `files` are `(project-relative path, text)` — every indexed Java file, generated sources
    /// included, exactly as the editor sees them.
    pub async fn workspace(
        manifest: &Manifest,
        files: &[(String, String)],
    ) -> Result<CompileArtifact, CompileFailure> {
        // Decided before any work, and by the selection rather than here: `javac` is not a "compile
        // then fail" case, it is a backend this host cannot have. `in_process` is the entry point
        // for exactly that — choosing it declares there is no process to spawn.
        let backend =
            match BackendSelection::in_process(manifest.build.backend, manifest.build.release) {
                BackendSelection::Available(backend) => backend,
                BackendSelection::Absent { id, reason } => {
                    return Err(CompileFailure::BackendUnavailable { id, reason });
                }
            };
        // Asked before any work, like the backend above: a refusal the manifest already implies
        // should not arrive after the slowest step in the host has run.
        if manifest.build.remap.is_some() {
            return Err(CompileFailure::RemapUnsupported);
        }
        if files.is_empty() {
            return Err(CompileFailure::NoSources);
        }

        let sources = Self::lower(manifest, files).await?;
        let options = BackendOptions::from_manifest(manifest);
        let request = BackendRequest {
            progress: &jals_progress::Progress::SILENT,
            tree: &sources,
            // The in-process compiler reads library signatures from the embedded stubs rather than
            // from the classpath, so resolved dependency jars do not participate — the same `&[]`
            // `jals-cli` passes.
            classpath: &[],
            options: &options,
        };
        let outcome = backend
            .compile(&request)
            .await
            .map_err(|error| CompileFailure::Backend(error.to_string()))?;
        if !outcome.success() {
            return Err(CompileFailure::NotCompiled(outcome.messages));
        }

        // How the output is *packaged* is still this host's question: one module for the whole
        // project passes straight through, one class file per type goes into a jar.
        if matches!(manifest.build.backend, BackendKind::JalsWasm {}) {
            let (_, bytes) = outcome.artifacts.into_iter().next().ok_or_else(|| {
                CompileFailure::Backend("the wasm backend emitted no module".to_owned())
            })?;
            return Ok(CompileArtifact {
                name: WASM_ARTIFACT.to_owned(),
                summary: format!(
                    "compiled {} source(s) into {WASM_ARTIFACT} ({} bytes)",
                    sources.len(),
                    bytes.len()
                ),
                bytes,
                runnable: true,
            });
        }

        // A project that declares no entry point still packages — as a library jar. Refusing here
        // would make `[run] main-class` a compile-time requirement it has never been.
        let main_class = RunTarget::resolve(manifest, None).ok();
        let name = Self::jar_file_name(manifest);
        let bytes =
            JarPackage::write(&outcome.artifacts, main_class).map_err(CompileFailure::Package)?;
        let summary = match main_class {
            Some(main_class) => format!(
                "packaged {} class file(s) into {name} (Main-Class: {main_class})",
                outcome.artifacts.len()
            ),
            None => format!(
                "packaged {} class file(s) into {name} (a library jar: no `[run] main-class`)",
                outcome.artifacts.len()
            ),
        };
        Ok(CompileArtifact {
            name,
            bytes,
            summary,
            // A jar is the download and nothing else here: the class files in it are for a JVM,
            // and this host cannot start one.
            runnable: false,
        })
    }

    /// Run the project's frontend over `files` and resolve its output back to the bytes a backend
    /// compiles.
    ///
    /// The published keys are looked up again rather than the input bytes reused: `BackendSource`
    /// carries the frontend's `CacheKey` as provenance, and reading the content back through it is
    /// what makes "the backend only ever sees frontend output" structural instead of assumed.
    async fn lower(
        manifest: &Manifest,
        files: &[(String, String)],
    ) -> Result<Vec<BackendSource>, CompileFailure> {
        let mut ir = Vec::with_capacity(files.len());
        for (path, text) in files {
            let relative =
                RelativePath::parse(path).map_err(|_| CompileFailure::InvalidPath(path.clone()))?;
            ir.push(IrFile::new(relative, text.as_bytes().to_vec().into()));
        }
        // The `[build.frontend]` decision — and the dialect features that override it — belongs to
        // `jals-frontend`, which is why this reads like the CLI's call rather than mirroring its
        // body. No command line in a browser, so `#[cfg(feature = "…")]` sees the manifest's own
        // `default` list: the same selection the Rhai build script ran under. A malformed
        // `[features]` table degrades to the empty set here rather than failing the compile — the
        // manifest editor is live, and the build script already ran under the same fallback.
        let build_features = manifest
            .resolve_build_features(&[], false, false)
            .unwrap_or_default()
            .into_features();
        let frontend = FrontendSelection::for_manifest(manifest, &build_features);

        // A throwaway cache: lowering reads its inputs from `ir`, never from a `ProjectView`, and
        // nothing memoizes backend output yet — so publishing into the workspace's own artifacts
        // would grow it every compile for no reuse.
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let lowered = frontend
            .lower(&mut cache, ir)
            .await
            .map_err(|error| CompileFailure::Lower(error.to_string()))?;

        let mut sources = Vec::with_capacity(lowered.tree.files().len());
        for file in lowered.tree.files() {
            let bytes = cache
                .lookup(&file.key)
                .await
                .map_err(|error| CompileFailure::Lower(error.to_string()))?
                .ok_or_else(|| {
                    CompileFailure::Lower(format!(
                        "lowered source `{}` was not published",
                        file.path
                    ))
                })?;
            sources.push(BackendSource {
                path: file.path.clone(),
                key: file.key.clone(),
                bytes,
            });
        }
        Ok(sources)
    }

    /// The jar's download name, from `[package] name`.
    ///
    /// The value becomes an `<a download>` attribute, so a name carrying a separator or a leading dot
    /// falls back rather than being sanitized into something the user did not write.
    fn jar_file_name(manifest: &Manifest) -> String {
        manifest
            .package
            .name
            .as_deref()
            .filter(|name| {
                !name.is_empty()
                    && !name.starts_with('.')
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            })
            .map_or_else(|| FALLBACK_JAR.to_owned(), |name| format!("{name}.jar"))
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;

    use super::*;
    use crate::workspace::SAMPLE_FILES;

    fn manifest(text: &str) -> Manifest {
        text.parse::<Manifest>().expect("test manifest parses")
    }

    /// Two files in the language subset both backends lower: primitives, `static` methods, a
    /// `while` loop, and a cross-file call.
    fn subset_sources() -> Vec<(String, String)> {
        vec![
            (
                "com/example/Greeter.java".to_owned(),
                "package com.example;\n\
                 public class Greeter {\n\
                 public static int twice(int n) { return n + n; }\n\
                 }\n"
                .to_owned(),
            ),
            (
                "com/example/Main.java".to_owned(),
                "package com.example;\n\
                 public class Main {\n\
                 public static int run() {\n\
                 int total = 0;\n\
                 int i = 0;\n\
                 while (i < 3) { total = total + Greeter.twice(i); i = i + 1; }\n\
                 return total;\n\
                 }\n\
                 }\n"
                .to_owned(),
            ),
        ]
    }

    /// The seed Rhai script generates a `public static final String MESSAGE = …` class, and every
    /// compile sees it — the build script runs on page load and its output joins the index. A
    /// `String`-typed `static` field must therefore be inert on *both* backends, or flipping
    /// `[build] backend` to `jals-wasm` would fail on a file the user never wrote.
    #[test]
    fn a_generated_static_string_field_does_not_block_either_backend() {
        let generated = (
            "target/jals/build/rhai/out/com/example/BuildInfo.java".to_owned(),
            "package com.example;\n\
             public final class BuildInfo {\n\
             public static final String MESSAGE = \"Generated in the browser\";\n\
             }\n"
            .to_owned(),
        );
        for backend in ["jals", "jals-wasm"] {
            let manifest = manifest(&format!("[build]\nbackend = {{ type = \"{backend}\" }}\n"));
            let mut files = subset_sources();
            files.push(generated.clone());
            files.sort();
            assert!(
                block_on_inline(Compile::workspace(&manifest, &files)).is_ok(),
                "`{backend}` must compile alongside the generated class"
            );
        }
    }

    /// The default backend is `javac`, which needs a process this host cannot spawn. The message
    /// has to name the way out, not just the wall.
    ///
    /// Pinned in full: the wording is the whole value of this failure — a browser tab cannot grow a
    /// JDK, so the only useful reply is which one-line manifest edit fixes it. The reason half now
    /// comes from the selection's own verdict rather than being restated here, which is exactly the
    /// kind of change that could silently reword it.
    #[test]
    fn the_javac_backend_reports_that_a_browser_has_no_host_process() {
        // Whether `javac` is the default or spelled out makes no difference to the answer.
        for source in [
            "[package]\nname = \"demo\"\n",
            "[build]\nbackend = { type = \"javac\" }\n",
        ] {
            let error = block_on_inline(Compile::workspace(&manifest(source), &subset_sources()))
                .err()
                .expect("javac cannot run here");
            assert!(
                matches!(
                    error,
                    CompileFailure::BackendUnavailable {
                        id: "javac",
                        reason: BackendAbsence::NoHostProcess,
                    }
                ),
                "{error}"
            );
            assert_eq!(
                error.to_string(),
                "javac needs a host process, and this host cannot run external compilers.\n\
                 Set `[build] backend = { type = \"jals\" }` in jals.toml for a downloadable .jar, \
                 or `{ type = \"jals-wasm\" }` for a WebAssembly module."
            );
        }
    }

    /// The whole point: packaged classes, compiled in-process, packaged into a named jar. Every
    /// `jals-javac` test compiles default-package types, so this is the first proof that a
    /// `package com.example;` project reaches a jar at all.
    #[test]
    fn a_packaged_project_compiles_into_a_jar() {
        let manifest = manifest(
            "[package]\nname = \"demo\"\n\n\
             [build]\nbackend = { type = \"jals\" }\n\n\
             [run]\nmain-class = \"com.example.Main\"\n",
        );
        let artifact = block_on_inline(Compile::workspace(&manifest, &subset_sources()))
            .expect("the subset compiles");
        assert_eq!(artifact.name, "demo.jar");
        assert!(artifact.bytes.starts_with(b"PK\x03\x04"), "not a zip");
        assert!(
            artifact.summary.contains("com.example.Main"),
            "{}",
            artifact.summary
        );
    }

    /// The same sources through the other backend: one module for the whole project.
    #[test]
    fn the_wasm_backend_yields_one_module() {
        let manifest = manifest("[build]\nbackend = { type = \"jals-wasm\" }\n");
        let artifact = block_on_inline(Compile::workspace(&manifest, &subset_sources()))
            .expect("the subset compiles");
        assert_eq!(artifact.name, "project.wasm");
        assert!(artifact.bytes.starts_with(b"\0asm"), "not a wasm module");
    }

    /// One module compiled here, and executed here: the browser tab that built it is also what
    /// runs it, with no download and no engine on the other side.
    #[test]
    fn a_module_runs_in_the_host_that_compiled_it() {
        let manifest = manifest("[build]\nbackend = { type = \"jals-wasm\" }\n");
        let artifact = block_on_inline(Compile::workspace(&manifest, &subset_sources()))
            .expect("the subset compiles");
        assert!(artifact.runnable);

        // A `static` method reached by name, with its argument read against the type the export
        // declares.
        assert_eq!(
            Execute::run(&artifact.bytes, "twice 21"),
            Ok("`twice` returned 42".to_owned())
        );
        // A cross-file call, which is what compiling every source as one unit is for.
        assert_eq!(
            Execute::run(&artifact.bytes, "run"),
            Ok("`run` returned 6".to_owned())
        );
        // Naming nothing is still a run.
        let Ok(report) = Execute::run(&artifact.bytes, "") else {
            panic!("instantiating is a run");
        };
        assert!(report.starts_with("instantiated project.wasm"), "{report}");
        // And what the engine refuses comes back as the answer, not as a panic.
        let Err(error) = Execute::run(&artifact.bytes, "absent") else {
            panic!("there is no `absent` export");
        };
        assert!(error.contains("no function named `absent`"), "{error}");
    }

    /// The jar is a download and nothing more: running the class files in it needs a JVM, which a
    /// browser tab cannot start. That is a property of the artifact, so the pane reads it off the
    /// compile rather than guessing from the file name.
    #[test]
    fn a_jar_is_not_something_this_host_can_run() {
        let manifest = manifest("[build]\nbackend = { type = \"jals\" }\n");
        let artifact = block_on_inline(Compile::workspace(&manifest, &subset_sources()))
            .expect("the subset compiles");
        assert!(!artifact.runnable);
    }

    /// The playground's seed project — `new`, string concatenation, arrays and all — compiles.
    ///
    /// This is the default experience, so it is the one that has to work rather than report. It used
    /// to be pinned as *not* compiled, back when the lowering had none of those three.
    #[test]
    fn the_seed_java_compiles_in_process() {
        let manifest = manifest(
            "[package]\nname = \"seed\"\n\n\
             [build]\nbackend = { type = \"jals\" }\n",
        );
        let files: Vec<(String, String)> = SAMPLE_FILES
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
            .collect();
        let artifact = block_on_inline(Compile::workspace(&manifest, &files))
            .expect("the seed project compiles");
        assert_eq!(artifact.name, "seed.jar");
        assert!(artifact.bytes.starts_with(b"PK\x03\x04"), "not a zip");
    }

    /// A path the storage layer would refuse is rejected with the path named, not unwrapped.
    #[test]
    fn an_unrepresentable_path_is_rejected_rather_than_panicking() {
        let manifest = manifest("[build]\nbackend = { type = \"jals\" }\n");
        let files = vec![("a/../b.java".to_owned(), "class B {}\n".to_owned())];
        let error = block_on_inline(Compile::workspace(&manifest, &files))
            .err()
            .expect("the path is not project-relative");
        assert!(matches!(error, CompileFailure::InvalidPath(_)), "{error}");
    }

    /// A `[package] name` that is not a plain file name must not become a download attribute.
    #[test]
    fn an_unsafe_package_name_falls_back_to_a_neutral_jar_name() {
        for name in ["../escape", "with/slash", ".hidden", ""] {
            let manifest = manifest(&format!("[package]\nname = \"{name}\"\n"));
            assert_eq!(
                Compile::jar_file_name(&manifest),
                FALLBACK_JAR,
                "for `{name}`"
            );
        }
        assert_eq!(
            Compile::jar_file_name(&manifest("[package]\nname = \"my-app_1.0\"\n")),
            "my-app_1.0.jar"
        );
    }

    /// Nothing in the pipeline reads a clock, so the same project always packages to the same jar.
    #[test]
    fn two_compiles_of_the_same_sources_are_byte_identical() {
        let manifest =
            manifest("[package]\nname = \"demo\"\n\n[build]\nbackend = { type = \"jals\" }\n");
        let sources = subset_sources();
        let first =
            block_on_inline(Compile::workspace(&manifest, &sources)).expect("first compile");
        let second =
            block_on_inline(Compile::workspace(&manifest, &sources)).expect("second compile");
        assert_eq!(first.bytes, second.bytes);
    }
}
