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
use jals_storage::{ContentDigest, ProvenanceFold, RelativePath};
use jals_syntax::{Parse, SyntaxNode};

use crate::backend::{Backend, BackendFuture, BackendOutcome, BackendRequest};

/// What the in-process compiler emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// One class file per declared type, for a JVM.
    ClassFiles { class_version: u16 },
    /// One WebAssembly module for the whole project, with the host's collector managing objects.
    Wasm,
}

/// Compiles with `jals-javac`, in this process.
pub struct JalsBackend {
    target: Target,
}

impl JalsBackend {
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
    pub(crate) fn new(release: Option<u32>) -> Self {
        // Java 25 when the manifest names no level, matching what `jals init` scaffolds.
        Self {
            target: Target::ClassFiles {
                class_version: Self::major_version(release.unwrap_or(25)),
            },
        }
    }

    /// A backend emitting one WebAssembly module for the whole project.
    ///
    /// `release` has no meaning here: there is no class-file version to pick, and no JVM to accept
    /// it. What bounds the output instead is the language subset with a wasm representation.
    pub(crate) const fn wasm() -> Self {
        Self {
            target: Target::Wasm,
        }
    }

    /// Parse, index, and lower every source together, collecting the class files.
    ///
    /// `async` all the way down rather than `block_on_inline` at each step: the parser, the index
    /// builder, and inference all yield cooperatively, and driving them on an inline executor from
    /// inside this future would swallow every one of those yields — the host's current-thread
    /// runtime would sit on one compile for its whole duration.
    async fn compile_all(&self, request: &BackendRequest<'_>) -> BackendOutcome {
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
            return BackendOutcome::failed(messages);
        }

        // Each file's own analysis first: it needs no index, so it is the half that could be
        // computed before one exists.
        let mut analyses: Vec<FileAnalysis> = Vec::with_capacity(roots.len());
        for (_, root) in &roots {
            analyses.push(FileAnalysis::of(root).await);
        }

        // The stdlib stubs stand in for `java.base`: the JVM supplies the implementations at run
        // time, so a compile only ever needs the signatures.
        let index = ProjectIndex::builder(&roots).with_stdlib().build().await;

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

        let class_version = match self.target {
            Target::ClassFiles { class_version } => class_version,
            // wasm has no dynamic loading and no classpath, so the whole project is one module
            // rather than one artifact per declared type.
            Target::Wasm => {
                return match CompileWasm::project(&typed_files, &index) {
                    Ok(module) => match RelativePath::parse("project.wasm") {
                        Ok(path) => BackendOutcome::compiled(alloc::vec![(path, module)]),
                        Err(error) => BackendOutcome::failed(alloc::vec![format!("{error:?}")]),
                    },
                    Err(error) => BackendOutcome::failed(alloc::vec![format!("{error}")]),
                };
            }
        };

        let mut classes = Vec::new();
        for (source, typed) in request.tree.iter().zip(&typed_files) {
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
            BackendOutcome::compiled(classes)
        } else {
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
            Target::Wasm => jals_config::BackendKind::JalsWasm {}.tag_name(),
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
                Target::Wasm => b"wasm",
            })
            .version(match self.target {
                Target::ClassFiles { class_version } => u32::from(class_version),
                Target::Wasm => 0,
            })
            .digest(request.options.digest());
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
            Target::Wasm => format!(
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
            tree: &tree,
            classpath: &[],
            options: &options,
        };

        let backend = JalsBackend::new(Some(25));
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
            tree: &tree,
            classpath: &[],
            options: &options,
        };

        let outcome =
            jals_exec::block_on_inline(JalsBackend::new(None).compile(&request)).expect("compile");
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
            JalsBackend::new(None).id(),
            jals_config::BackendKind::Jals {}.tag_name()
        );
        assert_eq!(
            JalsBackend::wasm().id(),
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
            tree: &tree,
            classpath: &[],
            options: &options,
        };
        assert_ne!(
            JalsBackend::new(Some(25)).config_digest(&request),
            JalsBackend::wasm().config_digest(&request),
            "two targets are two sets of artifacts"
        );
        assert_ne!(
            JalsBackend::new(Some(21)).config_digest(&request),
            JalsBackend::new(Some(25)).config_digest(&request),
            "a class-file version is part of the output"
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
