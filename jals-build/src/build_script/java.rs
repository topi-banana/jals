//! The Java build-script engine.
//!
//! A `build.java` is a Java program, and this module is what makes it a *script*: it is compiled
//! to one WebAssembly module by the same in-process backend a project uses, linked against the
//! `java.base` package, and run on the same engine a `jals-wasm` project runs on — with a budget
//! and with statement positions, so a run that stops says where.
//!
//! # Where the script's calls land
//!
//! A script's `Project.readText(...)` is an ordinary Java call into the `jals.build` package this
//! module builds. That package is a native package in the ordinary sense: Java with `native`
//! declarations, and one Rust binding per declaration. The declarations reach the host as wasm
//! imports, and [`Api::package`] is the table that answers them — every binding closes over this
//! run's pending output, so a script's writes and directives are buffered in one place until the
//! run commits them.
//!
//! What crosses the boundary is numbers and arrays, never strings: a `native` method takes a
//! `char[]` and answers with lengths and fills, because a `String`'s representation is the
//! backend's own layout. `jals-build/java/jals/build/*.java` is the Java half of that protocol,
//! and each of its `native` methods has one binding here.
//!
//! # What a script never sees
//!
//! `System.out` is captured and discarded: the sink a platform package writes to is the host's,
//! and a script's diagnostics belong in `Build.warning` and `Build.error`, where they are ordered,
//! bounded, and travel with a failure. A script also
//! cannot read its own module's exports or reach the filesystem: every read is a project key
//! through `Project`, and every write is buffered below the output root through `Output`.

use alloc::borrow::ToOwned as _;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString as _};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ops::Range;

use jals_hir::{FileAnalysis, FileId, FileSemantics, ProjectIndex, TypedFile};
use jals_javac::wasm::{CompileWasm, LibraryAbi, LinkedLibrary, WasmError, WasmOptions};
use jals_native::console::{CapturedConsole, ConsoleSink};
use jals_native::{
    Args, NativeError, NativeHost, NativePackage, NativePackageSet, NativeRegistry, NativeValue,
    RefSlot, Results,
};
use jals_progress::Progress;
use jals_storage::{DirKey, EntryRef, FileKey, ProjectView, RelativePath};
use jals_syntax::{Parse, SyntaxNode};

use super::{
    BUILD_ARTIFACT_ROOT, BUILD_SCRIPT_OUTPUT_ROOT, BuildScriptDiagnostic, BuildScriptEnvironment,
    BuildScriptError, BuildScriptLimits, BuildScriptPosition, PendingOutput,
};
use crate::task::{
    TaskDigestAlgorithm, TaskFetchKind, TaskId, TaskMappingFormat, TaskNodeKind, TaskPublishIntent,
    TaskPublishMode, TaskRemapDirection, TaskTerminal, TasksApi,
};
use crate::wasm_run::{WasmLibrary, WasmRunError, WasmRunRequest, WasmRunner};

/// The name the `jals.build` package is selected and cached under.
const PACKAGE: &str = "jals.build";

/// The package author's version. Bumping it is part of changing the API's *behavior*: the
/// build-script fingerprint carries the API version, not this one, so a Java-text change that
/// changes what a script observes has to bump [`super::BUILD_SCRIPT_API_VERSION`].
///
/// 2: the mapping-text task surface — `jarText`, `composeMappings`, `copyMappings`,
/// `resolveReferences`, `jsonFromLines` and the `publishText` terminal.
const VERSION: u32 = 2;

/// The exported method a script's entry point is.
const MAIN: &str = "main";

/// Every compilation unit the `jals.build` package publishes, in declaration order.
const SOURCES: &[(&str, &str)] = &[
    (
        "jals/build/Text.java",
        include_str!("../../java/jals/build/Text.java"),
    ),
    (
        "jals/build/Project.java",
        include_str!("../../java/jals/build/Project.java"),
    ),
    (
        "jals/build/Output.java",
        include_str!("../../java/jals/build/Output.java"),
    ),
    (
        "jals/build/Build.java",
        include_str!("../../java/jals/build/Build.java"),
    ),
    (
        "jals/build/MappingFormat.java",
        include_str!("../../java/jals/build/MappingFormat.java"),
    ),
    (
        "jals/build/Tasks.java",
        include_str!("../../java/jals/build/Tasks.java"),
    ),
];

/// The engine one `build.java` script runs on.
pub(super) struct Engine;

impl Engine {
    /// Compile, link, and run the project's configured Java script against `view`.
    ///
    /// The returned [`PendingOutput`] is buffered state — files, directives, diagnostics, and the
    /// finished task plan — and the caller checks its error
    /// diagnostics and turns it into cache state. Nothing here publishes anything.
    ///
    /// `script` is the script's text, already read and size-checked by the caller.
    pub(super) async fn evaluate(
        view: &ProjectView,
        script_key: &FileKey,
        script: &str,
        environment: &BuildScriptEnvironment,
        limits: &BuildScriptLimits,
    ) -> Result<PendingOutput, BuildScriptError> {
        let pending = Rc::new(RefCell::new(PendingOutput::new(limits.clone())));
        let tasks = TasksApi::new(limits.task_plan_limits());
        let api = Api {
            view: view.clone(),
            environment: environment.clone(),
            limits: limits.clone(),
            pending: Rc::clone(&pending),
            scratch: Rc::new(RefCell::new(Scratch::default())),
            tasks: tasks.clone(),
        };

        let selection = Self::selection(&api, script_key)?;
        let inputs = Self::parse_inputs(script, script_key, &selection).await?;
        let module = Self::compile(&inputs, script_key).await?;
        Self::run(&module, &inputs, &selection, script_key, limits)?;

        let task_plan = tasks.finish().map_err(|error| BuildScriptError::Execute {
            script: script_key.clone(),
            position: None,
            message: error.to_string(),
        })?;
        // The bindings inside `selection` hold `pending` alive, so it cannot be unwrapped; the
        // run is over, and taking the value leaves nothing behind that could write to it again.
        let mut pending = core::mem::replace(
            &mut *pending.borrow_mut(),
            PendingOutput::new(limits.clone()),
        );
        pending.task_plan = task_plan;
        Ok(pending)
    }

    /// The `jals.build` Java and its bindings, beside the `java.base` a script's `String` is.
    ///
    /// The platform is selected by name like any other package, and it is a *library* package:
    /// the script compiles against the Java its module's ABI publishes and links the module
    /// itself, so a script's `String` is the same class a project's is rather than a stub written
    /// for scripts. Its console is captured and discarded — see the module docs.
    fn selection(api: &Api, script_key: &FileKey) -> Result<NativePackageSet, BuildScriptError> {
        let console: Rc<dyn ConsoleSink> = Rc::new(CapturedConsole::new());
        let mut registry = NativeRegistry::new();
        registry.add(jals_platform::Platform::package(console));
        registry.add(api.package());
        registry
            .select(&[PACKAGE.to_owned(), jals_platform::Platform::NAME.to_owned()])
            .map_err(|error| BuildScriptError::Execute {
                script: script_key.clone(),
                position: None,
                message: error.to_string(),
            })
    }

    /// Parse every compile input and number it the way `CompileWasm` will.
    ///
    /// The order is the engine's own contract: the project's files first — here, exactly the
    /// script — then the selected packages' lowered sources, then every linked library's
    /// published Java. A [`FileId`] is an index into that order, and [`Failure`] is what turns one
    /// back into a path.
    async fn parse_inputs(
        script: &str,
        script_key: &FileKey,
        selection: &NativePackageSet,
    ) -> Result<Inputs, BuildScriptError> {
        let mut files = Vec::new();
        let mut roots = Vec::new();
        files.push(Input {
            path: script_key.to_string(),
            text: script.to_owned(),
            script: true,
        });
        roots.push((FileId(0), Parse::parse(script).await.syntax()));
        let project_files = roots.len();

        for (_, source) in selection.lowered_sources() {
            let file = FileId(u32::try_from(roots.len()).unwrap_or(u32::MAX));
            roots.push((file, Parse::parse(source.text).await.syntax()));
            files.push(Input {
                path: source.path.to_owned(),
                text: source.text.to_owned(),
                script: false,
            });
        }

        // A library's ABI is decoded here rather than at instantiation so a module that is not a
        // linked library fails under the package that shipped it, in the compiler's vocabulary.
        let mut linked: Vec<(String, LibraryAbi)> = Vec::new();
        for (name, bytes) in selection.libraries() {
            let abi = LibraryAbi::of_module(bytes).map_err(|error| BuildScriptError::Execute {
                script: script_key.clone(),
                position: None,
                message: format!("native package `{name}`: {error}"),
            })?;
            linked.push((name.to_owned(), abi));
        }

        // The published Java is indexed but never lowered — the code is already in the library's
        // module — so it takes file ids after every root without joining `roots`.
        let mut library_roots: Vec<(FileId, SyntaxNode)> = Vec::new();
        for (_, abi) in &linked {
            for source in &abi.sources {
                let file =
                    FileId(u32::try_from(roots.len() + library_roots.len()).unwrap_or(u32::MAX));
                library_roots.push((file, Parse::parse(&source.text).await.syntax()));
                files.push(Input {
                    path: source.path.clone(),
                    text: source.text.clone(),
                    script: false,
                });
            }
        }

        Ok(Inputs {
            files,
            roots,
            project_files,
            library_roots,
            linked,
        })
    }

    /// Index the inputs together and lower them into one module, with positions on.
    async fn compile(inputs: &Inputs, script_key: &FileKey) -> Result<Vec<u8>, BuildScriptError> {
        // Each file's own analysis first: it needs no index, so it is the half that could be
        // computed before one exists.
        let mut analyses: Vec<FileAnalysis> = Vec::with_capacity(inputs.roots.len());
        for (_, root) in &inputs.roots {
            analyses.push(FileAnalysis::of(root).await);
        }

        let (project_roots, native_roots) = inputs.roots.split_at(inputs.project_files);
        let index = ProjectIndex::builder(project_roots)
            .with_native_packages(native_roots)
            .with_source_deps(&inputs.library_roots)
            .with_stdlib()
            .build()
            .await;

        // The bindings must outlive the witnesses that borrow their memo cells, so both vectors
        // are held for the whole compile.
        let semantics: Vec<FileSemantics<'_>> = inputs
            .roots
            .iter()
            .zip(&analyses)
            .map(|((file, _), analysis)| analysis.in_project(&index, *file))
            .collect();
        let mut typed_files: Vec<TypedFile<'_>> = Vec::with_capacity(semantics.len());
        for binding in &semantics {
            typed_files.push(binding.typed().await);
        }
        let (typed_project, typed_natives) = typed_files.split_at(inputs.project_files);
        let linked: Vec<LinkedLibrary<'_>> = inputs
            .linked
            .iter()
            .map(|(name, abi)| LinkedLibrary { name, abi })
            .collect();

        CompileWasm::project_linked(
            typed_project,
            typed_natives,
            &linked,
            &index,
            WasmOptions {
                // A run that stops has to say where: the failure mapping below turns the module's
                // statement positions into the position a `BuildScriptError` carries.
                positions: true,
                ..WasmOptions::default()
            },
        )
        .map_err(|error| Failure::compile(&error, &inputs.files, script_key))
    }

