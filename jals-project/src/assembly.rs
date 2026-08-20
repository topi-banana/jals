//! The order and preconditions of project assembly.
//!
//! [`assemble`](crate::assemble) owns one *step* of it — the mode-independent graph projection. This
//! module owns the *sequence*: the root build script and its task plan, then dependency discovery,
//! preprocessing, projection, and input resolution.
//!
//! A host supplies policy and an aggregate; it never orders the steps. [`ProjectScript`] is the only
//! way from the script phase into the graph phase, so the order cannot be forgotten — and because it
//! is two calls rather than one, a host still chooses *where* to hand the aggregate over. That
//! matters: `jals-cli` reopens storage under narrower scopes for the graph phase, and the browser
//! playground releases its workspace lock in between so a jar download never blocks the editor.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::fmt;

use jals_build::build_script::{BuildScriptOutput, BuildScriptSession};
use jals_classpath::{
    ClasspathEntry, Fetcher, MemoryProjectPlan, ProjectInputOptions, ProjectInputPlan,
    ProjectInputs,
};
use jals_config::Manifest;
use jals_exec::Exec;
use jals_storage::{
    CacheBackend, CacheKey, DirKey, Name, ProjectStorage, RelativePath, SourceBackend,
};

use crate::assemble::{
    CompileClasspathEntry, CompileClasspathFile, ProjectAssemblyError, ProjectGraphAssembly,
};
use crate::diagnostics::ProjectReport;
use crate::graph::{
    GraphError, GraphMetadata, GraphPreprocess, GraphWarning, NodeBody, PreprocessedProjectGraph,
};
use crate::memory;
use crate::task::{RootBuildScriptError, RootBuildScriptOptions};

pub use api::script;

/// Namespace owning the project-assembly procedure.
mod api {
    use super::{
        BuildScriptSession, CacheBackend, Exec, Fetcher, ProjectScript, ProjectStorage,
        RootBuildScriptError, RootBuildScriptOptions, SourceBackend,
    };
    use crate::task;

    /// Phase 1 — prepare and execute the root build script and its task plan, publishing ordinary
    /// and task output in one transaction.
    ///
    /// The returned [`ProjectScript`] is the only way into the graph phase.
    pub async fn script<F, S, C>(
        exec: &Exec,
        fetcher: &F,
        storage: &mut ProjectStorage<S, C>,
        session: &mut BuildScriptSession,
        options: RootBuildScriptOptions<'_>,
    ) -> Result<ProjectScript, RootBuildScriptError>
    where
        F: Fetcher,
        S: SourceBackend,
        C: CacheBackend,
    {
        let output = task::execute_root(exec, fetcher, storage, session, options).await?;
        Ok(ProjectScript {
            output: output.script,
            task_classpath: output.task_classpath,
        })
    }
}

/// Phase 1's product and the precondition of the graph phase.
#[derive(Debug)]
pub struct ProjectScript {
    output: Option<BuildScriptOutput>,
    task_classpath: Vec<CacheKey>,
}

impl ProjectScript {
    /// The graph-phase entry for a host that deliberately runs no script.
    ///
    /// `jals lint` analyses what is already on disk: opening a folder must not execute an unreviewed
    /// `build.rhai`, so it enters the graph phase with nothing published and no task classpath.
    pub const fn skipped() -> Self {
        Self {
            output: None,
            task_classpath: Vec::new(),
        }
    }

    /// A script phase rebuilt from what it produced.
    ///
    /// For a test that needs a task classpath without running a terminal that fetches — the fold in
    /// [`project`](Self::project) is what is under test, not how the artifacts were acquired.
    #[cfg(test)]
    pub(crate) const fn from_parts(
        output: Option<BuildScriptOutput>,
        task_classpath: Vec<CacheKey>,
    ) -> Self {
        Self {
            output,
            task_classpath,
        }
    }

    /// What the script reported, or `None` when the manifest declares none.
    pub const fn output(&self) -> Option<&BuildScriptOutput> {
        self.output.as_ref()
    }

    /// Verified artifacts the root's task terminals put on the classpath.
    pub fn task_classpath(&self) -> &[CacheKey] {
        &self.task_classpath
    }

    /// Fold the script's `add_classpath` directives into the manifest the graph phase lowers, so a
    /// script-contributed entry is classified by exactly the rule that classifies one written in
    /// `[build] classpath` — and lands in the same order, after the authored entries.
    pub fn augment_classpath(&self, manifest: &mut Manifest) {
        let Some(output) = &self.output else {
            return;
        };
        for classpath in &output.additional_classpath {
            let classpath = classpath.to_string();
            if !manifest.build.classpath.contains(&classpath) {
                manifest.build.classpath.push(classpath);
            }
        }
    }

