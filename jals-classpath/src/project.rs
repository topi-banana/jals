//! Assembly of classpath inputs from one revisioned project storage aggregate.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::vec::Vec;

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};

use jals_classfile::ClassFile;
use jals_config::{Dependency, FeatureSet, Manifest, ResolvedBuildFeatures};
use jals_storage::{
    CacheBackend, CacheKey, DirKey, EntryRef, FileKey, Name, ProjectStorage, ProjectView,
    RelativePath, SourceBackend,
};

use crate::{
    ClasspathEntry, ClasspathLoad, DependencyLocation, DependencyResolver, DependencySpec,
    ExternalLocator, Fetcher, JarExtraction, JarRemap, LibrarySource, MappingResolver, MappingSpec,
    RemapDirection, RemapRequest, SkeletonGroup, Warning, WarningOrigin,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectInputOptions {
    Analysis,
    Compile,
    Editor,
}

/// Typed, already-classified input plan. Host path and URI conversion happens before this boundary.
#[derive(Debug, Clone, Default)]
pub struct ProjectInputPlan {
    pub dependencies: Vec<DependencySpec>,
    pub source_archives: Vec<DependencySpec>,
    pub classpath: Vec<ClasspathEntry>,
    pub source_dependency_roots: Vec<DirKey>,
    /// Source files already published by a host adapter, such as a native Git checkout, or a
    /// dependency build task's `publish_tree` output declared as a compile input.
    pub source_dependency_artifacts: Vec<LibrarySource>,
    /// Navigation-only sources already published into the verified cache, such as a dependency
    /// build task's `publish_tree` output declared for reading. Unlike
    /// [`source_dependency_artifacts`](Self::source_dependency_artifacts) these are never handed to
    /// the compiler — they exist so a reader can open the real source behind a classpath type.
    ///
    /// That is a contract, not an implementation detail: a dependency exports its types through the
    /// classpath, and such a publication is a *view* of types defined there. Handing `javac` both a
    /// decompiled tree and the jar it was decompiled from is how a working build acquires
    /// duplicates, and by the time publications are flattened to here nothing could tell one from
    /// another anyway. A tree that is the only carrier of its package says so in the script that
    /// publishes it and arrives in the field above instead.
    ///
    /// The premise the contract rests on — that something on the classpath carries the same types —
    /// is checked where it is still attributable, in `jals-project`'s preprocessing, which warns
    /// against the declaration when nothing does.
    pub library_source_artifacts: Vec<LibrarySource>,
    pub feature_set: FeatureSet,
}

impl ProjectInputPlan {
    /// Lower a manifest's `[dependencies]` jar entries into this plan — each binary jar plus its
    /// optional `sources` jar — classifying every locator through `classify` (hosts decide what
    /// resolves as a project file versus external content). A non-portable dependency name is
    /// diagnosed into `warnings` and skipped. Shared by the native lowering and the browser host.
    pub(crate) fn add_jar_dependencies(
        &mut self,
        manifest: &Manifest,
        features: &ResolvedBuildFeatures,
        mut classify: impl FnMut(&str) -> DependencyLocation,
        warnings: &mut Vec<Warning>,
    ) {
        for (raw_name, dependency) in manifest.active_dependencies(features) {
            let Dependency::Jar(jar) = dependency else {
                continue;
            };
            let name = match Name::new(raw_name) {
                Ok(name) => name,
                Err(error) => {
                    warnings.push(Warning::new(
                        WarningOrigin::External(ExternalLocator::new(raw_name)),
                        format!("dependency name is not a portable name: {error:?}"),
                    ));
                    continue;
                }
            };
            self.dependencies.push(DependencySpec {
                name: name.clone(),
                location: classify(&jar.jar),
                remap: Self::active_remap(manifest, features, jar.remap.as_deref(), warnings),
                recursive: jar.recursive.unwrap_or(false),
            });
            if let Some(sources) = &jar.sources {
                self.source_archives.push(DependencySpec {
                    name,
                    location: classify(sources),
                    // A sources jar is never remapped; the manifest rejects declaring both.
                    remap: None,
                    recursive: false,
                });
            }
        }
    }

    /// The mapping set a `remap` reference resolves to under `features`, or `None` when no
    /// alternative of the entry is active.
    ///
    /// The gate and the lowering are deliberately separate calls: `jals-project` applies the same
    /// gate with a *graph node's* features rather than these, and duplicating the lowering there is
    /// what would let the two drift.
    ///
    /// An ambiguous entry drops the jar, exactly as an unresolvable mapping does below: an archive
    /// nobody can say which names it answers to is not a degraded version of what was asked for.
    fn active_remap(
        manifest: &Manifest,
        features: &ResolvedBuildFeatures,
        reference: Option<&str>,
        warnings: &mut Vec<Warning>,
    ) -> Option<MappingSpec> {
        let reference = reference?;
        match MappingSpec::lower_active(manifest, reference, features.features(), warnings) {
            Ok(spec) => spec,
            Err(ambiguous) => {
                warnings.push(Warning::new(
                    WarningOrigin::External(ExternalLocator::new(reference)),
                    ambiguous.to_string(),
                ));
                None
            }
        }
    }
}

/// A source dependency read either from the captured project revision or from the verified cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceFile {
    Project(FileKey),
    Artifact(LibrarySource),
}