    /// Instantiate the module with the platform linked under its name, and call the script.
    fn run(
        module: &[u8],
        inputs: &Inputs,
        selection: &NativePackageSet,
        script_key: &FileKey,
        limits: &BuildScriptLimits,
    ) -> Result<(), BuildScriptError> {
        let libraries: Vec<WasmLibrary<'_>> = selection
            .libraries()
            .map(|(name, bytes)| WasmLibrary { name, bytes })
            .collect();
        let bindings = selection.bindings();
        WasmRunner::run(&WasmRunRequest {
            module,
            invoke: Some(MAIN),
            args: &[],
            natives: &bindings,
            libraries: &libraries,
            foreign: &[],
            fuel: Some(Self::fuel(limits)),
            progress: &Progress::SILENT,
        })
        .map_err(|error| Failure::run(&error, &inputs.files, script_key))?;
        Ok(())
    }

    /// The instruction budget for one run.
    ///
    /// `max_operations` is the source-level operation budget the limit has always been, and one
    /// source operation — an expression, the call around it, the statement it is part of — is
    /// about this many WebAssembly instructions. Metering is per instruction, so the conversion
    /// has to be stated somewhere; it is stated here, and the limit's unit follows in the same
    /// change.
    fn fuel(limits: &BuildScriptLimits) -> u32 {
        const INSTRUCTIONS_PER_OPERATION: u64 = 16;
        u32::try_from(
            limits
                .max_operations
                .saturating_mul(INSTRUCTIONS_PER_OPERATION),
        )
        .unwrap_or(u32::MAX)
        .max(1)
    }
}

/// The compile's inputs, numbered the way [`CompileWasm`] numbers them.
struct Inputs {
    /// One entry per file id, in id order.
    files: Vec<Input>,
    /// The project's own roots, then the selected packages' lowered sources — everything that is
    /// *lowered* into the module.
    roots: Vec<(FileId, SyntaxNode)>,
    /// How many of `roots` are the project's own.
    project_files: usize,
    /// Every linked library's published Java, indexed but not lowered.
    library_roots: Vec<(FileId, SyntaxNode)>,
    /// The link name and decoded ABI of every linked library, in selection order.
    linked: Vec<(String, LibraryAbi)>,
}

/// One compilation unit the engine handed the compile.
struct Input {
    /// The logical path, as a diagnostic names it.
    path: String,
    /// The text, so a byte range can be turned back into a line and column.
    text: String,
    /// Whether this is the script itself, whose failures carry a position in the error rather
    /// than a path in the message.
    script: bool,
}

/// The one host buffer a call that cannot answer in a single return value leaves its result in.
///
/// A native method answers with numbers and can fill arrays it was handed, but it cannot hand back
/// a `String` — so a result that is one is two calls: a length query that leaves the value here,
/// and a fill that copies it out. The Java half of the protocol always makes the two adjacent, and
/// keeping the values here rather than asking the host for them twice is also what keeps a project
/// read, a directory listing, and a generated file's key consistent between the two calls.
#[derive(Default)]
struct Scratch {
    /// The canonical key of the most recent output write.
    key: Option<String>,
    /// The most recent string list a count query produced: directory entries, walked files,
    /// enabled features, or one environment value.
    listing: Vec<String>,
    /// The most recent file bytes a size query read.
    bytes: Vec<u8>,
}

/// The per-run state every `jals.build` binding closes over.
#[derive(Clone)]
struct Api {
    view: ProjectView,
    environment: BuildScriptEnvironment,
    limits: BuildScriptLimits,
    pending: Rc<RefCell<PendingOutput>>,
    scratch: Rc<RefCell<Scratch>>,
    /// The task graph the `Tasks` half of the package records into. Shared with [`Engine::evaluate`],
    /// which takes the finished plan after the run.
    tasks: TasksApi,
}

impl Api {
    /// The `jals.build` package for this run: the Java a script compiles against, and one binding
    /// per `native` method that Java declares.
    ///
    /// Every declaration has to be bound. The package's Java is compiled *into* the script's
    /// module, so each `native` method is an import of that module whether or not the script ever
    /// calls it — a declaration without a binding is a module that will not link.
    fn package(&self) -> NativePackage {
        /// The code units of the `char[]` argument at `position`, decoded as text.
        ///
        /// Nested rather than an [`Api`] method because every binding that takes a string needs
        /// exactly these three lines, and the method form would make each call site repeat `Api::`.
        fn text(
            host: &mut dyn NativeHost,
            args: &Args<'_>,
            position: usize,
        ) -> Result<String, NativeError> {
            let slot = args.reference(position)?;
            let length = host.array_len(slot)?;
            host.array_text(slot, 0, length)
        }

        /// One new value node, as the `int` handle its caller names it by.
        fn push(tasks: &TasksApi, kind: TaskNodeKind) -> Result<i32, NativeError> {
            let handle = tasks
                .push(kind)
                .map_err(|error| NativeError::Message(error.to_string()))?;
            i32::try_from(handle.index())
                .map_err(|_| Api::refused("the build-task count is beyond what an `int` can name"))
        }

        /// One new terminal, refused with the plan's own reason.
        fn terminal(tasks: &TasksApi, terminal: TaskTerminal) -> Result<(), NativeError> {
            tasks
                .terminal(terminal)
                .map_err(|error| NativeError::Message(error.to_string()))
        }

        /// The `char[]` and `int[]` arguments at `position` and `position + 1`, as the strings the
        /// Java half packed into them.
        ///
        /// `Tasks.pack` writes one end offset per value, so a segment is the slice between two end
        /// offsets; both arrays are read here rather than trusted, because the pair crosses the
        /// boundary as plain data.
        fn packed_strings(
            host: &mut dyn NativeHost,
            args: &Args<'_>,
            position: usize,
        ) -> Result<Vec<String>, NativeError> {
            let packed = args.reference(position)?;
            let ends = host.array_i32(args.reference(position + 1)?)?;
            let mut out = Vec::new();
            let mut at = 0u32;
            for end in ends {
                let end = u32::try_from(end)
                    .map_err(|_| Api::refused("a packed string list has a negative end offset"))?;
                if end < at {
                    return Err(Api::refused("a packed string list is not in order"));
                }
                out.push(host.array_text(packed, at, end - at)?);
                at = end;
            }
            Ok(out)
        }

        /// One `int` argument as a node ID.
        fn task_id(args: &Args<'_>, position: usize) -> Result<TaskId, NativeError> {
            let value = args.i32(position)?;
            u32::try_from(value)
                .map(TaskId::new)
                .map_err(|_| Api::refused(format!("{value} is not a task handle")))
        }

        /// The digest algorithm an `int` argument names.
        fn digest_algorithm(kind: i32) -> Result<TaskDigestAlgorithm, NativeError> {
            match kind {
                0 => Ok(TaskDigestAlgorithm::Sha1),
                1 => Ok(TaskDigestAlgorithm::Sha256),
                _ => Err(Api::refused(format!("{kind} is not a digest algorithm"))),
            }
        }

        /// The fetch kind an `int` argument names.
        fn fetch_kind(kind: i32) -> Result<TaskFetchKind, NativeError> {
            match kind {
                0 => Ok(TaskFetchKind::Json),
                1 => Ok(TaskFetchKind::Jar),
                2 => Ok(TaskFetchKind::Text),
                _ => Err(Api::refused(format!("{kind} is not a fetch kind"))),
            }
        }

        /// The grammar a `MappingFormat` states.
        ///
        /// Revalidated here because a Java value crossed as data: the pair was checked where
        /// `Tasks.tinyV2` built it, and a script may have held it since. The messages keep the task
        /// API's spelling, as the rest of `jals.build`'s refusals do.
        fn mapping_format(
            kind: i32,
            from: String,
            to: String,
        ) -> Result<TaskMappingFormat, NativeError> {
            match kind {
                0 => Ok(TaskMappingFormat::Proguard),
                1 => {
                    if from.is_empty() || to.is_empty() {
                        return Err(Api::refused(
                            "Tasks.tinyV2 needs two namespace names, e.g. \
                             Tasks.tinyV2(\"official\", \"named\")",
                        ));
                    }
                    if from == to {
                        return Err(Api::refused(
                            "Tasks.tinyV2 names the two namespaces a remap translates between, \
                             so naming one twice renames nothing",
                        ));
                    }
                    Ok(TaskMappingFormat::TinyV2 { from, to })
                }
                _ => Err(Api::refused(format!("{kind} is not a mapping format"))),
            }
        }

        /// The publication intent a string argument names.
        fn publish_intent(intent: &str) -> Result<TaskPublishIntent, NativeError> {
            match intent {
                "compile" => Ok(TaskPublishIntent::Compile),
                "navigation" => Ok(TaskPublishIntent::Navigation),
                _ => Err(Api::refused(
                    "Tasks.publishTree needs an intent of `compile` (a consumer compiles this \
                     tree) or `navigation` (a consumer only reads it; the classpath defines these \
                     types)",
                )),
            }
        }

        /// A format that names the namespace pair a mapping-text node writes or reads through.
        ///
        /// The ProGuard-style grammar names none, and a node handed one would be guessing which
        /// two namespaces its answer lives in — the same refusal `TaskPlan::validate` states for a
        /// plan that arrives as data, phrased here the way the rest of `jals.build` phrases one.
        fn namespaces_required(
            format: TaskMappingFormat,
            operation: &str,
        ) -> Result<TaskMappingFormat, NativeError> {
            match format {
                TaskMappingFormat::TinyV2 { .. } => Ok(format),
                TaskMappingFormat::Proguard => Err(Api::refused(format!(
                    "{operation} needs the tiny v2 namespace pair, which the ProGuard-style \
                     grammar does not name: pass a format `Tasks.tinyV2(from, to)` built"
                ))),
            }
        }

        let mut package = NativePackage::new(PACKAGE, VERSION);
        for &(path, text) in SOURCES {
            package.source(path, text);
        }

        {
            let api = self.clone();
            package.bind(
                "jals/build/Text",
                "entryLength(I)I",
                move |_host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    results.set(0, NativeValue::I32(api.entry_length(args.i32(0)?)?));
                    Ok(())
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Text",
                "entryInto(I[C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.entry_into(host, args.i32(0)?, args.reference(1)?)
                },
            );
        }

