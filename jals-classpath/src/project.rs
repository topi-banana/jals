//! Assembly of classpath inputs from one revisioned project storage aggregate.

use alloc::format;
use alloc::vec::Vec;

use jals_classfile::ClassFile;
use jals_config::{Dependency, FeatureSet, Manifest};
use jals_storage::{CacheBackend, CacheKey, DirKey, FileKey, Name, ProjectStorage, SourceBackend};

use crate::{
    ClasspathEntry, ClasspathLoad, DependencyLocation, DependencyResolver, DependencySpec,
    ExternalLocator, Fetcher, JarExtraction, LibrarySource, SkeletonGroup, Warning, WarningOrigin,
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
    /// Source files already published by a host adapter, such as a native Git checkout.
    pub source_dependency_artifacts: Vec<LibrarySource>,
    pub feature_set: FeatureSet,
}

impl ProjectInputPlan {
    /// Lower a manifest's `[dependencies]` jar entries into this plan — each binary jar plus its
    /// optional `sources` jar — classifying every locator through `classify` (hosts decide what
    /// resolves as a project file versus external content). A non-portable dependency name is
    /// diagnosed into `warnings` and skipped. Shared by the native lowering and the browser host.
    pub fn add_jar_dependencies(
        &mut self,
        manifest: &Manifest,
        mut classify: impl FnMut(&str) -> DependencyLocation,
        warnings: &mut Vec<Warning>,
    ) {
        for (raw_name, dependency) in &manifest.dependencies {
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
                recursive: jar.recursive.unwrap_or(false),
            });
            if let Some(sources) = &jar.sources {
                self.source_archives.push(DependencySpec {
                    name,
                    location: classify(sources),
                    recursive: false,
                });
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
        let resolved_jars = resolved.jars;
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
