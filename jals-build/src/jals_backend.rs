//! The in-process compile backend: `jals-javac` behind the [`Backend`](crate::Backend) contract.
//!
//! Deliberately not behind the `native` feature. Compiling is pure computation, so this backend
//! runs wherever the rest of jals does — including `wasm32`, where a `javac` subprocess is not an
//! option at all and [`BackendAbsence::NoHostProcess`](crate::BackendAbsence) is the honest answer
//! for the alternative.
//!
//! # One compilation unit
//!
//! Every source in the request is indexed together before any of them is lowered. That is not an
//! optimisation: a call from one file to another needs the callee's descriptor, and a descriptor
//! needs the whole project's types resolved. Compiling file-by-file would mean each file seeing an
//! index that does not contain its siblings.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use jals_hir::{FileAnalysis, FileId, FileSemantics, ProjectIndex, TypedFile};
use jals_javac::lower::Compile;
use jals_javac::wasm::CompileWasm;
use jals_progress::{Activity, Outcome};
use jals_storage::{ContentDigest, ProvenanceFold, RelativePath};
use jals_syntax::{Parse, SyntaxNode};

use jals_native::PackageSelection;

use crate::backend::{Backend, BackendFuture, BackendOutcome, BackendRequest};

/// What the in-process compiler emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// One class file per declared type, for a JVM.
    ClassFiles { class_version: u16 },
    /// One WebAssembly module for the whole project, with the host's collector managing objects.
    ///
    /// Carries whether `assert` checks are emitted, because wasm has no run-time flag for them —
    /// see [`Assertions`](crate::Assertions).
    Wasm { assertions: bool },
}

/// Compiles with `jals-javac`, in this process.
pub struct JalsBackend {
    target: Target,
    /// The packages this project resolved — its platform, and anything `[build] native-packages`
    /// added.
    ///
    /// **Both targets read it, and they read it differently.** The wasm target compiles the
    /// implementation half beside the project's own sources and imports one host function per
    /// `native` method; the class-file target compiles none of it and indexes the whole selection
    /// as a signature record, because the JVM that loads the output supplies a real `java.base`
    /// that is a superset of anything shipped here.
    ///
    /// That asymmetry is the reason the class-file target holds a selection at all rather than an
    /// empty one. Without it there is no `java.lang.String` in the index and no implicit
    /// `java.lang.Object` supertype edge, so every reference into the standard library is an
    /// unresolved name and the lowering has no `String` to emit against.
    ///
    /// Held by the backend rather than passed on the request because it is *configuration*: it
    /// changes what comes out for unchanged input, which is exactly what
    /// [`config_digest`](Backend::config_digest) exists to fold.
    packages: PackageSelection,
}

impl JalsBackend {
    /// The project-relative path the whole-project WebAssembly module is published under.
    ///
    /// One name, because there is one module: wasm has no dynamic loading and no classpath, so the
    /// unit is the project rather than the declared type. Published as a constant because three
    /// places have to agree on it — this backend writes it, `jals run` reads it back to execute,
    /// and the playground offers it as a download — and a literal repeated in each is three places
    /// to edit and two chances to be wrong.
    pub const WASM_MODULE: &'static str = "project.wasm";

    /// The class-file major version each `--release N` produces: 45 for Java 1.1, then one per
    /// release (JVMS §4.1 Table 4.1-A).
    ///
    /// Saturating rather than wrapping: a release beyond what a `u16` can name is not a version
    /// this can emit, and clamping keeps the arithmetic total.
    fn major_version(release: u32) -> u16 {
        u16::try_from(release)
            .unwrap_or(u16::MAX)
            .saturating_add(44)
    }

    /// A backend emitting class files for `release` (`--release N`), defaulting to Java 25 when the
    /// manifest names no level — the same default `jals init` scaffolds.
    ///
    /// Crate-internal, like [`wasm`](Self::wasm): a host reaches this backend by calling
    /// [`BackendSelection`](crate::BackendSelection), which is what keeps the `[build] backend`
    /// decision table in one place. Constructing it directly is what that seam replaced.
    pub(crate) fn new(release: Option<u32>, packages: PackageSelection) -> Self {
        // Java 25 when the manifest names no level, matching what `jals init` scaffolds.
        Self {
            target: Target::ClassFiles {
                class_version: Self::major_version(release.unwrap_or(25)),
            },
            packages,
        }
    }