    /// The graph phase over one captured in-memory tree: discover, preprocess, project, and resolve
    /// the root's and the graph's inputs against `storage`.
    ///
    /// `preprocess.fetcher`'s [`NetworkPolicy`](jals_classpath::NetworkPolicy) governs the whole
    /// phase, discovery and input resolution included — a host cannot ask the graph to be
    /// discovered online and resolved offline, because there is one capability and every step is
    /// handed the same one.
    pub async fn resolve_memory<F, S, C>(
        &self,
        manifest: &Manifest,
        storage: &mut ProjectStorage<S, C>,
        preprocess: GraphPreprocess<'_, F>,
        options: ProjectInputOptions,
    ) -> Result<MemoryProjectAssembly, GraphResolveError>
    where
        F: Fetcher,
        S: SourceBackend,
        C: CacheBackend,
    {
        // `preprocess` is consumed by the phase it names, but the graph plan needs the same fetch
        // capability again when it resolves. The field is a shared reference, so copy it out first.
        let fetcher = preprocess.fetcher;
        let graph = memory::discover(manifest, &storage.view())
            .await
            .map_err(GraphResolveError::unreported)?;
        let discovered = graph.warnings.clone();
        let graph = graph
            .preprocess(storage.artifacts_mut(), preprocess)
            .await
            .map_err(|error| GraphResolveError::reporting(error, discovered))?;
        let graph_assembly = graph.assemble(storage.artifacts_mut()).await;
        // The manifest goes in whole. `MemoryProjectPlan` does not lower `[dependencies]` — that is
        // its documented contract and the thing `declared_dependencies_are_left_to_the_graph` pins —
        // so stripping the table here as well would state the same rule a second time, in the one
        // place a reader cannot check it from.
        let (inputs, source_roots) =
            MemoryProjectPlan::assemble(manifest, storage, fetcher, options).await;
        Ok(self
            .project(
                &graph,
                graph_assembly,
                RootProjection {
                    inputs,
                    source_roots,
                },
                fetcher,
                storage,
                options,
            )
            .await)
    }

    /// The projection steps shared by both adapters, independent of how the root plan was lowered:
    /// resolve the graph plan, normalize binary-node compile entries onto their resolved jars, and
    /// merge the root's inputs with the graph's.
    pub(crate) async fn project<F, S, C>(
        &self,
        graph: &PreprocessedProjectGraph,
        mut graph_assembly: ProjectGraphAssembly,
        root: RootProjection,
        fetcher: &F,
        storage: &mut ProjectStorage<S, C>,
        options: ProjectInputOptions,
    ) -> MemoryProjectAssembly
    where
        F: Fetcher,
        S: SourceBackend,
        C: CacheBackend,
    {
        let RootProjection {
            inputs: root_inputs,
            source_roots,
        } = root;
        // The root's task terminals produced verified jars, and they sit between the root's authored
        // `[build] classpath` and the graph's dependencies. `root_inputs` carries the first group and
        // is concatenated ahead of the graph's below, so prepending here puts them in that order.
        //
        // Putting them *in the plan* rather than loading them afterwards is also what gives them the
        // rest of the plan's treatment, and under `Editor` that includes skeleton synthesis: a task
        // jar's classes now get navigation sources like any other classpath entry, so a definition
        // in a task-provided library is openable instead of being a dead jump. That is the point of
        // the fold, not a side effect of it — `task_classpath_classes_get_navigation_sources` pins
        // it — but it does mean a host with a large task jar publishes skeleton artifacts it did not
        // publish before.
        let mut classpath: Vec<_> = self
            .task_classpath
            .iter()
            .cloned()
            .map(ClasspathEntry::Artifact)
            .collect();
        classpath.append(&mut graph_assembly.plan.classpath);
        graph_assembly.plan.classpath = classpath;

        // The same two groups in the same order, kept as keys because one consumer needs them as
        // archives rather than as classpath entries: a post-compile `[build] remap` closes the
        // hierarchy of what it reobfuscates against these, and a member inherited from a jar that is
        // missing keeps its original name in an otherwise remapped archive. Concatenated and not
        // deduplicated here — the set a host finally hands to the remap also carries the resolved
        // `[dependencies]` jars, which never pass through this function, so the one place all of it
        // is in hand is the host, and that is where the duplicates are dropped.
        let mut task_classpath = self.task_classpath.clone();
        task_classpath.extend(graph_assembly.task_classpath.iter().cloned());

        let graph_inputs =
            ProjectInputs::assemble(fetcher, storage, &graph_assembly.plan, options).await;

        // A binary node's captured bytes and its resolved jar are the same content reached two ways.
        // Compiling against both would put one library on the classpath twice, so the captured
        // entries drop out and the resolved jars stand in for them.
        let binary_nodes: BTreeSet<_> = graph
            .nodes
            .iter()
            .filter(|node| matches!(node.body, NodeBody::Binary(_)))
            .map(|node| node.id.clone())
            .collect();
        let mut compile_classpath = graph_assembly.compile_classpath;
        compile_classpath
            .retain(|entry| entry.node().is_none_or(|node| !binary_nodes.contains(node)));
        for key in &graph_inputs.dependency_jars {
            let path = RelativePath::new([
                Name::new("dependencies").expect("constant is portable"),
                Name::new("resolved").expect("constant is portable"),
                Name::new(format!("{}.jar", key.content().to_hex()))
                    .expect("digest-derived file name is portable"),
            ]);
            compile_classpath.push(CompileClasspathEntry::File(CompileClasspathFile {
                node: None,
                path,
                key: key.clone(),
            }));
        }

        let mut inputs = root_inputs;
        inputs.dependency_jars.extend(graph_inputs.dependency_jars);
        inputs
            .classpath_classes
            .extend(graph_inputs.classpath_classes);
        inputs.library_sources.extend(graph_inputs.library_sources);
        inputs
            .source_dep_sources
            .extend(graph_inputs.source_dep_sources);
        inputs.warnings.extend(graph_inputs.warnings);

        MemoryProjectAssembly {
            graph: graph_assembly.graph,
            plan: graph_assembly.plan,
            inputs,
            source_roots,
            compile_classpath,
            task_classpath,
            warnings: graph_assembly.warnings,
            errors: graph_assembly.errors,
        }
    }
}