#[derive(Debug, Default)]
pub struct ProjectInputs {
    pub dependency_jars: Vec<CacheKey>,
    pub classpath_classes: Vec<ClassFile>,
    pub library_sources: Vec<LibrarySource>,
    pub source_dep_sources: Vec<SourceFile>,
    pub feature_set: FeatureSet,
    pub warnings: Vec<Warning>,
}

impl ProjectInputs {
    /// Execute the plan against one immutable view. Cache publication does not mutate that view or
    /// advance the source revision. Fan-out work runs on the storage's own execution context.
    pub async fn assemble<F, S, C>(
        fetcher: &F,
        storage: &mut ProjectStorage<S, C>,
        plan: &ProjectInputPlan,
        options: ProjectInputOptions,
    ) -> Self
    where
        F: Fetcher,
        S: SourceBackend,
        C: CacheBackend,
    {
        use ProjectInputOptions::{Analysis, Compile, Editor};

        let exec = storage.exec().clone();
        let view = storage.view();
        let (want_sources, want_source_deps, want_classes, want_skeletons) = match options {
            Analysis => (false, false, true, false),
            Compile => (false, true, false, false),
            Editor => (true, true, true, true),
        };

        let resolved = DependencyResolver::resolve(
            fetcher,
            &view,
            storage.artifacts_mut(),
            &plan.dependencies,
        )
        .await;
        let mut warnings = resolved.warnings;

        // Deobfuscate every jar whose entry declared a `remap`, before anything reads one. Doing it
        // here rather than at each consumer is what makes the classpath, the analysis index, and the
        // skeletons an editor synthesizes agree on one set of names — and it happens before the
        // nested expansion below, so a fat jar's bundled members are unpacked from the remapped
        // archive rather than the original.
        //
        // A mapping that cannot be resolved drops its jar instead of admitting it unremapped. The
        // unremapped archive is not a degraded version of what was asked for: every type in it
        // answers to a name nothing else in the build uses, so the `cannot find symbol` a missing
        // entry produces points at the real problem and obfuscated names would not.
        let mut resolved_jars = Vec::with_capacity(resolved.jars.len());
        for mut jar in resolved.jars {
            let Some(spec) = plan
                .dependencies
                .iter()
                .find(|spec| spec.name == jar.name)
                .and_then(|spec| spec.remap.as_ref())
            else {
                resolved_jars.push(jar);
                continue;
            };
            let text =
                match MappingResolver::text(fetcher, &view, storage.artifacts_mut(), spec).await {
                    Ok(text) => text,
                    Err(warning) => {
                        warnings.push(warning);
                        continue;
                    }
                };
            let request = RemapRequest {
                mappings: &text,
                format: spec.format,
                direction: RemapDirection::Deobfuscate,
                // A dependency jar closes over its own hierarchy: it is the artifact the mapping
                // set was published for. Reobfuscating compiled output is the case that needs the
                // classpath, and that is a different caller.
                hierarchy: &[],
            };
            match JarRemap::remap(&exec, storage.artifacts_mut(), &jar.key, &request).await {
                Ok(key) => {
                    jar.key = key;
                    resolved_jars.push(jar);
                }
                Err(error) => warnings.push(Warning::new(
                    WarningOrigin::Artifact(jar.key.clone()),
                    format!("dependency `{}` could not be remapped: {error}", jar.name),
                )),
            }
        }

        // Keep top-level dependencies in request order; recursive members are additions appended in
        // the same second-pass order.
        let mut dependency_jars: Vec<_> = resolved_jars.iter().map(|jar| jar.key.clone()).collect();
        for jar in resolved_jars.iter().filter(|jar| jar.recursive) {
            let nested = JarExtraction::nested(&exec, storage.artifacts_mut(), &jar.key).await;
            warnings.extend(nested.warnings);
            dependency_jars.extend(nested.artifacts.into_iter().map(|artifact| artifact.key));
        }

        let mut library_sources = Vec::new();
        if want_sources {
            let source_jars = DependencyResolver::resolve(
                fetcher,
                &view,
                storage.artifacts_mut(),
                &plan.source_archives,
            )
            .await;
            warnings.extend(source_jars.warnings);
            let keys: Vec<_> = source_jars.jars.into_iter().map(|jar| jar.key).collect();
            let extracted =
                JarExtraction::<LibrarySource>::sources(&exec, storage.artifacts_mut(), &keys)
                    .await;
            warnings.extend(extracted.warnings);
            library_sources.extend(extracted.artifacts);
            // Already-published navigation sources sit between the real `sources` jars above and
            // the synthesized skeletons below, which is exactly their standing: closer to the
            // truth than a rendered skeleton, further from it than the library's own `.java`.
            library_sources.extend(plan.library_source_artifacts.iter().cloned());
        }

        let source_dep_sources = if want_source_deps {
            let mut files = Vec::new();
            for root in &plan.source_dependency_roots {
                if let Err(error) = view.directory(root) {
                    warnings.push(Warning::new(
                        WarningOrigin::ProjectDirectory(root.clone()),
                        format!("source dependency root cannot be read: {error}"),
                    ));
                    continue;
                }
                files.extend(
                    view.tree()
                        .files_under(root)
                        .filter(|file| file.key().has_extension("java"))
                        .map(|file| SourceFile::Project(file.key().clone())),
                );
            }
            files.extend(
                plan.source_dependency_artifacts
                    .iter()
                    .cloned()
                    .map(SourceFile::Artifact),
            );
            files
        } else {
            Vec::new()
        };

        let classpath_classes = if want_classes {
            let mut entries = plan.classpath.clone();
            entries.extend(
                dependency_jars
                    .iter()
                    .cloned()
                    .map(ClasspathEntry::Artifact),
            );
            let load = ClasspathLoad::load(&exec, &view, storage.artifacts(), &entries).await;
            warnings.extend(load.warnings);
            load.classes
        } else {
            Vec::new()
        };

        if want_skeletons {
            let skeletons =
                SkeletonGroup::synthesize(storage.artifacts_mut(), &classpath_classes).await;
            warnings.extend(skeletons.warnings);
            library_sources.extend(skeletons.sources);
        }
        // One navigation path, one artifact. The three producers above all address a type by the
        // same package-relative path, so a class carried by more than one of them arrives more than
        // once; a host that mounts them (the LSP overlays each at `.jals/library/<path>`) would
        // otherwise resolve the collision by mount order, which is not a decision worth leaving to
        // whichever `set_overlays` call happens to run last. Keeping the first occurrence fixes the
        // precedence to the collection order: a library's own `.java`, then a published tree, then
        // a synthesized skeleton.
        let mut seen = BTreeSet::new();
        library_sources.retain(|source| seen.insert(source.path.clone()));

        Self {
            dependency_jars,
            classpath_classes,
            library_sources,
            source_dep_sources,
            feature_set: plan.feature_set,
            warnings,
        }
    }
}