        {
            let api = self.clone();
            package.bind(
                "jals/build/Project",
                "readSize([C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    results.set(0, NativeValue::I32(api.read_size(&text(host, &args, 0)?)?));
                    Ok(())
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Project",
                "readInto([C[B)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.read_into(host, args.reference(1)?)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Project",
                "readTextSize([C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    results.set(
                        0,
                        NativeValue::I32(api.read_text_size(&text(host, &args, 0)?)?),
                    );
                    Ok(())
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Project",
                "readTextInto([C[C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.entry_into(host, 0, args.reference(1)?)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Project",
                "exists0([C)Z",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let answer = api.exists(&text(host, &args, 0)?)?;
                    results.set(0, NativeValue::I32(i32::from(answer)));
                    Ok(())
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Project",
                "readDirCount([C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    results.set(
                        0,
                        NativeValue::I32(api.read_dir_count(&text(host, &args, 0)?)?),
                    );
                    Ok(())
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Project",
                "walkFilesCount([C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    results.set(
                        0,
                        NativeValue::I32(api.walk_files_count(&text(host, &args, 0)?)?),
                    );
                    Ok(())
                },
            );
        }

        {
            let api = self.clone();
            package.bind(
                "jals/build/Output",
                "writeSize([C[B)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let path = text(host, &args, 0)?;
                    results.set(
                        0,
                        NativeValue::I32(api.write_size(host, &path, args.reference(1)?)?),
                    );
                    Ok(())
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Output",
                "writeTextSize([C[C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let path = text(host, &args, 0)?;
                    let value = text(host, &args, 1)?;
                    results.set(0, NativeValue::I32(api.write_text_size(&path, &value)?));
                    Ok(())
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Output",
                "takeKey([C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.take_key(host, args.reference(0)?)
                },
            );
        }

        {
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "envSize([C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    results.set(0, NativeValue::I32(api.env_size(&text(host, &args, 0)?)));
                    Ok(())
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "feature0([C)Z",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let enabled = api.feature(&text(host, &args, 0)?);
                    results.set(0, NativeValue::I32(i32::from(enabled)));
                    Ok(())
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "featureCount()I",
                move |_host: &mut dyn NativeHost, _args: Args<'_>, mut results: Results<'_>| {
                    results.set(0, NativeValue::I32(api.feature_count()?));
                    Ok(())
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "rerunIfChanged0([C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.rerun_if_changed(&text(host, &args, 0)?)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "rerunIfEnvChanged0([C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.rerun_if_env_changed(&text(host, &args, 0)?)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "addSource0([C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.add_source(&text(host, &args, 0)?)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "addClasspath0([C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.add_classpath(&text(host, &args, 0)?)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "addJavacArg0([C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.add_javac_arg(&text(host, &args, 0)?)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "addJvmArg0([C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.add_jvm_arg(&text(host, &args, 0)?)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "setCompileEnv0([C[C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    let name = text(host, &args, 0)?;
                    let value = text(host, &args, 1)?;
                    api.set_compile_env(&name, &value)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "setRunEnv0([C[C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    let name = text(host, &args, 0)?;
                    let value = text(host, &args, 1)?;
                    api.set_run_env(&name, &value)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "warning0([C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.warning(&text(host, &args, 0)?)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "error0([C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    api.error(&text(host, &args, 0)?)
                },
            );
            let api = self.clone();
            package.bind(
                "jals/build/Build",
                "metadata0([C[C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    let key = text(host, &args, 0)?;
                    let value = text(host, &args, 1)?;
                    api.metadata(&key, &value)
                },
            );
        }

        {
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "httpsUrl0([C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let value = text(host, &args, 0)?;
                    let handle = push(&tasks, TaskNodeKind::HttpsUrl { value })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "projectJar0([C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let path = text(host, &args, 0)?;
                    let handle = push(&tasks, TaskNodeKind::ProjectJar { path })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "digest0([CI)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let value = text(host, &args, 0)?;
                    let algorithm = digest_algorithm(args.i32(1)?)?;
                    let handle = push(&tasks, TaskNodeKind::Digest { algorithm, value })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "bytes0(J)I",
                move |_host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let value = u64::try_from(args.i64(0)?)
                        .map_err(|_| Self::refused("Tasks.bytes requires a positive byte count"))?;
                    let handle = push(&tasks, TaskNodeKind::ByteCount { value })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "fetch0(IIII)I",
                move |_host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let kind = fetch_kind(args.i32(3)?)?;
                    let handle = push(
                        &tasks,
                        TaskNodeKind::Fetch {
                            kind,
                            url: task_id(&args, 0)?,
                            digest: task_id(&args, 1)?,
                            max_bytes: task_id(&args, 2)?,
                        },
                    )?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "jsonAt0(I[C[I)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let json = task_id(&args, 0)?;
                    let path = packed_strings(host, &args, 1)?;
                    let handle = push(&tasks, TaskNodeKind::JsonAt { json, path })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "jsonFindString0(I[C[I[C[C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let json = task_id(&args, 0)?;
                    let path = packed_strings(host, &args, 1)?;
                    let field = text(host, &args, 3)?;
                    let value = text(host, &args, 4)?;
                    let handle = push(
                        &tasks,
                        TaskNodeKind::JsonFindString {
                            json,
                            path,
                            field,
                            value,
                        },
                    )?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "jsonUrl0(I[C[I)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let json = task_id(&args, 0)?;
                    let path = packed_strings(host, &args, 1)?;
                    let handle = push(&tasks, TaskNodeKind::JsonUrl { json, path })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "jsonDigest0(I[C[II)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let json = task_id(&args, 0)?;
                    let path = packed_strings(host, &args, 1)?;
                    let algorithm = digest_algorithm(args.i32(3)?)?;
                    let handle = push(
                        &tasks,
                        TaskNodeKind::JsonDigest {
                            json,
                            path,
                            algorithm,
                        },
                    )?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "jsonU640(I[C[I)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let json = task_id(&args, 0)?;
                    let path = packed_strings(host, &args, 1)?;
                    let handle = push(&tasks, TaskNodeKind::JsonU64 { json, path })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "extractJava0(I[C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let jar = task_id(&args, 0)?;
                    let prefix = text(host, &args, 1)?;
                    let handle = push(&tasks, TaskNodeKind::ExtractJava { jar, prefix })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "nestedJar0(I[C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let jar = task_id(&args, 0)?;
                    let member = text(host, &args, 1)?;
                    let handle = push(&tasks, TaskNodeKind::NestedJar { jar, member })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "jarText0(I[C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let jar = task_id(&args, 0)?;
                    let member = text(host, &args, 1)?;
                    let handle = push(&tasks, TaskNodeKind::JarText { jar, member })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            package.bind(
                "jals/build/Tasks",
                "tinyV2Check([C[C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    let from = text(host, &args, 0)?;
                    let to = text(host, &args, 1)?;
                    mapping_format(1, from, to).map(|_| ())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "remapJar0(III[C[C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let jar = task_id(&args, 0)?;
                    let mappings = task_id(&args, 1)?;
                    let format =
                        mapping_format(args.i32(2)?, text(host, &args, 3)?, text(host, &args, 4)?)?;
                    let handle = push(
                        &tasks,
                        TaskNodeKind::RemapJar {
                            jar,
                            mappings,
                            format,
                            direction: TaskRemapDirection::Deobfuscate,
                            hierarchy: Vec::new(),
                        },
                    )?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "mergeJars0(II)I",
                move |_host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let base = task_id(&args, 0)?;
                    let overlay = task_id(&args, 1)?;
                    let handle = push(&tasks, TaskNodeKind::MergeJars { base, overlay })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "composeMappings0(III[C[C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let official = task_id(&args, 0)?;
                    let intermediary = task_id(&args, 1)?;
                    let format =
                        mapping_format(args.i32(2)?, text(host, &args, 3)?, text(host, &args, 4)?)?;
                    let format = namespaces_required(format, "Tasks.composeMappings")?;
                    let handle = push(
                        &tasks,
                        TaskNodeKind::ComposeMappings {
                            official,
                            intermediary,
                            format,
                        },
                    )?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "copyMappings0(I[C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let mappings = task_id(&args, 0)?;
                    let copies = text(host, &args, 1)?;
                    let handle = push(&tasks, TaskNodeKind::CopyMappings { mappings, copies })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "resolveReferences0([CII[C[C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let requests = text(host, &args, 0)?;
                    let mappings = task_id(&args, 1)?;
                    let format =
                        mapping_format(args.i32(2)?, text(host, &args, 3)?, text(host, &args, 4)?)?;
                    let format = namespaces_required(format, "Tasks.resolveReferences")?;
                    let handle = push(
                        &tasks,
                        TaskNodeKind::ResolveReferences {
                            requests,
                            mappings,
                            format,
                        },
                    )?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "jsonFromLines0(I)I",
                move |_host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let records = task_id(&args, 0)?;
                    let handle = push(&tasks, TaskNodeKind::JsonFromLines { records })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "decompileJava0(I[C)I",
                move |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                    let jar = task_id(&args, 0)?;
                    let prefix = text(host, &args, 1)?;
                    let handle = push(&tasks, TaskNodeKind::DecompileJava { jar, prefix })?;
                    results.set(0, NativeValue::I32(handle));
                    Ok(())
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "addClasspath0(I)V",
                move |_host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    terminal(
                        &tasks,
                        TaskTerminal::AddClasspath {
                            jar: task_id(&args, 0)?,
                        },
                    )
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "addNestedClasspath0(I)V",
                move |_host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    terminal(
                        &tasks,
                        TaskTerminal::AddNestedClasspath {
                            jar: task_id(&args, 0)?,
                        },
                    )
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "publishTree0([CI[C[C)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    let owner = text(host, &args, 0)?;
                    let tree = task_id(&args, 1)?;
                    let destination = text(host, &args, 2)?;
                    let intent = publish_intent(&text(host, &args, 3)?)?;
                    terminal(
                        &tasks,
                        TaskTerminal::PublishTree {
                            owner,
                            tree,
                            destination,
                            mode: TaskPublishMode::ReplaceRoot,
                            intent,
                        },
                    )
                },
            );
            let tasks = self.tasks.clone();
            package.bind(
                "jals/build/Tasks",
                "publishText0([CI)V",
                move |host: &mut dyn NativeHost, args: Args<'_>, _results: Results<'_>| {
                    let path = text(host, &args, 0)?;
                    let text = task_id(&args, 1)?;
                    terminal(&tasks, TaskTerminal::PublishText { path, text })
                },
            );
        }

        package
    }

    /// Scratch entry `index`, as a length in UTF-16 code units.
    fn entry_length(&self, index: i32) -> Result<i32, NativeError> {
        let scratch = self.scratch.borrow();
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        let entry = scratch
            .listing
            .get(index)
            .ok_or_else(|| NativeError::OutOfBounds {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                len: u32::try_from(scratch.listing.len()).unwrap_or(u32::MAX),
            })?;
        Ok(Self::code_units(entry))
    }

    /// Copy scratch entry `index` into `out`, which has to be its exact length.
    fn entry_into(
        &self,
        host: &mut dyn NativeHost,
        index: i32,
        out: RefSlot,
    ) -> Result<(), NativeError> {
        let entry = {
            let scratch = self.scratch.borrow();
            let index = usize::try_from(index).unwrap_or(usize::MAX);
            scratch
                .listing
                .get(index)
                .cloned()
                .ok_or_else(|| NativeError::OutOfBounds {
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    len: u32::try_from(scratch.listing.len()).unwrap_or(u32::MAX),
                })?
        };
        let units: Vec<u16> = entry.encode_utf16().collect();
        let length = usize::try_from(host.array_len(out)?).unwrap_or(usize::MAX);
        if length != units.len() {
            return Err(Self::refused(format!(
                "the host holds a {} code-unit string and the buffer has {length}",
                units.len()
            )));
        }
        for (position, unit) in units.iter().enumerate() {
            host.array_set(
                out,
                u32::try_from(position).unwrap_or(u32::MAX),
                NativeValue::I32(i32::from(*unit)),
            )?;
        }
        Ok(())
    }

    /// Read the file at `path` and hold its bytes for the fill that follows.
    fn read_size(&self, path: &str) -> Result<i32, NativeError> {
        let key = Self::parse_file(path, "Project.read", &self.limits)?;
        let file = self
            .view
            .file(&key)
            .map_err(|error| Self::refused(format!("Project.read `{key}`: {error}")))?;
        Self::check_array_len(
            file.bytes().len(),
            self.limits.max_array_size,
            "Project.read",
        )?;
        let length = i32::try_from(file.bytes().len()).unwrap_or(i32::MAX);
        self.scratch.borrow_mut().bytes = file.bytes().to_vec();
        Ok(length)
    }

    /// Copy the bytes the last read held into `out`.
    fn read_into(&self, host: &mut dyn NativeHost, out: RefSlot) -> Result<(), NativeError> {
        let scratch = self.scratch.borrow();
        let length = usize::try_from(host.array_len(out)?).unwrap_or(usize::MAX);
        if length != scratch.bytes.len() {
            return Err(Self::refused(format!(
                "the host holds a {}-byte file and the buffer has {length}",
                scratch.bytes.len()
            )));
        }
        for (position, byte) in scratch.bytes.iter().enumerate() {
            host.array_set(
                out,
                u32::try_from(position).unwrap_or(u32::MAX),
                NativeValue::I32(i32::from(*byte)),
            )?;
        }
        Ok(())
    }

    /// Read the text of the file at `path` and hold it for the fill that follows.
    fn read_text_size(&self, path: &str) -> Result<i32, NativeError> {
        let key = Self::parse_file(path, "Project.readText", &self.limits)?;
        let text = self
            .view
            .file_text(&key)
            .map_err(|error| Self::refused(format!("Project.readText `{key}`: {error}")))?;
        Self::check_string_len(text, self.limits.max_string_size, "Project.readText")?;
        let length = Self::code_units(text);
        self.scratch.borrow_mut().listing = alloc::vec![text.to_owned()];
        Ok(length)
    }

    /// Whether a project-relative file or directory exists at `path`.
    fn exists(&self, path: &str) -> Result<bool, NativeError> {
        let path = Self::parse_relative(path, "Project.exists", &self.limits)?;
        if path.is_root() {
            return Ok(true);
        }
        let file = FileKey::new(path.clone()).map_err(|error| {
            Self::refused(format!("Project.exists rejected path `{path}`: {error:?}"))
        })?;
        Ok(self.view.tree().file(&file).is_some()
            || self.view.tree().directory(&DirKey::new(path)).is_some())
    }

    /// List the direct children of `path` and hold them for the entry fills that follow.
    fn read_dir_count(&self, path: &str) -> Result<i32, NativeError> {
        let key = Self::parse_dir(path, "Project.readDir", &self.limits)?;
        self.view
            .directory(&key)
            .map_err(|error| Self::refused(format!("Project.readDir `{key}`: {error}")))?;
        let entries: Vec<String> = self
            .view
            .tree()
            .children(&key)
            .map(|entry| match entry {
                EntryRef::Directory(dir) => dir.to_string(),
                EntryRef::File(file) => file.key().to_string(),
            })
            .collect();
        Self::check_entries(&entries, "Project.readDir", &self.limits)?;
        let count = i32::try_from(entries.len()).unwrap_or(i32::MAX);
        self.scratch.borrow_mut().listing = entries;
        Ok(count)
    }

    /// List every file below `path` and hold them for the entry fills that follow.
    fn walk_files_count(&self, path: &str) -> Result<i32, NativeError> {
        let key = Self::parse_dir(path, "Project.walkFiles", &self.limits)?;
        self.view
            .directory(&key)
            .map_err(|error| Self::refused(format!("Project.walkFiles `{key}`: {error}")))?;
        let files: Vec<String> = self
            .view
            .tree()
            .files_under(&key)
            .map(|file| file.key().to_string())
            .collect();
        Self::check_entries(&files, "Project.walkFiles", &self.limits)?;
        let count = i32::try_from(files.len()).unwrap_or(i32::MAX);
        self.scratch.borrow_mut().listing = files;
        Ok(count)
    }

    /// Buffer the `byte[]` in `bytes` at `path`, hold the canonical key, and answer its length.
    fn write_size(
        &self,
        host: &mut dyn NativeHost,
        path: &str,
        bytes: RefSlot,
    ) -> Result<i32, NativeError> {
        let values = host.array_i32(bytes)?;
        let mut output = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            let byte = u8::try_from(*value).map_err(|_| {
                Self::refused(format!(
                    "Output.write byte {index} for `{path}` is outside 0..=255"
                ))
            })?;
            output.push(byte);
        }
        self.hold_key(path, output)
    }

    /// Buffer the UTF-8 bytes of `text` at `path`, hold the canonical key, and answer its length.
    fn write_text_size(&self, path: &str, text: &str) -> Result<i32, NativeError> {
        self.hold_key(path, text.as_bytes().to_vec())
    }

    /// Copy the held key into `out`, which has to be its exact length.
    fn take_key(&self, host: &mut dyn NativeHost, out: RefSlot) -> Result<(), NativeError> {
        let key = {
            let scratch = self.scratch.borrow();
            scratch
                .key
                .clone()
                .ok_or_else(|| Self::refused("the host holds no output key to copy".to_owned()))?
        };
        let units: Vec<u16> = key.encode_utf16().collect();
        let length = usize::try_from(host.array_len(out)?).unwrap_or(usize::MAX);
        if length != units.len() {
            return Err(Self::refused(format!(
                "the host holds a {} code-unit key and the buffer has {length}",
                units.len()
            )));
        }
        for (position, unit) in units.iter().enumerate() {
            host.array_set(
                out,
                u32::try_from(position).unwrap_or(u32::MAX),
                NativeValue::I32(i32::from(*unit)),
            )?;
        }
        Ok(())
    }

    /// Buffer `bytes` below the output root and hold the canonical key, answering its length.
    fn hold_key(&self, path: &str, bytes: Vec<u8>) -> Result<i32, NativeError> {
        let key = self.write_output(path, bytes)?;
        let length = Self::code_units(&key);
        self.scratch.borrow_mut().key = Some(key);
        Ok(length)
    }

    /// The body every output write shares: validate the path, bound the bytes, buffer them, and
    /// answer the canonical key.
    fn write_output(&self, path: &str, bytes: Vec<u8>) -> Result<String, NativeError> {
        let relative = Self::parse_relative(path, "Output.write", &self.limits)?;
        // An empty path is the root relative path, and committing the root as a *file* would make
        // every later write fail against its own ancestor. `decode_state` already refuses that
        // key; refuse it on the write side too.
        if relative.is_root() {
            return Err(Self::refused(
                "Output.write rejected an empty path: expected a path below the output root",
            ));
        }
        let output_root = DirKey::parse(BUILD_SCRIPT_OUTPUT_ROOT)
            .map_err(|error| Self::refused(format!("invalid internal output root: {error:?}")))?;
        let key = output_root.file_at(&relative).map_err(|error| {
            Self::refused(format!(
                "Output.write rejected output path `{path}`: {error:?}"
            ))
        })?;

        if self
            .view
            .tree()
            .directory(&DirKey::new(key.path().clone()))
            .is_some()
        {
            return Err(Self::refused(format!(
                "Output.write expected a file but `{key}` is a directory"
            )));
        }
        for ancestor in key.parent().ancestors() {
            if ancestor == DirKey::ROOT {
                continue;
            }
            if let Ok(file) = FileKey::new(ancestor.path().clone())
                && self.view.tree().file(&file).is_some()
            {
                return Err(Self::refused(format!(
                    "Output.write cannot create `{key}` because `{file}` is a file"
                )));
            }
        }

        let mut pending = self
            .pending
            .try_borrow_mut()
            .map_err(|_| Self::refused("reentrant Output.write call"))?;
        if bytes.len() > pending.limits.max_output_file_size {
            return Err(Self::refused(format!(
                "Output.write `{path}` has {} bytes, exceeding the per-file limit of {}",
                bytes.len(),
                pending.limits.max_output_file_size
            )));
        }
        if !pending.generated.contains_key(&key)
            && pending.generated.len() == pending.limits.max_output_files
        {
            return Err(Self::refused(format!(
                "Output.write exceeds the generated-file limit of {}",
                pending.limits.max_output_files
            )));
        }
        if let Some(conflict) = pending.generated.keys().find(|existing| {
            *existing != &key
                && (existing.path().starts_with(key.path())
                    || key.path().starts_with(existing.path()))
        }) {
            return Err(Self::refused(format!(
                "Output.write path `{key}` conflicts with generated file `{conflict}`"
            )));
        }
        let previous_len = pending.generated.get(&key).map_or(0, Vec::len);
        let total = pending
            .total_output_bytes
            .checked_sub(previous_len)
            .and_then(|size| size.checked_add(bytes.len()))
            .ok_or_else(|| Self::refused("Output.write total byte count overflowed"))?;
        if total > pending.limits.max_total_output_size {
            return Err(Self::refused(format!(
                "Output.write would produce {total} total bytes, exceeding the limit of {}",
                pending.limits.max_total_output_size
            )));
        }
        pending.total_output_bytes = total;
        pending.generated.insert(key.clone(), bytes);
        Ok(key.to_string())
    }

    /// Answer the length of the environment value at `name`, or `-1` when it is absent.
    fn env_size(&self, name: &str) -> i32 {
        self.environment
            .get(name)
            .map(str::to_owned)
            .map_or(-1, |value| {
                let length = Self::code_units(&value);
                self.scratch.borrow_mut().listing = alloc::vec![value];
                length
            })
    }

    /// Whether the project's resolved feature set contains `name`.
    fn feature(&self, name: &str) -> bool {
        self.environment.has_feature(name)
    }

    /// List the project's enabled features and hold them for the entry fills that follow.
    fn feature_count(&self) -> Result<i32, NativeError> {
        let features: Vec<String> = self.environment.features().map(str::to_owned).collect();
        Self::check_entries(&features, "build.features", &self.limits)?;
        let count = i32::try_from(features.len()).unwrap_or(i32::MAX);
        self.scratch.borrow_mut().listing = features;
        Ok(count)
    }

    /// Track one project file for cache invalidation.
    fn rerun_if_changed(&self, path: &str) -> Result<(), NativeError> {
        let key = Self::project_file(&self.view, path, "Build.rerunIfChanged", &self.limits)?;
        if Self::is_managed_build_path(&key) {
            return Err(Self::refused(format!(
                "Build.rerunIfChanged rejected managed build output `{key}`"
            )));
        }
        let mut pending = self
            .pending
            .try_borrow_mut()
            .map_err(|_| Self::refused("reentrant Build.rerunIfChanged call"))?;
        let limit = pending.limits.max_array_size;
        Self::insert_host_file(&mut pending.rerun_files, key, limit, "Build.rerunIfChanged")
    }

    /// Track one supplied environment value for cache invalidation.
    fn rerun_if_env_changed(&self, name: &str) -> Result<(), NativeError> {
        Self::validate_env_name(name, "build.rerun_if_env_changed")?;
        let mut pending = self
            .pending
            .try_borrow_mut()
            .map_err(|_| Self::refused("reentrant build.rerun_if_env_changed call"))?;
        if !pending.rerun_env.contains(name) {
            Self::check_host_collection(
                pending.rerun_env.len(),
                pending.limits.max_array_size,
                "build.rerun_if_env_changed",
            )?;
            pending.rerun_env.insert(name.to_owned());
        }
        Ok(())
    }

    /// Add a project file or a generated file's key to the later source set.
    fn add_source(&self, path: &str) -> Result<(), NativeError> {
        let key = Self::project_file(&self.view, path, "build.add_source", &self.limits)?;
        let mut pending = self
            .pending
            .try_borrow_mut()
            .map_err(|_| Self::refused("reentrant build.add_source call"))?;
        let limit = pending.limits.max_array_size;
        Self::insert_host_file(
            &mut pending.generated_sources,
            key,
            limit,
            "build.add_source",
        )
    }

    /// Add a project file or a generated file's key to the later classpath.
    fn add_classpath(&self, path: &str) -> Result<(), NativeError> {
        let key = Self::project_file(&self.view, path, "build.add_classpath", &self.limits)?;
        let mut pending = self
            .pending
            .try_borrow_mut()
            .map_err(|_| Self::refused("reentrant build.add_classpath call"))?;
        let limit = pending.limits.max_array_size;
        Self::insert_host_file(
            &mut pending.additional_classpath,
            key,
            limit,
            "build.add_classpath",
        )
    }

    /// Append one compiler argument.
    fn add_javac_arg(&self, arg: &str) -> Result<(), NativeError> {
        let mut pending = self
            .pending
            .try_borrow_mut()
            .map_err(|_| Self::refused("reentrant build.add_javac_arg call"))?;
        Self::push_argument(&mut pending, arg.to_owned(), true)
    }

    /// Append one JVM argument.
    fn add_jvm_arg(&self, arg: &str) -> Result<(), NativeError> {
        let mut pending = self
            .pending
            .try_borrow_mut()
            .map_err(|_| Self::refused("reentrant build.add_jvm_arg call"))?;
        Self::push_argument(&mut pending, arg.to_owned(), false)
    }

    /// Add one compiler environment entry, replacing an earlier value for the name.
    fn set_compile_env(&self, name: &str, value: &str) -> Result<(), NativeError> {
        self.set_environment(name, value, true)
    }

    /// Add one runtime environment entry, replacing an earlier value for the name.
    fn set_run_env(&self, name: &str, value: &str) -> Result<(), NativeError> {
        self.set_environment(name, value, false)
    }

    /// The body both environment setters share.
    fn set_environment(&self, name: &str, value: &str, compile: bool) -> Result<(), NativeError> {
        let operation = if compile {
            "build.set_compile_env"
        } else {
            "build.set_run_env"
        };
        Self::validate_env_name(name, operation)?;
        let mut pending = self
            .pending
            .try_borrow_mut()
            .map_err(|_| Self::refused(format!("reentrant {operation} call")))?;
        let limit = pending.limits.max_map_size;
        let previous = if compile {
            pending.compile_env.get(name)
        } else {
            pending.run_env.get(name)
        };
        let previous_bytes = previous.map_or(0, |value| name.len() + value.len());
        let environment_len = if compile {
            pending.compile_env.len()
        } else {
            pending.run_env.len()
        };
        if previous.is_none() {
            Self::check_host_collection(environment_len, limit, operation)?;
        }
        Self::update_host_directive_bytes(
            &mut pending,
            previous_bytes,
            name.len() + value.len(),
            operation,
        )?;
        if compile {
            pending
                .compile_env
                .insert(name.to_owned(), value.to_owned());
        } else {
            pending.run_env.insert(name.to_owned(), value.to_owned());
        }
        Ok(())
    }

    /// Report a non-fatal diagnostic.
    fn warning(&self, message: &str) -> Result<(), NativeError> {
        self.push_diagnostic(
            BuildScriptDiagnostic::warning(message.to_owned()),
            "build.warning",
        )
    }

    /// Report a fatal diagnostic.
    fn error(&self, message: &str) -> Result<(), NativeError> {
        self.push_diagnostic(
            BuildScriptDiagnostic::error(message.to_owned()),
            "build.error",
        )
    }

    /// The body both diagnostic reporters share.
    fn push_diagnostic(
        &self,
        diagnostic: BuildScriptDiagnostic,
        operation: &str,
    ) -> Result<(), NativeError> {
        let mut pending = self
            .pending
            .try_borrow_mut()
            .map_err(|_| Self::refused(format!("reentrant {operation} call")))?;
        Self::check_host_collection(
            pending.diagnostics.len(),
            pending.limits.max_array_size,
            operation,
        )?;
        Self::update_host_directive_bytes(&mut pending, 0, diagnostic.message().len(), operation)?;
        pending.diagnostics.push(diagnostic);
        Ok(())
    }

    /// Record deterministic host-readable metadata.
    fn metadata(&self, key: &str, value: &str) -> Result<(), NativeError> {
        if key.is_empty() {
            return Err(Self::refused("Build.metadata rejected an empty key"));
        }
        let mut pending = self
            .pending
            .try_borrow_mut()
            .map_err(|_| Self::refused("reentrant Build.metadata call"))?;
        let previous = pending.metadata.get(key);
        let previous_bytes = previous.map_or(0, |value| key.len() + value.len());
        if previous.is_none() {
            Self::check_host_collection(
                pending.metadata.len(),
                pending.limits.max_map_size,
                "Build.metadata",
            )?;
        }
        Self::update_host_directive_bytes(
            &mut pending,
            previous_bytes,
            key.len() + value.len(),
            "Build.metadata",
        )?;
        pending.metadata.insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    /// `path`'s depth in segments, the way the path limits count it.
    fn path_depth(path: &str) -> usize {
        if path.is_empty() {
            0
        } else {
            path.bytes()
                .filter(|byte| *byte == b'/')
                .count()
                .saturating_add(1)
        }
    }

    /// Refuse `path` when it exceeds the byte or segment limit.
    fn check_path_limits(
        path: &str,
        operation: &str,
        limits: &BuildScriptLimits,
    ) -> Result<(), NativeError> {
        if path.len() > limits.max_path_bytes {
            return Err(Self::refused(format!(
                "{operation} rejected a {}-byte path, exceeding the limit of {}",
                path.len(),
                limits.max_path_bytes
            )));
        }
        let depth = Self::path_depth(path);
        if depth > limits.max_path_depth {
            return Err(Self::refused(format!(
                "{operation} rejected a {depth}-segment path, exceeding the limit of {}",
                limits.max_path_depth
            )));
        }
        Ok(())
    }

    /// `path` as a portable relative path.
    fn parse_relative(
        path: &str,
        operation: &str,
        limits: &BuildScriptLimits,
    ) -> Result<RelativePath, NativeError> {
        Self::check_path_limits(path, operation, limits)?;
        RelativePath::parse(path).map_err(|error| {
            Self::refused(format!("{operation} rejected path `{path}`: {error:?}"))
        })
    }

    /// `path` as a portable file key.
    fn parse_file(
        path: &str,
        operation: &str,
        limits: &BuildScriptLimits,
    ) -> Result<FileKey, NativeError> {
        FileKey::new(Self::parse_relative(path, operation, limits)?).map_err(|error| {
            Self::refused(format!(
                "{operation} rejected file path `{path}`: {error:?}"
            ))
        })
    }

    /// `path` as a portable directory key.
    fn parse_dir(
        path: &str,
        operation: &str,
        limits: &BuildScriptLimits,
    ) -> Result<DirKey, NativeError> {
        Ok(DirKey::new(Self::parse_relative(path, operation, limits)?))
    }

    /// `path` as a file key that is not a directory in `view`.
    fn project_file(
        view: &ProjectView,
        path: &str,
        operation: &str,
        limits: &BuildScriptLimits,
    ) -> Result<FileKey, NativeError> {
        let key = Self::parse_file(path, operation, limits)?;
        if view
            .tree()
            .directory(&DirKey::new(key.path().clone()))
            .is_some()
        {
            return Err(Self::refused(format!(
                "{operation} expected a file but `{key}` is a directory"
            )));
        }
        Ok(key)
    }

    /// Whether `key` is inside the managed build tree, which generated files cannot track.
    fn is_managed_build_path(key: &FileKey) -> bool {
        DirKey::parse(BUILD_ARTIFACT_ROOT).is_ok_and(|root| key.path().starts_with(root.path()))
    }

    /// Refuse a list longer than the script's array limit.
    fn check_array_len(len: usize, limit: usize, operation: &str) -> Result<(), NativeError> {
        if len > limit {
            Err(Self::refused(format!(
                "{operation} produced {len} items, exceeding the limit of {limit}"
            )))
        } else {
            Ok(())
        }
    }

    /// Refuse a string that outgrew the script's string limit.
    fn check_string_len(value: &str, limit: usize, operation: &str) -> Result<(), NativeError> {
        if value.len() > limit {
            Err(Self::refused(format!(
                "{operation} produced a {}-byte string, exceeding the limit of {limit}",
                value.len()
            )))
        } else {
            Ok(())
        }
    }

    /// A string list's length and every entry, bounded by the host-collection limit.
    fn check_entries(
        entries: &[String],
        operation: &str,
        limits: &BuildScriptLimits,
    ) -> Result<(), NativeError> {
        Self::check_array_len(entries.len(), limits.max_array_size, operation)?;
        for entry in entries {
            Self::check_string_len(entry, limits.max_string_size, operation)?;
        }
        Ok(())
    }

    /// Refuse a collection that is already at its limit.
    fn check_host_collection(len: usize, limit: usize, operation: &str) -> Result<(), NativeError> {
        if len == limit {
            Err(Self::refused(format!(
                "{operation} exceeds the collection limit of {limit}"
            )))
        } else {
            Ok(())
        }
    }

    /// Insert `key` into a host-owned set, bounded.
    fn insert_host_file(
        set: &mut BTreeSet<FileKey>,
        key: FileKey,
        limit: usize,
        operation: &str,
    ) -> Result<(), NativeError> {
        if set.contains(&key) {
            return Ok(());
        }
        // Validate before making a failed call observable, so a refusal leaves nothing behind.
        Self::check_host_collection(set.len(), limit, operation)?;
        set.insert(key);
        Ok(())
    }

    /// Move one directive's byte accounting from `previous` to `next`, bounded.
    fn update_host_directive_bytes(
        pending: &mut PendingOutput,
        previous: usize,
        next: usize,
        operation: &str,
    ) -> Result<(), NativeError> {
        let total = pending
            .host_directive_bytes
            .checked_sub(previous)
            .and_then(|bytes| bytes.checked_add(next))
            .ok_or_else(|| Self::refused(format!("{operation} directive byte count overflowed")))?;
        if total > pending.limits.max_host_directive_bytes {
            return Err(Self::refused(format!(
                "{operation} would produce {total} host-directive bytes, exceeding the limit of {}",
                pending.limits.max_host_directive_bytes
            )));
        }
        pending.host_directive_bytes = total;
        Ok(())
    }

    /// Refuse an environment name the platform could not set.
    fn validate_env_name(name: &str, operation: &str) -> Result<(), NativeError> {
        if name.is_empty() || name.bytes().any(|byte| byte == b'=' || byte == b'\0') {
            Err(Self::refused(format!(
                "{operation} rejected environment name `{name}`"
            )))
        } else {
            Ok(())
        }
    }

    /// Append one argument to the compiler's or the runtime's argument list, bounded.
    fn push_argument(
        pending: &mut PendingOutput,
        argument: String,
        javac: bool,
    ) -> Result<(), NativeError> {
        let (len, operation) = if javac {
            (pending.javac_args.len(), "build.add_javac_arg")
        } else {
            (pending.jvm_args.len(), "build.add_jvm_arg")
        };
        Self::check_host_collection(len, pending.limits.max_array_size, operation)?;
        Self::update_host_directive_bytes(pending, 0, argument.len(), operation)?;
        if javac {
            pending.javac_args.push(argument);
        } else {
            pending.jvm_args.push(argument);
        }
        Ok(())
    }

    /// `value`'s length in UTF-16 code units, which is what a Java `char[]` counts.
    fn code_units(value: &str) -> i32 {
        i32::try_from(value.encode_utf16().count()).unwrap_or(i32::MAX)
    }

    /// A refusal the engine turns into a trap, which stops the run where the call was made.
    fn refused(message: impl Into<String>) -> NativeError {
        NativeError::Message(message.into())
    }
}

/// The engine's failures, resolved from the compile's file numbering into what a script author can
/// act on.
///
/// A failure in the script carries a *position in the script*, which is what a host renders as
/// `path:line:column`; a failure anywhere else — the platform's Java, the API package's — is a
/// defect in what shipped rather than in the script, so it keeps its logical path in front of the
/// message and no position: a host function's failure is never attributed to the script's
/// coordinates.
struct Failure;

impl Failure {
    /// The line and character column of byte `offset` in `text`, both 1-based.
    fn position_in(text: &str, offset: usize) -> BuildScriptPosition {
        let mut line = 1u32;
        let mut line_start = 0usize;
        for (index, byte) in text.bytes().enumerate() {
            if index >= offset {
                break;
            }
            if byte == b'\n' {
                line = line.saturating_add(1);
                line_start = index + 1;
            }
        }
        let column = text
            .get(line_start..offset)
            .map_or(1, |prefix| prefix.chars().count() + 1);
        BuildScriptPosition {
            line,
            column: u32::try_from(column).unwrap_or(u32::MAX),
        }
    }

    /// A message and position for a failure at `location`, if the compile reported one.
    fn situated(
        message: String,
        location: Option<(FileId, Range<usize>)>,
        files: &[Input],
    ) -> (Option<BuildScriptPosition>, String) {
        let Some((file, range)) = location else {
            return (None, message);
        };
        let Some(input) = files.get(usize::try_from(file.0).unwrap_or(usize::MAX)) else {
            return (None, message);
        };
        let position = Self::position_in(&input.text, range.start);
        if input.script {
            (Some(position), message)
        } else {
            let path = &input.path;
            let line = position.line;
            let column = position.column;
            (None, format!("{path}:{line}:{column}: {message}"))
        }
    }

    /// A compile failure, as a `build.java` author should read it.
    fn compile(error: &WasmError, files: &[Input], script_key: &FileKey) -> BuildScriptError {
        let (position, message) = Self::situated(error.to_string(), error.location(), files);
        BuildScriptError::Compile {
            script: script_key.clone(),
            position,
            message,
        }
    }

    /// A run failure, as a `build.java` author should read it.
    fn run(error: &WasmRunError, files: &[Input], script_key: &FileKey) -> BuildScriptError {
        let message = match error {
            // The entry-point convention is the one thing a script's author has to know and the
            // engine cannot state in a signature, so its absence says what it is; everything else
            // reports the engine's own answer.
            WasmRunError::NoSuchExport { name, available } if name == MAIN => {
                let exports = if available.is_empty() {
                    "it exports no functions at all".to_owned()
                } else {
                    format!("it exports {}", available.join(", "))
                };
                format!(
                    "the script declares no `static` method named `main`, which is a build \
                     script's entry point (`public static void main()`); {exports}"
                )
            }
            _ => error.to_string(),
        };
        let (position, message) = Self::situated(message, error.location(), files);
        BuildScriptError::Execute {
            script: script_key.clone(),
            position,
            message,
        }
    }
}

/// The engine's own tests: a real script end to end, where a failure points, and the budget.
///
/// The `native` gate is only for `MemoryStorage` — the engine itself is independent of it, and an
/// in-memory storage is the shortest honest host for a script that reads and writes files.
#[cfg(all(test, feature = "native"))]
mod tests {
    use alloc::collections::BTreeSet;
    use alloc::string::String;
    use alloc::vec::Vec;

    use jals_config::{BuildScript, Manifest};
    use jals_exec::block_on_inline;
    use jals_storage::{CodeTree, Entry, FileKey, MemoryStorage};

    use super::super::{
        BUILD_SCRIPT_OUTPUT_ROOT, BuildScriptCacheScope, BuildScriptEnvironment, BuildScriptError,
        BuildScriptLimits, PreparedBuildScript, prepare_build_script,
    };
    use crate::task::{
        TaskDigestAlgorithm, TaskFetchKind, TaskId, TaskMappingFormat, TaskNode, TaskNodeKind,
        TaskPlan, TaskPublishIntent, TaskPublishMode, TaskRemapDirection, TaskTerminal,
    };

    /// One project file, as an entry for a memory storage.
    fn file(path: &str, text: &str) -> Entry {
        Entry::File(FileKey::parse(path).unwrap(), text.as_bytes().to_vec())
    }

    /// An in-memory project whose `build.java` is `script`.
    fn storage(script: &str, extra: impl IntoIterator<Item = Entry>) -> MemoryStorage {
        MemoryStorage::memory(
            CodeTree::new(core::iter::once(file("build.java", script)).chain(extra)).unwrap(),
        )
    }

    /// The manifest that selects a Java script.
    fn manifest() -> Manifest {
        let mut manifest = Manifest::default();
        manifest.build.script = Some(BuildScript::Java {
            file: "build.java".into(),
        });
        manifest
    }

    /// Prepare the configured script, as a host about to publish it or report it would.
    async fn prepare(
        storage: &MemoryStorage,
        environment: &BuildScriptEnvironment,
        limits: &BuildScriptLimits,
    ) -> Result<PreparedBuildScript, BuildScriptError> {
        prepare_build_script(
            &storage.view(),
            storage.artifacts(),
            BuildScriptCacheScope::ROOT,
            &manifest(),
            environment,
            limits,
        )
        .await
        .map(Option::unwrap)
    }

    /// A project key below the output root, as `Output.write` returns it.
    fn output(path: &str) -> FileKey {
        FileKey::parse(&format!("{BUILD_SCRIPT_OUTPUT_ROOT}/{path}")).unwrap()
    }

    /// The 1-based line byte `offset` falls on.
    fn line_of(source: &str, offset: usize) -> u32 {
        u32::try_from(source[..offset].matches('\n').count() + 1).unwrap()
    }

    /// A script that exercises every non-task call the `jals.build` package publishes.
    const SURFACE: &str = r#"
import jals.build.Build;
import jals.build.Output;
import jals.build.Project;

class build {
    public static void main() {
        String side = "server";
        if (Build.feature("client")) {
            side = "client";
        }
        Output.writeText("side.txt", side);
        Output.writeText("text/note.txt", Project.readText("note.txt"));
        byte[] raw = Project.read("note.txt");
        if (raw.length != 8) {
            Build.error("the byte read is wrong");
        }
        byte[] data = new byte[3];
        data[0] = 1;
        data[1] = 2;
        data[2] = 3;
        Output.write("bytes.bin", data);
        String[] children = Project.readDir("src");
        String[] walked = Project.walkFiles("src");
        String[] features = Build.features();
        if (children.length != 2 || walked.length != 2 || features.length != 1) {
            Build.error("a listing is wrong");
        }
        if (!Project.exists("src") || Project.exists("missing")) {
            Build.error("existence is wrong");
        }
        if (Build.env("VALUE").length() != 5) {
            Build.error("the environment value is wrong");
        }
        if (Build.env("ABSENT") != null) {
            Build.error("an absent variable is not null");
        }
        Build.metadata("side", side);
        Build.warning("a warning");
        Build.addSource("source.java");
        Build.addClasspath("gen/classes.jar");
        Build.addJavacArg("-Xlint");
        Build.addJvmArg("-Xmx1g");
        Build.setCompileEnv("LANG", "C");
        Build.setRunEnv("MODE", "test");
        Build.rerunIfChanged("note.txt");
        Build.rerunIfEnvChanged("VALUE");
    }
}
"#;

    #[test]
    fn runs_a_script_that_reads_and_writes_the_whole_surface() {
        block_on_inline(async {
            let storage = storage(
                SURFACE,
                [
                    file("note.txt", "the note"),
                    file("src/A.txt", "A"),
                    file("src/nested/B.txt", "B"),
                ],
            );
            let mut environment =
                BuildScriptEnvironment::new().with_features(BTreeSet::from(["client".to_owned()]));
            environment.insert("VALUE", "hello");

            let prepared = prepare(&storage, &environment, &BuildScriptLimits::default())
                .await
                .expect("the script is well-formed and every call is inside its limits");

            assert_eq!(
                prepared
                    .file_bytes(&storage.view(), &output("side.txt"))
                    .unwrap(),
                b"client"
            );
            assert_eq!(
                prepared
                    .file_bytes(&storage.view(), &output("text/note.txt"))
                    .unwrap(),
                b"the note"
            );
            assert_eq!(
                prepared
                    .file_bytes(&storage.view(), &output("bytes.bin"))
                    .unwrap(),
                [1, 2, 3]
            );

            let emitted = prepared.output(storage.revision());
            assert_eq!(emitted.metadata.get("side").unwrap(), "client");
            assert_eq!(emitted.javac_args, ["-Xlint"]);
            assert_eq!(emitted.jvm_args, ["-Xmx1g"]);
            assert_eq!(emitted.compile_env.get("LANG").unwrap(), "C");
            assert_eq!(emitted.run_env.get("MODE").unwrap(), "test");
            assert!(
                emitted
                    .generated_sources
                    .contains(&FileKey::parse("source.java").unwrap())
            );
            assert!(
                emitted
                    .additional_classpath
                    .contains(&FileKey::parse("gen/classes.jar").unwrap())
            );
            assert!(
                emitted
                    .rerun_files
                    .contains(&FileKey::parse("note.txt").unwrap())
            );
            assert_eq!(emitted.diagnostics.len(), 1);
            assert_eq!(emitted.diagnostics[0].to_string(), "warning: a warning");
        });
    }

    /// The script's own entry-point convention is not a signature the compiler can check, so a
    /// script that spells it wrong is told what the convention is instead of what it exported.
    #[test]
    fn a_script_without_main_is_told_what_an_entry_point_is() {
        block_on_inline(async {
            let storage = storage("class build {\n    static void helper() {\n    }\n}\n", []);

            let error = prepare(
                &storage,
                &BuildScriptEnvironment::new(),
                &BuildScriptLimits::default(),
            )
            .await
            .unwrap_err();
            let BuildScriptError::Execute { message, .. } = error else {
                panic!("a missing entry point is a run failure, got {error:?}");
            };
            assert!(
                message.contains("`static` method named `main`"),
                "the message names the convention: {message}"
            );
        });
    }

    /// A refusal is a trap at the call that made it, and the position is the statement that call
    /// was written in.
    #[test]
    fn a_host_refusal_stops_the_run_at_its_statement() {
        block_on_inline(async {
            let script = "\
class build {
    public static void main() {
        String value = jals.build.Project.readText(\"missing.txt\");
    }
}
";
            let storage = storage(script, []);

            let error = prepare(
                &storage,
                &BuildScriptEnvironment::new(),
                &BuildScriptLimits::default(),
            )
            .await
            .unwrap_err();
            let BuildScriptError::Execute {
                position, message, ..
            } = error
            else {
                panic!("a refused read is a run failure, got {error:?}");
            };
            assert!(
                message.contains("Project.readText `missing.txt`"),
                "the refusal names the operation and the key: {message}"
            );
            assert!(
                !message.contains("host function trap"),
                "the engine's scaffolding is not part of the refusal: {message}"
            );
            let position = position
                .unwrap_or_else(|| panic!("the module carries statement positions: {message}"));
            let range = position.byte_range(script).unwrap();
            assert_eq!(
                line_of(script, range.start),
                3,
                "the position is the statement the call was written in"
            );
        });
    }

    /// `Build.error` records rather than throws, and the run's report is every diagnostic in
    /// order — the warnings before it are its context.
    #[test]
    fn reported_errors_travel_with_every_earlier_diagnostic() {
        block_on_inline(async {
            let script = "\
class build {
    public static void main() {
        jals.build.Build.warning(\"first\");
        jals.build.Build.error(\"boom\");
        jals.build.Build.error(\"second\");
    }
}
";
            let storage = storage(script, []);

            let error = prepare(
                &storage,
                &BuildScriptEnvironment::new(),
                &BuildScriptLimits::default(),
            )
            .await
            .unwrap_err();
            let BuildScriptError::ReportedErrors(diagnostics) = error else {
                panic!("a reported error diverts the output, got {error:?}");
            };
            let rendered: Vec<String> = diagnostics.iter().map(ToString::to_string).collect();
            assert_eq!(rendered, ["warning: first", "error: boom", "error: second"]);
        });
    }

    /// A compile failure points at the expression it happened in, which is what the exact-position
    /// contract means for the script's own file.
    #[test]
    fn a_compile_error_points_at_the_expression() {
        block_on_inline(async {
            let script = "\
class build {
    public static void main() {
        int x = \"text\";
    }
}
";
            let storage = storage(script, []);

            let error = prepare(
                &storage,
                &BuildScriptEnvironment::new(),
                &BuildScriptLimits::default(),
            )
            .await
            .unwrap_err();
            let BuildScriptError::Compile {
                position, message, ..
            } = error
            else {
                panic!("an ill-typed initialiser is a compile failure, got {error:?}");
            };
            let position = position.expect("a compile error in the script carries a position");
            let range = position.byte_range(script).unwrap();
            assert_eq!(
                line_of(script, range.start),
                3,
                "the position is the statement the expression was written in: {message}"
            );
        });
    }

    /// The budget is `max_operations` in source-level units: a script that never finishes stops,
    /// and says which statement it was in when the budget ran out.
    #[test]
    fn a_run_over_its_budget_stops_and_names_the_statement() {
        block_on_inline(async {
            let script = "\
class build {
    public static void main() {
        int n = 0;
        while (n >= 0)
            n = n + 1;
    }
}
";
            let storage = storage(script, []);
            let limits = BuildScriptLimits {
                // 625 operations at the engine's 16 instructions per operation.
                max_operations: 625,
                ..BuildScriptLimits::default()
            };

            let error = prepare(&storage, &BuildScriptEnvironment::new(), &limits)
                .await
                .unwrap_err();
            let BuildScriptError::Execute {
                position, message, ..
            } = error
            else {
                panic!("an unbounded loop is stopped by the budget, got {error:?}");
            };
            assert!(
                message.contains("instruction budget"),
                "the failure is the budget, not the code: {message}"
            );
            let position = position.expect("the module carries statement positions");
            let range = position.byte_range(script).unwrap();
            assert_eq!(
                line_of(script, range.start),
                5,
                "the position is the statement the loop was in"
            );
        });
    }

    /// A script that records every task the Java `Tasks` publishes: one of each node kind, both
    /// `remapJar` grammars, and every terminal.
    const TASKS: &str = r#"
import jals.build.MappingFormat;
import jals.build.Tasks;

class build {
    public static void main() {
        String sha1 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        String sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        int url = Tasks.httpsUrl("https://example.invalid/sources.jar");
        int local = Tasks.projectJar("lib/local.jar");
        int old = Tasks.sha1(sha1);
        int digest = Tasks.sha256(sha256);
        int capped = Tasks.bytes(4096);
        int index = Tasks.fetchJson(url, digest, capped);
        int fetched = Tasks.fetchJar(url, digest, capped);
        int text = Tasks.fetchText(url, digest, capped);
        String[] empty = new String[0];
        String[] down = new String[2];
        down[0] = "downloads";
        down[1] = "sources";
        int at = Tasks.jsonAt(index, down);
        int found = Tasks.jsonFindString(index, empty, "latest", "release");
        int listed = Tasks.jsonUrl(index, down);
        int oldSum = Tasks.jsonSha1(index, down);
        int sum = Tasks.jsonSha256(index, down);
        int size = Tasks.jsonU64(index, down);
        int sources = Tasks.extractJava(local, "net/example");
        int nested = Tasks.nestedJar(local, "META-INF/libraries/a.jar");
        int remapped = Tasks.remapJar(nested, text);
        MappingFormat tiny = Tasks.tinyV2("official", "named");
        int named = Tasks.remapJarAs(fetched, text, tiny);
        MappingFormat plain = Tasks.proguard();
        int guarded = Tasks.remapJarAs(local, text, plain);
        int merged = Tasks.mergeJars(remapped, named);
        int decompiled = Tasks.decompileJava(merged, "src");
        int member = Tasks.jarText(local, "mappings/mappings.tiny");
        MappingFormat pair = Tasks.tinyV2("intermediary", "mojang");
        int composed = Tasks.composeMappings(text, member, pair);
        int extended = Tasks.copyMappings(composed, "me/M\ta/B\ttick");
        int resolved = Tasks.resolveReferences("k\tctx\ttick", extended, pair);
        int document = Tasks.jsonFromLines(resolved);
        Tasks.addClasspath(guarded);
        Tasks.addNestedClasspath(nested);
        Tasks.publishTree("example", sources, "src/main/java/net/example", "navigation");
        Tasks.publishTree("example", decompiled, "src/main/java", "compile");
        Tasks.publishText("resources/refmap.json", document);
    }
}
"#;

    /// The whole task vocabulary crosses the boundary as one plan, node for node.
    ///
    /// The expected plan is written out rather than spot-checked because what the test is worth is
    /// the *crossing*: handles named by the number a Java call answered with, a `String[]` path
    /// unpacked from its two arrays, algorithm and fetch-kind tags, and a `MappingFormat` that
    /// crossed as data rather than as a call.
    #[test]
    fn a_script_records_every_task_the_java_api_publishes() {
        block_on_inline(async {
            let storage = storage(TASKS, []);
            let prepared = prepare(
                &storage,
                &BuildScriptEnvironment::new(),
                &BuildScriptLimits::default(),
            )
            .await
            .expect("every declared task is inside the limits");

            let plan = prepared.output(storage.revision()).task_plan;
            let node = |index: u32, kind: TaskNodeKind| TaskNode {
                id: TaskId::new(index),
                kind,
            };
            let down = vec!["downloads".to_owned(), "sources".to_owned()];
            let expected = TaskPlan {
                nodes: vec![
                    node(
                        0,
                        TaskNodeKind::HttpsUrl {
                            value: "https://example.invalid/sources.jar".to_owned(),
                        },
                    ),
                    node(
                        1,
                        TaskNodeKind::ProjectJar {
                            path: "lib/local.jar".to_owned(),
                        },
                    ),
                    node(
                        2,
                        TaskNodeKind::Digest {
                            algorithm: TaskDigestAlgorithm::Sha1,
                            value: "a".repeat(40),
                        },
                    ),
                    node(
                        3,
                        TaskNodeKind::Digest {
                            algorithm: TaskDigestAlgorithm::Sha256,
                            value: "a".repeat(64),
                        },
                    ),
                    node(4, TaskNodeKind::ByteCount { value: 4096 }),
                    node(
                        5,
                        TaskNodeKind::Fetch {
                            kind: TaskFetchKind::Json,
                            url: TaskId::new(0),
                            digest: TaskId::new(3),
                            max_bytes: TaskId::new(4),
                        },
                    ),
                    node(
                        6,
                        TaskNodeKind::Fetch {
                            kind: TaskFetchKind::Jar,
                            url: TaskId::new(0),
                            digest: TaskId::new(3),
                            max_bytes: TaskId::new(4),
                        },
                    ),
                    node(
                        7,
                        TaskNodeKind::Fetch {
                            kind: TaskFetchKind::Text,
                            url: TaskId::new(0),
                            digest: TaskId::new(3),
                            max_bytes: TaskId::new(4),
                        },
                    ),
                    node(
                        8,
                        TaskNodeKind::JsonAt {
                            json: TaskId::new(5),
                            path: down.clone(),
                        },
                    ),
                    node(
                        9,
                        TaskNodeKind::JsonFindString {
                            json: TaskId::new(5),
                            path: Vec::new(),
                            field: "latest".to_owned(),
                            value: "release".to_owned(),
                        },
                    ),
                    node(
                        10,
                        TaskNodeKind::JsonUrl {
                            json: TaskId::new(5),
                            path: down.clone(),
                        },
                    ),
                    node(
                        11,
                        TaskNodeKind::JsonDigest {
                            json: TaskId::new(5),
                            path: down.clone(),
                            algorithm: TaskDigestAlgorithm::Sha1,
                        },
                    ),
                    node(
                        12,
                        TaskNodeKind::JsonDigest {
                            json: TaskId::new(5),
                            path: down.clone(),
                            algorithm: TaskDigestAlgorithm::Sha256,
                        },
                    ),
                    node(
                        13,
                        TaskNodeKind::JsonU64 {
                            json: TaskId::new(5),
                            path: down,
                        },
                    ),
                    node(
                        14,
                        TaskNodeKind::ExtractJava {
                            jar: TaskId::new(1),
                            prefix: "net/example".to_owned(),
                        },
                    ),
                    node(
                        15,
                        TaskNodeKind::NestedJar {
                            jar: TaskId::new(1),
                            member: "META-INF/libraries/a.jar".to_owned(),
                        },
                    ),
                    node(
                        16,
                        TaskNodeKind::RemapJar {
                            jar: TaskId::new(15),
                            mappings: TaskId::new(7),
                            format: TaskMappingFormat::Proguard,
                            direction: TaskRemapDirection::Deobfuscate,
                            hierarchy: Vec::new(),
                        },
                    ),
                    node(
                        17,
                        TaskNodeKind::RemapJar {
                            jar: TaskId::new(6),
                            mappings: TaskId::new(7),
                            format: TaskMappingFormat::TinyV2 {
                                from: "official".to_owned(),
                                to: "named".to_owned(),
                            },
                            direction: TaskRemapDirection::Deobfuscate,
                            hierarchy: Vec::new(),
                        },
                    ),
                    node(
                        18,
                        TaskNodeKind::RemapJar {
                            jar: TaskId::new(1),
                            mappings: TaskId::new(7),
                            format: TaskMappingFormat::Proguard,
                            direction: TaskRemapDirection::Deobfuscate,
                            hierarchy: Vec::new(),
                        },
                    ),
                    node(
                        19,
                        TaskNodeKind::MergeJars {
                            base: TaskId::new(16),
                            overlay: TaskId::new(17),
                        },
                    ),
                    node(
                        20,
                        TaskNodeKind::DecompileJava {
                            jar: TaskId::new(19),
                            prefix: "src".to_owned(),
                        },
                    ),
                    node(
                        21,
                        TaskNodeKind::JarText {
                            jar: TaskId::new(1),
                            member: "mappings/mappings.tiny".to_owned(),
                        },
                    ),
                    node(
                        22,
                        TaskNodeKind::ComposeMappings {
                            official: TaskId::new(7),
                            intermediary: TaskId::new(21),
                            format: TaskMappingFormat::TinyV2 {
                                from: "intermediary".to_owned(),
                                to: "mojang".to_owned(),
                            },
                        },
                    ),
                    node(
                        23,
                        TaskNodeKind::CopyMappings {
                            mappings: TaskId::new(22),
                            copies: "me/M\ta/B\ttick".to_owned(),
                        },
                    ),
                    node(
                        24,
                        TaskNodeKind::ResolveReferences {
                            requests: "k\tctx\ttick".to_owned(),
                            mappings: TaskId::new(23),
                            format: TaskMappingFormat::TinyV2 {
                                from: "intermediary".to_owned(),
                                to: "mojang".to_owned(),
                            },
                        },
                    ),
                    node(
                        25,
                        TaskNodeKind::JsonFromLines {
                            records: TaskId::new(24),
                        },
                    ),
                ],
                terminals: vec![
                    TaskTerminal::AddClasspath {
                        jar: TaskId::new(18),
                    },
                    TaskTerminal::AddNestedClasspath {
                        jar: TaskId::new(15),
                    },
                    TaskTerminal::PublishTree {
                        owner: "example".to_owned(),
                        tree: TaskId::new(14),
                        destination: "src/main/java/net/example".to_owned(),
                        mode: TaskPublishMode::ReplaceRoot,
                        intent: TaskPublishIntent::Navigation,
                    },
                    TaskTerminal::PublishTree {
                        owner: "example".to_owned(),
                        tree: TaskId::new(20),
                        destination: "src/main/java".to_owned(),
                        mode: TaskPublishMode::ReplaceRoot,
                        intent: TaskPublishIntent::Compile,
                    },
                    TaskTerminal::PublishText {
                        path: "resources/refmap.json".to_owned(),
                        text: TaskId::new(25),
                    },
                ],
            };
            assert_eq!(plan, expected);
        });
    }

    /// Prepare `script`, expecting a run failure, and answer its message and 1-based line.
    async fn refusal(script: &str) -> (String, u32) {
        let storage = storage(script, []);
        let error = prepare(
            &storage,
            &BuildScriptEnvironment::new(),
            &BuildScriptLimits::default(),
        )
        .await
        .unwrap_err();
        let BuildScriptError::Execute {
            position, message, ..
        } = error
        else {
            panic!("expected a run failure, got {error:?}");
        };
        let position =
            position.unwrap_or_else(|| panic!("the module carries statement positions: {message}"));
        let range = position.byte_range(script).unwrap();
        (message, line_of(script, range.start))
    }

    /// A task refusal is the vocabulary's, is written at the call, and is not the engine's
    /// scaffolding — every kind of mistake the Java surface can make, one case each.
    #[test]
    fn the_task_vocabulary_refuses_what_cannot_run() {
        block_on_inline(async {
            let cases = [
                (
                    "class build {\n    public static void main() {\n        long max = jals.build.Tasks.bytes(-1);\n    }\n}\n",
                    "Tasks.bytes requires a positive byte count",
                ),
                (
                    "class build {\n    public static void main() {\n        jals.build.MappingFormat f = jals.build.Tasks.tinyV2(\"\", \"named\");\n    }\n}\n",
                    "Tasks.tinyV2 needs two namespace names",
                ),
                (
                    "class build {\n    public static void main() {\n        jals.build.MappingFormat f = jals.build.Tasks.tinyV2(\"official\", \"official\");\n    }\n}\n",
                    "naming one twice renames nothing",
                ),
                (
                    "class build {\n    public static void main() {\n        int url = jals.build.Tasks.httpsUrl(\"https://example.invalid/x\");\n        jals.build.Tasks.addClasspath(url);\n    }\n}\n",
                    "expected Jar",
                ),
                (
                    "class build {\n    public static void main() {\n        jals.build.Tasks.addClasspath(7);\n    }\n}\n",
                    "missing node 7",
                ),
                (
                    "class build {\n    public static void main() {\n        int local = jals.build.Tasks.projectJar(\"lib/local.jar\");\n        jals.build.Tasks.publishTree(\"example\", local, \"dest\", \"watch\");\n    }\n}\n",
                    "Tasks.publishTree needs an intent of `compile`",
                ),
                (
                    "class build {\n    public static void main() {\n        int url = jals.build.Tasks.httpsUrl(\"https://example.invalid/m.txt\");\n        int digest = jals.build.Tasks.sha256(\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\");\n        int capped = jals.build.Tasks.bytes(4096);\n        int text = jals.build.Tasks.fetchText(url, digest, capped);\n        int composed = jals.build.Tasks.composeMappings(text, text, jals.build.Tasks.proguard());\n    }\n}\n",
                    "Tasks.composeMappings needs the tiny v2 namespace pair",
                ),
                (
                    "class build {\n    public static void main() {\n        int url = jals.build.Tasks.httpsUrl(\"https://example.invalid/m.txt\");\n        int digest = jals.build.Tasks.sha256(\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\");\n        int capped = jals.build.Tasks.bytes(4096);\n        int text = jals.build.Tasks.fetchText(url, digest, capped);\n        int resolved = jals.build.Tasks.resolveReferences(\"k\\tctx\\ttick\", text, jals.build.Tasks.proguard());\n    }\n}\n",
                    "Tasks.resolveReferences needs the tiny v2 namespace pair",
                ),
            ];
            for (script, needle) in cases {
                let (message, _) = refusal(script).await;
                assert!(message.contains(needle), "`{needle}` not in `{message}`");
                assert!(
                    !message.contains("host function trap"),
                    "the engine's scaffolding is not part of the refusal: {message}"
                );
            }
        });
    }

    /// A refusal the plan's own validation makes — not the binding's — still points at the
    /// statement that wrote the offending value.
    #[test]
    fn a_task_refusal_points_at_its_statement() {
        block_on_inline(async {
            let script = "\
class build {
    public static void main() {
        int local = jals.build.Tasks.projectJar(\"lib/local.jar\");
        long bytes = jals.build.Tasks.bytes(0);
    }
}
";
            let (message, line) = refusal(script).await;
            assert!(
                message.contains("byte count must be non-zero"),
                "the plan's validation named the value: {message}"
            );
            assert_eq!(
                line, 4,
                "the position is the statement the call was written in"
            );
        });
    }
}