/// The root plan's already-lowered half, and the only part of the projection that differs between
/// the portable and native adapters.
pub(crate) struct RootProjection {
    pub(crate) inputs: ProjectInputs,
    pub(crate) source_roots: Vec<DirKey>,
}

/// Fully projected portable root plus its preprocessed dependency graph.
///
/// What a portable host reads is `inputs`, `warnings`, and `errors`. The rest is what the native
/// adapter carries into `NativeProjectAssembly`, where a host path gives it a consumer — which is
/// why those fields have no reader at all once `native` is compiled out. That is the shape of the
/// projection, not an oversight, so the portable build allows it rather than widening them back to
/// `pub` for a consumer that does not exist.
///
/// The allowance is per-field rather than on the struct: these five are the ones whose only reader
/// is behind `native`, and a sixth that went unread would be a mistake worth hearing about.
#[derive(Debug)]
pub struct MemoryProjectAssembly {
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) graph: GraphMetadata,
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) plan: ProjectInputPlan,
    pub inputs: ProjectInputs,
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) source_roots: Vec<DirKey>,
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) compile_classpath: Vec<CompileClasspathEntry>,
    /// Every archive the build tasks put on the classpath: the root script's terminals, then each
    /// dependency node's in discovery order — the order the plan's classpath already places them in.
    ///
    /// The subset of the compile classpath a `[build] remap` can read, and the reason it is a list
    /// of its own: an entry here is an archive because a task terminal only accepts one, whereas the
    /// compile classpath mixes those with directory members and bare `.class` artifacts that a
    /// hierarchy index would try to unpack and fail on.
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) task_classpath: Vec<CacheKey>,
    pub(crate) warnings: Vec<GraphWarning>,
    pub(crate) errors: Vec<ProjectAssemblyError>,
}

impl MemoryProjectAssembly {
    /// Everything this assembly reported, in one value.
    ///
    /// The three channels are not separately readable. The graph's warnings and errors sit here,
    /// the classpath's input warnings sit inside `inputs`, and a host that reached for the first
    /// two alone silently dropped the third — which is how an unreadable jar became something only
    /// a server's stderr ever mentioned. [`diagnostics::assemble`] takes this, so there is
    /// nothing left to forget.
    ///
    /// [`diagnostics::assemble`]: crate::diagnostics::assemble
    pub fn report(&self) -> ProjectReport<'_> {
        ProjectReport::new(&self.warnings, &self.errors, &self.inputs.warnings)
    }
}

/// A graph phase that failed, carrying the warnings the phases before it had already produced.
///
/// A successful phase returns its warnings inside the assembly, so before this type existed a
/// failure was the one outcome that discarded them — exactly when a host most needs them, because
/// the warning is often what explains the error. Discovery reporting `classpath entry is
/// unavailable` for a dependency, and preprocessing then failing on that same dependency, is one
/// diagnostic split across two values; a host that only prints the error prints half of it.
///
/// [`warnings`](Self::warnings) is empty when the failure happened *inside* discovery: the phase
/// that produces them did not finish, so there is nothing to carry rather than something withheld.
#[derive(Debug)]
pub struct GraphResolveError {
    pub error: GraphError,
    pub(crate) warnings: Vec<GraphWarning>,
}

impl GraphResolveError {
    /// A failure with nothing to report alongside it, because discovery itself did not complete.
    pub(crate) const fn unreported(error: GraphError) -> Self {
        Self {
            error,
            warnings: Vec::new(),
        }
    }

    /// A failure after discovery, carrying what discovery had already reported.
    pub(crate) const fn reporting(error: GraphError, warnings: Vec<GraphWarning>) -> Self {
        Self { error, warnings }
    }
}

impl fmt::Display for GraphResolveError {
    /// The error alone. Warnings are a separate list because a host shapes them separately — the
    /// CLI prints one `warning:` line each, the server turns each into its own diagnostic.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl core::error::Error for GraphResolveError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;