/// Portable lowering of a manifest's `[build]` section into the classpath plan — the in-memory
/// sibling of [`NativeProjectPlan`](crate::NativeProjectPlan).
///
/// Host paths never enter here, so there is no external fallback to lower them into: an in-memory
/// project has exactly one address space, and an entry reaching outside it names nothing that could
/// be read. Such an entry is a warning.
///
/// That is also why the lowered halves stay private while [`NativeProjectPlan`]'s are public: a
/// native caller has to materialize git and out-of-tree `path` sources *between* lowering and
/// assembly, so it needs the plan in hand. Nothing comes between the two steps here, so
/// [`assemble`](Self::assemble) is the whole surface — and a consumer reaching past it for
/// `from_manifest` plus the fields would be re-implementing exactly the lowering this type exists to
/// be the only copy of.
///
/// [`NativeProjectPlan`]: crate::NativeProjectPlan
#[derive(Debug, Default)]
pub struct MemoryProjectPlan {
    plan: ProjectInputPlan,
    source_roots: Vec<DirKey>,
    warnings: Vec<Warning>,
}

impl MemoryProjectPlan {
    /// Lower and execute the whole portable input assembly against one aggregate, merging the
    /// lowering warnings into the result's. Returns the resolved inputs plus the manifest's source
    /// roots.
    ///
    /// `manifest` is the root manifest as written. A caller does *not* strip `[dependencies]` first:
    /// this lowering never reads that table (see [`from_manifest`](Self::from_manifest)), so a caller
    /// that cleared it would be restating the rule somewhere a reader cannot check it — and would
    /// clone a whole manifest to do it.
    pub async fn assemble<F, S, C>(
        manifest: &Manifest,
        storage: &mut ProjectStorage<S, C>,
        fetcher: &F,
        options: ProjectInputOptions,
    ) -> (ProjectInputs, Vec<DirKey>)
    where
        F: Fetcher,
        S: SourceBackend,
        C: CacheBackend,
    {
        let mut lowered = Self::from_manifest(manifest, &storage.view());
        let mut inputs = ProjectInputs::assemble(fetcher, storage, &lowered.plan, options).await;
        lowered.warnings.append(&mut inputs.warnings);
        inputs.warnings = lowered.warnings;
        (inputs, lowered.source_roots)
    }