    /// A backend emitting one WebAssembly module for the whole project.
    ///
    /// `release` has no meaning here: there is no class-file version to pick, and no JVM to accept
    /// it. What bounds the output instead is the language subset with a wasm representation.
    ///
    /// `assertions` takes the place `-ea` has on the other target: a JVM decides at start-up
    /// whether a class file's `assert` checks run, and a wasm host has no such moment.
    pub(crate) const fn wasm(assertions: crate::Assertions, packages: PackageSelection) -> Self {
        Self {
            target: Target::Wasm {
                assertions: assertions.enabled(),
            },
            packages,
        }
    }

    /// Whether this target compiles a package's Java into its own artifact.
    ///
    /// The mirror of `jals_config::Manifest::links_packages`, asked of the target rather than of the
    /// manifest because that is what this backend was handed. The two must agree, and they do for
    /// one reason: `BackendSelection` is what turns the manifest's answer into this target.
    const fn links(&self) -> bool {
        matches!(self.target, Target::Wasm { .. })
    }

    /// The package units this compile **lowers**, each at the fidelity that follows.
    ///
    /// Empty for the class-file target: a package's `native` method is a host function supplied to
    /// a WebAssembly module, and a class file has nowhere to put one.
    fn compiled_sources(
        &self,
    ) -> impl Iterator<Item = (&jals_native::JavaSource, jals_hir::LibraryFidelity)> {
        self.links()
            .then(|| self.packages.link_sources())
            .into_iter()
            .flatten()
            .map(|(_, source)| (source, jals_hir::LibraryFidelity::Complete))
    }

    /// The package units this compile **only indexes**: the signature tier always, and the whole
    /// selection when nothing is linked.
    fn recorded_sources(
        &self,
    ) -> impl Iterator<Item = (&jals_native::JavaSource, jals_hir::LibraryFidelity)> {
        let links = self.links();
        self.packages
            .analysis_sources()
            .filter(move |(_, source)| {
                !links || !matches!(source.kind, jals_native::SourceKind::Implementation)
            })
            .map(|(_, source)| (source, jals_hir::LibraryFidelity::Signatures))
    }