    use jals_build::build_script::{BuildScriptEnvironment, BuildScriptLimits};
    use jals_config::ResolvedBuildFeatures;
    use jals_exec::block_on_inline;
    use jals_storage::{
        CacheNamespace, CodeTree, ContentDigest, Entry, FileKey, MemoryStorage, ProvenanceFold,
    };

    use super::*;

    /// A fetch capability for assemblies that declare no task plan. Reaching it is the failure.
    struct UnreachableFetcher;

    impl Fetcher for UnreachableFetcher {
        // `Online`: the panic is the assertion — `Offline` would refuse first and pass blind.
        fn network(&self) -> jals_classpath::NetworkPolicy {
            jals_classpath::NetworkPolicy::Online
        }

        async fn fetch_admitted(&self, locator: &str) -> Result<Vec<u8>, String> {
            panic!("this assembly must not fetch, but asked for `{locator}`")
        }
    }

    /// Preprocessing inputs for an assembly under test, defaulting everything a task plan would
    /// need. A macro because the borrowed defaults only have to outlive the calling statement.
    macro_rules! inert {
        () => {
            GraphPreprocess {
                exec: &jals_exec::Exec::inline(),
                fetcher: &UnreachableFetcher,
                environment: &BuildScriptEnvironment::new(),
                root_features: &ResolvedBuildFeatures::default(),
                limits: &BuildScriptLimits::default(),
            }
        };
    }

    fn root_manifest() -> Manifest {
        "[build]\nsource-dirs = [\"src\"]\n[dependencies]\nchild = { path = \"deps/child\" }\n"
            .parse()
            .expect("test manifest is valid")
    }

    /// One project holding both its own sources and an in-tree path dependency, which is the shape a
    /// browser host has: view and artifact cache are halves of the same aggregate.
    fn project() -> MemoryStorage {
        MemoryStorage::memory(
            CodeTree::new([
                Entry::File(
                    FileKey::parse("src/Main.java").expect("portable key"),
                    b"class Main {}".to_vec(),
                ),
                Entry::File(
                    FileKey::parse("deps/child/jals.toml").expect("portable key"),
                    b"[build]\nsource-dirs = [\"src\"]\n".to_vec(),
                ),
                Entry::File(
                    FileKey::parse("deps/child/src/Child.java").expect("portable key"),
                    b"class Child {}".to_vec(),
                ),
            ])
            .expect("tree is valid"),
        )
    }