    /// Lower `manifest`'s `[build] source_dirs` and `[build] classpath` against one immutable view.
    ///
    /// `[dependencies]` are deliberately *not* lowered, and this is the only place that decides so:
    /// a caller assembling a dependency graph projects each declared dependency as a graph node, so
    /// lowering them here as well would resolve every jar twice and double-count it on the classpath.
    /// Anything added here that reads `manifest.dependencies` breaks callers that hand over the
    /// manifest whole, which is all of them — `declared_dependencies_are_left_to_the_graph` is what
    /// holds the line.
    fn from_manifest(manifest: &Manifest, view: &ProjectView) -> Self {
        let mut result = Self {
            plan: ProjectInputPlan {
                feature_set: manifest.feature_set(),
                ..ProjectInputPlan::default()
            },
            source_roots: Vec::new(),
            warnings: Vec::new(),
        };

        for source in &manifest.build.source_dirs {
            match Self::project_relative(source) {
                Ok(path) => result.source_roots.push(DirKey::new(path)),
                Err(message) => result.warn_path(source, message),
            }
        }
        for classpath in &manifest.build.classpath {
            let path = match Self::project_relative(classpath) {
                Ok(path) => path,
                Err(message) => {
                    result.warn_path(classpath, message);
                    continue;
                }
            };
            // A file and a directory can share neither a path nor a key, so probing both is
            // unambiguous; the file probe runs first because `FileKey::new` rejects the root.
            let found = FileKey::new(path.clone())
                .ok()
                .and_then(|key| view.tree().lookup_file(&key))
                .or_else(|| view.tree().lookup_dir(&DirKey::new(path)));
            match found {
                Some(EntryRef::File(file)) => result
                    .plan
                    .classpath
                    .push(ClasspathEntry::ProjectFile(file.key().clone())),
                Some(EntryRef::Directory(directory)) => result
                    .plan
                    .classpath
                    .push(ClasspathEntry::ProjectDirectory(directory.clone())),
                None => result.warn_path(
                    classpath,
                    "classpath entry is missing or invalid".to_owned(),
                ),
            }
        }

        result.source_roots.sort();
        result.source_roots.dedup();
        result
    }

    /// One `[build]` entry, resolved against the project root.
    ///
    /// The fold itself is [`RelativePath::resolve`]'s — it is shared with `jals-project`'s graph
    /// discovery, which resolves the same spellings against a *dependency's* root. What is this
    /// crate's own is the base: an in-memory project has one address space and no host path to
    /// fall back on, so a path leaving the root is an error here rather than an external entry.
    fn project_relative(raw: &str) -> Result<RelativePath, String> {
        RelativePath::resolve(&RelativePath::ROOT, raw).map_err(|error| error.to_string())
    }