    /// Parse, index, and lower every source together, collecting the class files.
    ///
    /// `async` all the way down rather than `block_on_inline` at each step: the parser, the index
    /// builder, and inference all yield cooperatively, and driving them on an inline executor from
    /// inside this future would swallow every one of those yields — the host's current-thread
    /// runtime would sit on one compile for its whole duration.
    async fn compile_all(&self, request: &BackendRequest<'_>) -> BackendOutcome {
        // One unit for the whole compile, counted in files. A per-file *line* would be the wrong
        // shape — cargo says `Compiling <package>` once, not once per module — but the bar under it
        // is what makes a hundred-file project look like progress instead of a hang.
        let report =
            request
                .progress
                .begin_bounded(Activity::Compile, "", request.tree.len() as u64);
        let mut roots: Vec<(FileId, SyntaxNode)> = Vec::with_capacity(request.tree.len());
        let mut messages = Vec::new();
        for (index, source) in request.tree.iter().enumerate() {
            let Ok(text) = core::str::from_utf8(&source.bytes) else {
                messages.push(format!("{}: not valid UTF-8", source.path));
                continue;
            };
            let file = FileId(u32::try_from(index).unwrap_or(u32::MAX));
            roots.push((file, Parse::parse(text).await.syntax()));
        }
        if !messages.is_empty() {
            report.finish(Outcome::Failed);
            return BackendOutcome::failed(messages);
        }
        // The resolved packages' Java, parsed into the same compile, **implementation units
        // first**. That order is what lets one `split_at` hand the lowering exactly the units it
        // compiles: a signature unit has no body to lower, and `java.lang.Object` is one of them —
        // it is the backend's own `anyref`, so a declared `Object` would be one question with two
        // answers. Ordering here is what makes that structural rather than a rule to remember.
        let project_files = roots.len();
        let package_sources: Vec<(&jals_native::JavaSource, jals_hir::LibraryFidelity)> = self
            .compiled_sources()
            .chain(self.recorded_sources())
            .collect();
        let compiled_files = self.compiled_sources().count();
        for (source, _) in &package_sources {
            roots.push((
                FileId::library(u32::try_from(roots.len() - project_files).unwrap_or(u32::MAX)),
                Parse::parse(source.text.as_ref()).await.syntax(),
            ));
        }

        // Each file's own analysis first: it needs no index, so it is the half that could be
        // computed before one exists.
        let mut analyses: Vec<FileAnalysis> = Vec::with_capacity(roots.len());
        for (_, root) in &roots {
            analyses.push(FileAnalysis::of(root).await);
        }

        // One library slot, both tiers, each unit carrying the fidelity its target decides. The
        // wasm target reads an implementation unit as the code that *will run*, so what it does not
        // declare the program does not have; the class-file target reads the same text as a record,
        // because the JVM loading the output supplies a real `java.base` that is a superset of it.
        let (project_roots, library_roots) = roots.split_at(project_files);
        let library: Vec<jals_hir::LibraryFile> = library_roots
            .iter()
            .zip(&package_sources)
            .map(|((file, root), (_, fidelity))| jals_hir::LibraryFile {
                file: *file,
                root: root.clone(),
                fidelity: *fidelity,
            })
            .collect();
        let index = ProjectIndex::builder(project_roots)
            .with_library(&library)
            .build()
            .await;

        // Bind each analysis to the index, then force the inference. The bindings must outlive the
        // witnesses that borrow their memo cells, so both vectors are held for the whole compile.
        let semantics: Vec<FileSemantics<'_>> = roots
            .iter()
            .zip(&analyses)
            .map(|((file, _), analysis)| analysis.in_project(&index, *file))
            .collect();
        let mut typed_files: Vec<TypedFile<'_>> = Vec::with_capacity(semantics.len());
        for binding in &semantics {
            typed_files.push(binding.typed().await);
        }
        let (typed_project, typed_library) = typed_files.split_at(project_files);
        let typed_natives = &typed_library[..compiled_files];

        let class_version = match self.target {
            Target::ClassFiles { class_version } => class_version,
            // wasm has no dynamic loading and no classpath, so the whole project is one module
            // rather than one artifact per declared type.
            Target::Wasm { assertions } => {
                // The whole project is one module, so this arm *is* the wasm compile — and it
                // returns past the `finish` below. Ending the unit here is what keeps a green
                // wasm build from reporting `Abandoned`, which says the emitter has a hole in it.
                let options = jals_javac::wasm::WasmOptions { assertions };
                let outcome =
                    match CompileWasm::project(typed_project, typed_natives, &index, options) {
                        Ok(module) => match RelativePath::parse(Self::WASM_MODULE) {
                            Ok(path) => BackendOutcome::compiled(alloc::vec![(path, module)]),
                            Err(error) => BackendOutcome::failed(alloc::vec![format!("{error:?}")]),
                        },
                        Err(error) => BackendOutcome::failed(alloc::vec![format!("{error}")]),
                    };
                report.finish(if outcome.success() {
                    Outcome::Completed
                } else {
                    Outcome::Failed
                });
                return outcome;
            }
        };

        let mut classes = Vec::new();
        for (source, typed) in request.tree.iter().zip(typed_project) {
            report.advance(1);
            match Compile::file(*typed, class_version) {
                Ok(compiled) => {
                    for class in compiled {
                        // A type's internal name is also its output path, `/` separators and all.
                        match RelativePath::parse(&format!("{}.class", class.internal_name)) {
                            Ok(path) => classes.push((path, class.bytes)),
                            Err(error) => messages.push(format!(
                                "{}: not a writable path ({error:?})",
                                class.internal_name
                            )),
                        }
                    }
                }
                Err(error) => messages.push(format!("{}: {error}", source.path)),
            }
        }
        if messages.is_empty() {
            report.finish(Outcome::Completed);
            BackendOutcome::compiled(classes)
        } else {
            report.finish(Outcome::Failed);
            BackendOutcome::failed(messages)
        }
    }
}