    /// How a dependency source arrives, not just whether it does. The distinction is load-bearing
    /// for a host: `Project` is a key in the project's own revision, `Artifact` is cache bytes a
    /// host has to mount before a definition target can be opened.
    fn source_kinds(assembly: &MemoryProjectAssembly, suffix: &str) -> Vec<&'static str> {
        assembly
            .inputs
            .source_dep_sources
            .iter()
            .filter_map(|source| match source {
                jals_classpath::SourceFile::Project(key) => {
                    key.to_string().ends_with(suffix).then_some("project")
                }
                jals_classpath::SourceFile::Artifact(source) => source
                    .path
                    .to_string()
                    .ends_with(suffix)
                    .then_some("artifact"),
            })
            .collect()
    }

    #[test]
    fn a_host_that_runs_no_script_still_lowers_the_root_and_the_graph() {
        block_on_inline(async {
            let mut storage = project();
            let assembly = ProjectScript::skipped()
                .resolve_memory(
                    &root_manifest(),
                    &mut storage,
                    inert!(),
                    ProjectInputOptions::Editor,
                )
                .await
                .expect("an in-tree path dependency resolves offline");

            assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
            assert_eq!(
                assembly.source_roots,
                vec![DirKey::parse("src").expect("portable key")],
                "the root's own `[build] source-dirs` come back from the root plan"
            );
            assert_eq!(
                assembly.graph.nodes().len(),
                1,
                "the declared path dependency is exactly one graph node"
            );
            assert_eq!(
                source_kinds(&assembly, "Child.java"),
                vec!["artifact"],
                "a portable graph publishes a dependency's sources into the consumer's cache, so a \
                 host mounts them rather than reading them from its own revision"
            );
        });
    }

    /// A dependency's sources are a **typing authority**, so every inputs policy reads them.
    ///
    /// `Analysis` used to come back empty here, which is why `jals lint` could not resolve a
    /// `path` dependency's types. Withholding a typing authority does not make an analysis pass
    /// cheaper — it makes every reference into that dependency an unresolved name. What the policy
    /// still decides is the *navigation* half (`-sources.jar` extraction and skeleton synthesis),
    /// which [`task_classpath_classes_get_navigation_sources`] covers.
    #[test]
    fn every_inputs_policy_reads_a_dependency_source() {
        block_on_inline(async {
            let manifest = root_manifest();
            for options in [
                ProjectInputOptions::Editor,
                ProjectInputOptions::Compile,
                ProjectInputOptions::Analysis,
            ] {
                let mut storage = project();
                let assembly = ProjectScript::skipped()
                    .resolve_memory(&manifest, &mut storage, inert!(), options)
                    .await
                    .expect("an in-tree path dependency resolves offline");
                assert!(
                    assembly.errors.is_empty(),
                    "{options:?}: {:?}",
                    assembly.errors
                );
                assert_eq!(
                    source_kinds(&assembly, "Child.java"),
                    vec!["artifact"],
                    "{options:?}: a dependency's sources define types the consumer names"
                );
            }
        });
    }

    /// Discovery and preprocessing report one diagnostic between them: the dependency discovery
    /// could not read is often the one preprocessing then fails on. A host that receives only the
    /// error prints half of it, so the failure carries the earlier phase's warnings — and says so
    /// when it has none, rather than leaving a host unable to tell "nothing to report" from "report
    /// discarded".
    #[test]
    fn a_failed_graph_phase_carries_what_the_earlier_phase_reported() {
        block_on_inline(async {
            let manifest: Manifest = "[dependencies]\n\
                 remote = { git = \"https://example.invalid/repo.git\" }\n\
                 child = { path = \"deps/child\", features = [\"nope\"] }\n"
                .parse()
                .expect("test manifest is valid");
            let mut storage = MemoryStorage::memory(
                CodeTree::new([
                    Entry::File(
                        FileKey::parse("deps/child/jals.toml").expect("portable key"),
                        b"[build]\nsource-dirs = [\"src\"]\n".to_vec(),
                    ),
                    // Present so the Git entry below is the *only* thing discovery has to warn
                    // about: an absent source root would warn too, and the count is the assertion.
                    Entry::File(
                        FileKey::parse("deps/child/src/Child.java").expect("portable key"),
                        b"class Child {}".to_vec(),
                    ),
                ])
                .expect("tree is valid"),
            );

            // Discovery warns about the Git entry a portable graph cannot acquire and carries on;
            // preprocessing then rejects the feature the child does not declare.
            let failure = ProjectScript::skipped()
                .resolve_memory(
                    &manifest,
                    &mut storage,
                    inert!(),
                    ProjectInputOptions::Editor,
                )
                .await
                .expect_err("`nope` is not a feature the child declares");

            assert!(
                matches!(failure.error, GraphError::InvalidDependency { .. }),
                "{:?}",
                failure.error
            );
            let messages: Vec<_> = failure
                .warnings
                .iter()
                .map(|warning| warning.message.clone())
                .collect();
            assert_eq!(messages.len(), 1, "{messages:?}");
            assert!(messages[0].contains("Git dependencies"), "{messages:?}");

            // A failure *inside* discovery is the one case with nothing to carry: the phase that
            // produces warnings never finished.
            // Built rather than parsed: parsing validates, and the point here is a manifest that
            // only `discover` gets to reject.
            let mut invalid = Manifest::default();
            invalid.build.classes_dir = String::new();
            let failure = ProjectScript::skipped()
                .resolve_memory(
                    &invalid,
                    &mut storage,
                    inert!(),
                    ProjectInputOptions::Editor,
                )
                .await
                .expect_err("the project root is not a usable `classes-dir`");
            assert!(
                matches!(failure.error, GraphError::InvalidRootManifest { .. }),
                "{:?}",
                failure.error
            );
            assert!(failure.warnings.is_empty(), "{:?}", failure.warnings);
        });
    }

    /// The whole classpath in the order a host gets it: the root's authored `[build] classpath`,
    /// then the root's task artifacts, then the graph's dependencies.
    ///
    /// All three groups carry *the same* fully-qualified name, which is what makes this a
    /// precedence test rather than a list-concatenation test — with distinct names the order proves
    /// nothing about what a downstream index binds, because nothing collides. `minor_version` tells
    /// the three copies apart: a lossless parse carries it through verbatim, and it is not part of
    /// the name that makes them collide.
    ///
    /// `Editor` so the entries are actually loaded. `the_root_tasks_classpath_leads_the_graphs_own`
    /// makes the narrower statement for `Compile`, where the entry is placed but never parsed.
    #[test]
    fn the_three_classpath_groups_resolve_in_host_order() {
        const BOX_CLASS: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../jals-classpath/tests/fixtures/Box.class"
        ));

        /// `Box.class` with `minor_version` (the `u16` right after the 4-byte magic) set to `group`,
        /// so a loaded class says which of the three placed it without changing its name.
        fn stamped(group: u8) -> Vec<u8> {
            let mut bytes = BOX_CLASS.to_vec();
            bytes[5] = group;
            bytes
        }

        block_on_inline(async {
            let manifest: Manifest = "[build]\nclasspath = [\"lib/Box.class\"]\n\
                 [dependencies]\nchild = { path = \"deps/child\" }\n"
                .parse()
                .expect("test manifest is valid");
            let mut storage = MemoryStorage::memory(
                CodeTree::new([
                    Entry::File(
                        FileKey::parse("lib/Box.class").expect("portable key"),
                        stamped(1),
                    ),
                    Entry::File(
                        FileKey::parse("deps/child/jals.toml").expect("portable key"),
                        b"[build]\nclasspath = [\"lib/Box.class\"]\n".to_vec(),
                    ),
                    Entry::File(
                        FileKey::parse("deps/child/lib/Box.class").expect("portable key"),
                        stamped(3),
                    ),
                ])
                .expect("tree is valid"),
            );
            // A task terminal's verified jar. It has to be a real archive: the fold places task
            // artifacts as `ClasspathEntry::Artifact`, which a classpath load reads as a jar.
            let task_jar = jals_classpath::write_jar(
                &[(
                    RelativePath::parse("Box.class").expect("portable path"),
                    stamped(2),
                )],
                None,
            )
            .expect("packaging one class is infallible");
            let task_key = jals_storage::CacheKey::new(
                CacheNamespace::BuildTaskArtifact,
                ProvenanceFold::new(b"assembly-test\0").finish(),
                ContentDigest::of(&task_jar),
            );
            storage
                .artifacts_mut()
                .publish(&task_key, &task_jar)
                .await
                .expect("an in-memory publication is infallible");
            let script = ProjectScript::from_parts(None, vec![task_key]);

            let assembly = script
                .resolve_memory(
                    &manifest,
                    &mut storage,
                    inert!(),
                    ProjectInputOptions::Editor,
                )
                .await
                .expect("an in-tree path dependency resolves offline");

            assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
            let names: BTreeSet<_> = assembly
                .inputs
                .classpath_classes
                .iter()
                .map(|class| {
                    class
                        .constant_pool
                        .class_name(class.this_class)
                        .expect("a loaded class names itself")
                        .into_owned()
                })
                .collect();
            assert_eq!(
                names.len(),
                1,
                "the three groups have to collide for order to mean precedence: {names:?}"
            );
            let groups: Vec<_> = assembly
                .inputs
                .classpath_classes
                .iter()
                .map(|class| class.minor_version)
                .collect();
            assert_eq!(
                groups,
                vec![1, 2, 3],
                "the root's authored entry, then the root's task artifact, then the graph's"
            );
        });
    }

    #[test]
    fn the_root_tasks_classpath_leads_the_graphs_own() {
        block_on_inline(async {
            let mut storage = project();
            let bytes = b"task output".as_slice();
            let key = jals_storage::CacheKey::new(
                CacheNamespace::BuildTaskArtifact,
                ProvenanceFold::new(b"assembly-test\0").finish(),
                ContentDigest::of(bytes),
            );
            storage
                .artifacts_mut()
                .publish(&key, bytes)
                .await
                .expect("an in-memory publication is infallible");
            // What `api::script` would have produced for a root plan whose terminal
            // added one verified artifact to the classpath.
            let script = ProjectScript::from_parts(None, vec![key.clone()]);

            // `Compile` so the entry is placed but not parsed: what is under test is where the fold
            // puts it, not what a classpath loader makes of these bytes.
            let assembly = script
                .resolve_memory(
                    &root_manifest(),
                    &mut storage,
                    inert!(),
                    ProjectInputOptions::Compile,
                )
                .await
                .expect("an in-tree path dependency resolves offline");

            assert_eq!(
                assembly.plan.classpath.first(),
                Some(&ClasspathEntry::Artifact(key)),
                "a task artifact precedes the graph's dependency classpath"
            );
        });
    }

    /// Folding the task classpath into the plan rather than loading it afterwards is what earns a
    /// task jar's classes the rest of the plan's treatment, and under `Editor` that means skeleton
    /// synthesis: a definition in a task-provided library becomes an openable navigation source
    /// instead of a dead jump.
    ///
    /// Pinned because it is the visible half of the fold that is easiest to lose again — placing
    /// these entries anywhere downstream of [`ProjectInputs::assemble`] restores the old behavior
    /// and no ordering assertion notices.
    #[test]
    fn task_classpath_classes_get_navigation_sources() {
        const BOX_CLASS: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../jals-classpath/tests/fixtures/Box.class"
        ));

        block_on_inline(async {
            // No `[build] classpath` and a dependency that contributes sources rather than classes,
            // so the task jar is the only thing on the classpath and the only thing a skeleton can
            // have come from.
            let manifest = root_manifest();
            let task_jar = jals_classpath::write_jar(
                &[(
                    RelativePath::parse("Box.class").expect("portable path"),
                    BOX_CLASS.to_vec(),
                )],
                None,
            )
            .expect("packaging one class is infallible");
            let task_key = jals_storage::CacheKey::new(
                CacheNamespace::BuildTaskArtifact,
                ProvenanceFold::new(b"assembly-test\0").finish(),
                ContentDigest::of(&task_jar),
            );

            let mut skeletons = Vec::new();
            for options in [ProjectInputOptions::Editor, ProjectInputOptions::Analysis] {
                let mut storage = project();
                storage
                    .artifacts_mut()
                    .publish(&task_key, &task_jar)
                    .await
                    .expect("an in-memory publication is infallible");
                let assembly = ProjectScript::from_parts(None, vec![task_key.clone()])
                    .resolve_memory(&manifest, &mut storage, inert!(), options)
                    .await
                    .expect("an in-tree path dependency resolves offline");

                assert_eq!(
                    assembly.inputs.classpath_classes.len(),
                    1,
                    "{options:?}: the task jar's one class is the whole classpath"
                );
                skeletons.push(
                    assembly
                        .inputs
                        .library_sources
                        .iter()
                        .filter(|source| source.key.namespace() == CacheNamespace::Skeleton)
                        .map(|source| source.path.to_string())
                        .collect::<Vec<_>>(),
                );
            }

            let [editor, analysis] = <[Vec<String>; 2]>::try_from(skeletons).expect("two modes");
            assert_eq!(
                editor.len(),
                1,
                "the task jar's class is navigable under `Editor`: {editor:?}"
            );
            assert!(
                editor[0].ends_with("Box.java"),
                "the skeleton is the task jar's own type: {editor:?}"
            );
            // The mode still decides. `Analysis` loads the same class for the index and synthesizes
            // nothing, so the fold widened what navigation covers, not what every host pays for.
            assert!(analysis.is_empty(), "{analysis:?}");
        });
    }

    /// A project holding an in-tree path dependency whose own build script puts `jar` on the
    /// classpath — the shape a mod has, where the library it compiles against is a task artifact
    /// rather than a declared dependency.
    fn project_with_child_task_jar(jar: &[u8]) -> MemoryStorage {
        MemoryStorage::memory(
            CodeTree::new([
                Entry::File(
                    FileKey::parse("src/Main.java").expect("portable key"),
                    b"class Main {}".to_vec(),
                ),
                Entry::File(
                    FileKey::parse("deps/child/jals.toml").expect("portable key"),
                    b"[build]\nsource-dirs = [\"src\"]\n\
                      script = { type = \"rhai\", file = \"build.rhai\" }\n"
                        .to_vec(),
                ),
                Entry::File(
                    FileKey::parse("deps/child/build.rhai").expect("portable key"),
                    br#"tasks.add_classpath(tasks.project_jar("vendor/lib.jar"));"#.to_vec(),
                ),
                Entry::File(
                    FileKey::parse("deps/child/vendor/lib.jar").expect("portable key"),
                    jar.to_vec(),
                ),
                Entry::File(
                    FileKey::parse("deps/child/src/Child.java").expect("portable key"),
                    b"class Child {}".to_vec(),
                ),
            ])
            .expect("tree is valid"),
        )
    }

    /// A one-class jar, packaged the way a task terminal's value would be.
    fn task_jar() -> Vec<u8> {
        const BOX_CLASS: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../jals-classpath/tests/fixtures/Box.class"
        ));
        jals_classpath::write_jar(
            &[(
                RelativePath::parse("Box.class").expect("portable path"),
                BOX_CLASS.to_vec(),
            )],
            None,
        )
        .expect("packaging one class is infallible")
    }

    /// The archives a `[build] remap` closes its hierarchy with are one list, and both halves are in
    /// it: the root script's own terminals and every dependency node's.
    ///
    /// Either half missing is a *silent* wrong answer rather than a failure — the remap succeeds and
    /// writes a jar whose inherited member still says the source name — so this asserts the list
    /// itself rather than that some remap eventually worked.
    #[test]
    fn the_root_and_the_graphs_task_jars_are_one_hierarchy_in_that_order() {
        block_on_inline(async {
            let jar = task_jar();
            let mut storage = project_with_child_task_jar(&jar);
            let root_key = jals_storage::CacheKey::new(
                CacheNamespace::BuildTaskArtifact,
                ProvenanceFold::new(b"assembly-test\0").finish(),
                ContentDigest::of(b"root task jar"),
            );
            storage
                .artifacts_mut()
                .publish(&root_key, b"root task jar")
                .await
                .expect("an in-memory publication is infallible");

            // `Compile`, so nothing parses these bytes: what is under test is which keys the
            // projection collects and in what order, not what a classpath loader makes of them.
            let assembly = ProjectScript::from_parts(None, vec![root_key.clone()])
                .resolve_memory(
                    &root_manifest(),
                    &mut storage,
                    inert!(),
                    ProjectInputOptions::Compile,
                )
                .await
                .expect("an in-tree path dependency resolves offline");

            assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
            assert_eq!(
                assembly.task_classpath.len(),
                2,
                "the root's terminal and the dependency's are both hierarchy jars"
            );
            assert_eq!(
                assembly.task_classpath[0], root_key,
                "the root's own terminal comes first, as it does on the plan's classpath"
            );
            assert_eq!(
                assembly.task_classpath[1].content(),
                ContentDigest::of(&jar),
                "the second entry is the jar the dependency's script added"
            );

            // The two lists are produced by one traversal and must not drift apart: this fixture
            // declares no `[build] classpath`, so every classpath artifact in the plan is a task
            // jar and the two orders are directly comparable.
            let planned: Vec<_> = assembly
                .plan
                .classpath
                .iter()
                .filter_map(|entry| match entry {
                    ClasspathEntry::Artifact(key) | ClasspathEntry::ArtifactFile { key, .. } => {
                        Some(key.clone())
                    }
                    ClasspathEntry::ProjectFile(_) | ClasspathEntry::ProjectDirectory(_) => None,
                })
                .collect();
            assert_eq!(
                planned, assembly.task_classpath,
                "the hierarchy list and the plan's task entries are one traversal"
            );
        });
    }

    /// A `[build] classpath` *directory* contributes nothing to the hierarchy, and that is the point
    /// rather than an omission.
    ///
    /// Its members are individual `.class` files. A hierarchy entry is unpacked as an archive and a
    /// failure there fails the whole remap, so collecting keys from the compile classpath — where a
    /// tree's members sit beside real jars — would turn a build that works today into a decode
    /// error. The list is taken from task terminals precisely because those are archives by type.
    #[test]
    fn a_classpath_directory_contributes_no_hierarchy_entry() {
        block_on_inline(async {
            let mut storage = MemoryStorage::memory(
                CodeTree::new([
                    Entry::File(
                        FileKey::parse("src/Main.java").expect("portable key"),
                        b"class Main {}".to_vec(),
                    ),
                    Entry::File(
                        FileKey::parse("deps/child/jals.toml").expect("portable key"),
                        b"[build]\nsource-dirs = [\"src\"]\nclasspath = [\"vendor/classes\"]\n"
                            .to_vec(),
                    ),
                    Entry::File(
                        FileKey::parse("deps/child/vendor/classes/net/example/Api.class")
                            .expect("portable key"),
                        b"class bytes".to_vec(),
                    ),
                    Entry::File(
                        FileKey::parse("deps/child/src/Child.java").expect("portable key"),
                        b"class Child {}".to_vec(),
                    ),
                ])
                .expect("tree is valid"),
            );

            let assembly = ProjectScript::skipped()
                .resolve_memory(
                    &root_manifest(),
                    &mut storage,
                    inert!(),
                    ProjectInputOptions::Compile,
                )
                .await
                .expect("an in-tree path dependency resolves offline");

            // Assert the tree is actually there: an empty compile classpath would satisfy the
            // second assertion for the wrong reason.
            assert!(
                assembly
                    .compile_classpath
                    .iter()
                    .any(|entry| matches!(entry, CompileClasspathEntry::Tree(_))),
                "the classpath directory reaches the compile classpath as a tree: {:?}",
                assembly.compile_classpath
            );
            assert!(
                assembly.task_classpath.is_empty(),
                "a tree's `.class` members are not hierarchy jars: {:?}",
                assembly.task_classpath
            );
        });
    }

    #[test]
    fn script_classpath_directives_land_after_the_authored_entries() {
        block_on_inline(async {
            let manifest: Manifest = "[build]\nclasspath = [\"lib/first.class\"]\n\
                 script = { type = \"rhai\", file = \"build.rhai\" }\n"
                .parse()
                .expect("test manifest is valid");
            let mut storage = MemoryStorage::memory(
                CodeTree::new([
                    Entry::File(
                        FileKey::parse("build.rhai").expect("portable key"),
                        br#"build.add_classpath("lib/second.class");"#.to_vec(),
                    ),
                    Entry::File(
                        FileKey::parse("lib/first.class").expect("portable key"),
                        b"first".to_vec(),
                    ),
                    Entry::File(
                        FileKey::parse("lib/second.class").expect("portable key"),
                        b"second".to_vec(),
                    ),
                ])
                .expect("tree is valid"),
            );

            let script = api::script(
                &jals_exec::Exec::inline(),
                &UnreachableFetcher,
                &mut storage,
                &mut jals_build::build_script::BuildScriptSession::new(),
                RootBuildScriptOptions {
                    manifest: &manifest,
                    environment: &BuildScriptEnvironment::new(),
                    limits: &BuildScriptLimits::default(),
                    host: crate::BuildTaskHost::NoTerminals,
                    blocked_files: &[],
                    publications: crate::SourcePublication::Apply,
                },
            )
            .await
            .expect("a script that only adds a classpath entry succeeds offline");

            let mut augmented = manifest.clone();
            script.augment_classpath(&mut augmented);
            assert_eq!(
                augmented.build.classpath,
                vec!["lib/first.class".to_owned(), "lib/second.class".to_owned()],
                "the script's entry follows the authored one"
            );
            // Idempotent: a second fold must not duplicate what is already there.
            script.augment_classpath(&mut augmented);
            assert_eq!(augmented.build.classpath.len(), 2);
        });
    }
}