    fn warn_path(&mut self, path: &str, message: String) {
        self.warnings.push(Warning::new(
            WarningOrigin::External(ExternalLocator::new(path)),
            message,
        ));
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use jals_storage::{CodeTree, Entry, MemoryStorage};

    use super::*;

    fn class_bytes() -> &'static [u8] {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/Box.class"
        ))
    }

    fn manifest_with_classpath(entries: &[&str]) -> Manifest {
        let mut manifest = Manifest::default();
        manifest.build.classpath = entries.iter().copied().map(ToOwned::to_owned).collect();
        manifest
    }

    #[test]
    fn root_classpath_files_and_directories_lower_in_manifest_order() {
        let root_file = FileKey::parse("lib/Box.class").expect("portable key");
        let directory_file = FileKey::parse("classes/Box.class").expect("portable key");
        let script_file = FileKey::parse("generated/Script.class").expect("portable key");
        let storage = MemoryStorage::memory(
            CodeTree::new([
                Entry::File(root_file.clone(), class_bytes().to_vec()),
                Entry::File(directory_file, class_bytes().to_vec()),
                Entry::File(script_file.clone(), class_bytes().to_vec()),
            ])
            .expect("tree is valid"),
        );
        // `generated/Script.class` stands in for a build script's `add_classpath`: a host folds the
        // script's entries onto the manifest, so they lower by this same rule and land after the
        // authored ones.
        let manifest =
            manifest_with_classpath(&["lib/./Box.class", "classes", "generated/Script.class"]);

        let lowered = MemoryProjectPlan::from_manifest(&manifest, &storage.view());

        assert!(lowered.warnings.is_empty(), "{:?}", lowered.warnings);
        assert_eq!(
            lowered.plan.classpath,
            vec![
                ClasspathEntry::ProjectFile(root_file),
                ClasspathEntry::ProjectDirectory(DirKey::parse("classes").expect("portable key")),
                ClasspathEntry::ProjectFile(script_file),
            ],
            "entries keep the order the manifest spells them in"
        );
    }

    #[test]
    fn malformed_root_classpath_entries_warn_in_manifest_order() {
        let storage = MemoryStorage::memory(CodeTree::default());
        let manifest =
            manifest_with_classpath(&["../escape.class", "bad:name.class", "missing.class"]);

        let lowered = MemoryProjectPlan::from_manifest(&manifest, &storage.view());

        assert!(lowered.plan.classpath.is_empty());
        let messages: Vec<_> = lowered
            .warnings
            .iter()
            .map(|warning| warning.message.clone())
            .collect();
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(
            messages[0].contains("leaves the project root"),
            "{messages:?}"
        );
        assert!(messages[1].contains("invalid segment"), "{messages:?}");
        assert!(messages[2].contains("missing or invalid"), "{messages:?}");
        // None of the three messages names the offending entry, so the locator is the only thing
        // that does: it has to be the entry the user wrote rather than the normalized path.
        let origins: Vec<_> = lowered
            .warnings
            .iter()
            .map(|warning| warning.origin.clone())
            .collect();
        assert_eq!(
            origins,
            vec![
                WarningOrigin::External(ExternalLocator::new("../escape.class")),
                WarningOrigin::External(ExternalLocator::new("bad:name.class")),
                WarningOrigin::External(ExternalLocator::new("missing.class")),
            ]
        );
        // And a host prints that locator, because it renders the whole warning. Asserted here
        // rather than left to each host: three of them report these, and the reason the message
        // alone is not enough is a property of the warning, not of any one host.
        let rendered: Vec<_> = lowered.warnings.iter().map(ToString::to_string).collect();
        assert_eq!(
            rendered,
            vec![
                "`../escape.class`: path leaves the project root".to_owned(),
                "`bad:name.class`: path contains an invalid segment: WindowsReservedCharacter"
                    .to_owned(),
                "`missing.class`: classpath entry is missing or invalid".to_owned(),
            ]
        );
    }

    #[test]
    fn source_dirs_lower_to_sorted_deduplicated_roots() {
        let storage = MemoryStorage::memory(CodeTree::default());
        let mut manifest = Manifest::default();
        manifest.build.source_dirs = vec![
            "src/main/java".to_owned(),
            "./src/main/java".to_owned(),
            "generated".to_owned(),
            "../outside".to_owned(),
        ];

        let lowered = MemoryProjectPlan::from_manifest(&manifest, &storage.view());

        assert_eq!(
            lowered.source_roots,
            vec![
                DirKey::parse("generated").expect("portable key"),
                DirKey::parse("src/main/java").expect("portable key"),
            ],
            "`.` normalizes to the same root and duplicates collapse"
        );
        assert_eq!(lowered.warnings.len(), 1);
        assert!(
            lowered.warnings[0]
                .message
                .contains("leaves the project root"),
            "{:?}",
            lowered.warnings
        );
    }

    /// Guards a *caller's* invariant, not merely a local one: `ProjectScript::resolve_memory` hands
    /// its root manifest over with `[dependencies]` still in place, on the strength of this.
    #[test]
    fn declared_dependencies_are_left_to_the_graph() {
        let storage = MemoryStorage::memory(CodeTree::default());
        let manifest: Manifest = "[dependencies]\nlib = { jar = \"vendor/lib.jar\" }\n\
             child = { path = \"deps/child\" }\n"
            .parse()
            .expect("manifest parses");

        let lowered = MemoryProjectPlan::from_manifest(&manifest, &storage.view());

        assert!(lowered.plan.dependencies.is_empty());
        assert!(lowered.plan.source_archives.is_empty());
        assert!(lowered.plan.source_dependency_roots.is_empty());
        assert!(lowered.warnings.is_empty(), "{:?}", lowered.warnings);
    }
}