impl Backend for JalsBackend {
    /// The manifest's own `type` tag, not a literal beside it: `[build.backend]` and the cache key
    /// have to name this backend with one string, or the two drift apart silently.
    fn id(&self) -> &'static str {
        match self.target {
            Target::ClassFiles { .. } => jals_config::BackendKind::Jals {}.tag_name(),
            Target::Wasm { .. } => jals_config::BackendKind::JalsWasm {}.tag_name(),
        }
    }

    fn config_digest(&self, request: &BackendRequest<'_>) -> ContentDigest {
        let mut fold = ProvenanceFold::new(b"jals.backend.jals\0");
        // The "tool identity" a subprocess backend has to fold in is, for this one, the compiler
        // that shipped in this binary — so `jals-javac`'s version stands in for the installed JDK.
        // Reading `CARGO_PKG_VERSION` here would name *this* crate instead, which is the wrong
        // tool: `jals-build` only routes to the compiler.
        fold.bytes(jals_javac::VERSION.as_bytes())
            .bytes(match self.target {
                Target::ClassFiles { .. } => b"class",
                Target::Wasm { .. } => b"wasm",
            })
            // One slot, two meanings, because the two targets have one number each that changes
            // what comes out: the class-file version there, and whether `assert` checks are
            // emitted here. Folding the wasm one is what keeps a test compile's module out of an
            // ordinary build's cache entry — without it, `jals build` would be served the
            // assertion-checking module a `jals test` left behind, and vice versa.
            .version(match self.target {
                Target::ClassFiles { class_version } => u32::from(class_version),
                Target::Wasm { assertions } => u32::from(assertions),
            })
            .digest(request.options.digest())
            // What the selected native packages contribute: their names, their versions, the Java
            // they publish, and the import keys they bind. Not the Rust bodies behind those keys —
            // nothing can observe one — which is why a package carries an author-set version and
            // that version is in here.
            .bytes(&self.packages.provenance());
        fold.finish()
    }

    fn compile<'a>(&'a self, request: &'a BackendRequest<'a>) -> BackendFuture<'a> {
        Box::pin(async move { Ok(self.compile_all(request).await) })
    }

    fn describe(&self, request: &BackendRequest<'_>) -> String {
        match self.target {
            Target::ClassFiles { class_version } => format!(
                "jals-javac: {} source(s) -> class files at major version {class_version}",
                request.tree.len()
            ),
            Target::Wasm { .. } => format!(
                "jals-javac: {} source(s) -> one WebAssembly module (host-managed memory)",
                request.tree.len()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendOptions, BackendSource};
    use jals_storage::{CacheKey, CacheNamespace};

    fn source(path: &str, text: &str) -> BackendSource {
        let bytes = text.as_bytes().to_vec();
        BackendSource {
            path: RelativePath::parse(path).expect("a valid path"),
            key: CacheKey::new(
                CacheNamespace::FrontendOutput,
                ContentDigest::of(b"test"),
                ContentDigest::of(&bytes),
            ),
            bytes,
        }
    }

    /// The platform, as every real project resolves one.
    ///
    /// A compile with [`PackageSelection::empty`] has no `java.lang` at all — not `String`, not the
    /// implicit `Object` supertype edge — and refuses with "`String` is not an indexed type". That
    /// is the honest answer rather than a regression: there is no fallback name list behind the
    /// packages any more, so a host that resolves none gets none.
    fn platform() -> PackageSelection {
        let mut resolver = jals_native::StaticResolver::new("test");
        resolver.add(jals_platform::JavaBase::package(alloc::rc::Rc::new(
            jals_platform::CapturedHost::new(),
        )));
        jals_native::ResolverChain::new()
            .push(Box::new(resolver))
            .select(&[jals_platform::JavaBase::NAME.to_owned()])
            .expect("the platform ships with this build")
    }

    /// Two files compiled as one unit: `Main` calls a method declared in `Helper`, which only
    /// resolves because both are indexed before either is lowered.
    #[test]
    fn a_project_compiles_as_one_unit() {
        let tree = [
            source(
                "Main.java",
                "public class Main { public static void main(String[] a) { Helper.twice(1); } }",
            ),
            source(
                "Helper.java",
                "public class Helper { static int twice(int n) { return n + n; } }",
            ),
        ];
        let options = BackendOptions::default();
        let request = BackendRequest {
            progress: &jals_progress::Progress::SILENT,
            tree: &tree,
            classpath: &[],
            options: &options,
        };

        let backend = JalsBackend::new(Some(25), platform());
        let outcome = jals_exec::block_on_inline(backend.compile(&request)).expect("compile");
        assert!(outcome.success(), "messages: {:?}", outcome.messages);

        let names: Vec<String> = outcome
            .artifacts
            .iter()
            .map(|(path, _)| path.to_string())
            .collect();
        assert_eq!(names, ["Main.class", "Helper.class"]);
        // Every emitted file is a class file, magic and all.
        for (_, bytes) in &outcome.artifacts {
            assert_eq!(&bytes[..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
        }
    }

    /// A source the lowering cannot compile is reported, not silently dropped.
    ///
    /// The fixture is a lambda: a construct the lowering still reports rather than emits. It used to
    /// be `new int[1]`, which now compiles — so any replacement has to be something the lowering
    /// genuinely refuses, or the test would assert nothing.
    #[test]
    fn an_uncompilable_source_is_reported() {
        let tree = [source(
            "Arrays.java",
            "public class Arrays { public static void main(String[] a) { Runnable r = () -> {}; } }",
        )];
        let options = BackendOptions::default();
        let request = BackendRequest {
            progress: &jals_progress::Progress::SILENT,
            tree: &tree,
            classpath: &[],
            options: &options,
        };

        let outcome = jals_exec::block_on_inline(
            JalsBackend::new(None, PackageSelection::empty()).compile(&request),
        )
        .expect("compile");
        assert!(!outcome.success());
        assert!(
            outcome
                .messages
                .iter()
                .any(|m| m.starts_with("Arrays.java")),
            "expected a message naming the file, got {:?}",
            outcome.messages
        );
    }

    /// A backend's `id` is the manifest's own `type` tag. Two literals that merely agree today
    /// would let `[build.backend]` and the cache key drift apart with nothing to notice.
    #[test]
    fn a_backend_is_named_by_its_manifest_tag() {
        assert_eq!(
            JalsBackend::new(None, PackageSelection::empty()).id(),
            jals_config::BackendKind::Jals {}.tag_name()
        );
        assert_eq!(
            JalsBackend::wasm(crate::Assertions::Disabled, PackageSelection::empty()).id(),
            jals_config::BackendKind::JalsWasm {}.tag_name()
        );
    }

    /// The compiler that shipped in this binary is the tool whose identity the key folds — the
    /// counterpart of the installed JDK's version for the `javac` backend.
    #[test]
    fn the_config_digest_folds_the_compiler_and_the_target() {
        let tree = [source("Main.java", "public class Main {}")];
        let options = BackendOptions::default();
        let request = BackendRequest {
            progress: &jals_progress::Progress::SILENT,
            tree: &tree,
            classpath: &[],
            options: &options,
        };
        assert_ne!(
            JalsBackend::new(Some(25), platform()).config_digest(&request),
            JalsBackend::wasm(crate::Assertions::Disabled, PackageSelection::empty())
                .config_digest(&request),
            "two targets are two sets of artifacts"
        );
        assert_ne!(
            JalsBackend::new(Some(21), PackageSelection::empty()).config_digest(&request),
            JalsBackend::new(Some(25), platform()).config_digest(&request),
            "a class-file version is part of the output"
        );
        // The wasm half of the same slot, and the one claim in `config_digest` nothing was
        // asserting: `Assertions` decides whether the module carries the `assert` checks, so a
        // test compile's module and a build's are two artifacts. Reverting the wasm arm to a
        // constant `.version(0)` — the regression the comment there warns about — passed every
        // gate without this line, and `Backend::config_digest` has no production caller yet
        // (`CacheNamespace::BackendOutput` memoization is still the TODO in `backend.rs`), so
        // this test is the only thing holding the property until one exists.
        assert_ne!(
            JalsBackend::wasm(crate::Assertions::Disabled, PackageSelection::empty())
                .config_digest(&request),
            JalsBackend::wasm(crate::Assertions::Enabled, PackageSelection::empty())
                .config_digest(&request),
            "an assertion-checking module is not the module a build produces"
        );
    }

    /// `--release N` selects the class-file version, which is what a JVM checks before anything
    /// else in the file.
    #[test]
    fn the_release_level_selects_the_class_version() {
        assert_eq!(JalsBackend::major_version(8), 52);
        assert_eq!(JalsBackend::major_version(17), 61);
        assert_eq!(JalsBackend::major_version(25), 69);
    }
}
