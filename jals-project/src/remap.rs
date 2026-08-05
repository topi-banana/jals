//! The post-compile step `[build] remap` names: reobfuscate compiled classes into a jar.
//!
//! The output side of the pipeline `[build] frontend` and `[build] backend` describe. It is the one
//! part of the remap facility that cannot be a task-plan node — a plan runs before the compiler, and
//! this consumes what the compiler produced.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use jals_classpath::{
    Fetcher, JarPackage, JarRemap, MappingResolver, MappingSpec, RemapDirection, RemapRequest,
};
use jals_config::{AmbiguousMapping, BackendKind, Manifest, ResolvedBuildFeatures};
use jals_exec::Exec;
use jals_storage::{
    CacheBackend, CacheKey, CacheNamespace, ContentDigest, DirKey, ProjectStorage, ProjectView,
    ProvenanceFold, RelativePath, SourceBackend,
};

/// Whether this project asks for a post-compile remap, and whether its backend can supply one.
///
/// Absence is a value carrying a reason rather than a failure raised at the end of a doomed
/// pipeline, for the same reason `BackendSelection` is: a host calls this once and never matches on
/// `[build] remap` itself, so the decision table lives in one place.
#[derive(Debug)]
pub enum RemapSelection {
    /// Nothing to do: the manifest declares no `[build] remap`.
    ///
    /// Only that. An unmet `required-features` is [`Requested`](Self::Requested) with no mapping,
    /// because the difference is observable in the output: a declared step writes its jar whether
    /// or not a mapping applies. "This selection ships no mappings" says *do not rewrite the
    /// names*, not *produce nothing* — a release that ships deobfuscated needs the same
    /// distributable as one that does not.
    NotRequested,
    /// Package the compiled classes, reobfuscating first when a mapping set is active.
    Requested(RemapPlan),
    /// Declared, but more than one alternative of the `[mappings]` entry it names is active.
    Ambiguous(AmbiguousMapping),
    /// Declared, but this backend does not produce class files to remap.
    Unsupported {
        /// The backend's `[build] backend` tag.
        backend: &'static str,
        reason: RemapAbsence,
    },
}

/// Why a declared `[build] remap` cannot run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemapAbsence {
    /// The backend emits one WebAssembly module for the whole project rather than class files.
    /// There is no constant pool to rewrite and no jar to write it into.
    NotClassFiles,
}

impl fmt::Display for RemapAbsence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotClassFiles => {
                f.write_str("that backend does not emit class files, so there is nothing to remap")
            }
        }
    }
}

/// A resolved `[build] remap`: which mapping set (if any is active), and where the jar goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemapPlan {
    /// The mapping set, or `None` when no alternative of the named entry is active under this
    /// selection — the jar is packaged and its names are left alone.
    mapping: Option<MappingSpec>,
    /// The `[build] resource-dirs` whose files are packaged alongside the classes, lowered. A
    /// directory that does not exist in the snapshot is skipped when the jar is written.
    resources: Vec<DirKey>,
    /// The output jar, as the project-relative path `[build] remap` resolved to.
    pub jar: String,
}

impl RemapSelection {
    /// The remap step `[build] remap` names, under `features`.
    ///
    /// The single place a host's `[build] remap` question is answered — including the backend arm,
    /// which is exhaustive on purpose so a new backend has to say whether its output is class files
    /// rather than inherit whichever answer the callers assumed.
    pub fn for_manifest(manifest: &Manifest, features: &ResolvedBuildFeatures) -> Self {
        let Some(remap) = &manifest.build.remap else {
            return Self::NotRequested;
        };
        match manifest.build.backend {
            BackendKind::Javac {} | BackendKind::Jals {} => {}
            BackendKind::JalsWasm {} => {
                return Self::Unsupported {
                    backend: manifest.build.backend.tag_name(),
                    reason: RemapAbsence::NotClassFiles,
                };
            }
        }
        let mut warnings = Vec::new();
        // `Manifest::validate` rejects an undeclared reference and every provably ambiguous table,
        // so an unvalidated manifest is the only way past either. A missing or malformed entry
        // still packages: the step was declared, and refusing to write the jar because the *names*
        // could not be resolved would withhold the deliverable over the optional half of it.
        let mapping = match MappingSpec::lower_active(
            manifest,
            &remap.with,
            features.features(),
            &mut warnings,
        ) {
            Ok(mapping) => mapping,
            Err(ambiguous) => return Self::Ambiguous(ambiguous),
        };
        // A `resource-dirs` entry `Manifest::validate` accepted always parses; one that does not is
        // a manifest that reached here unvalidated, and dropping it is the same answer the missing
        // directory below gets.
        let resources = manifest
            .build
            .resource_dirs
            .iter()
            .filter_map(|dir| DirKey::parse(dir).ok())
            .collect();
        Self::Requested(RemapPlan {
            mapping,
            resources,
            jar: remap.jar_path(&manifest.package),
        })
    }
}

impl RemapPlan {
    /// Package `classes` and return the jar's bytes, reobfuscating first when a mapping is active.
    ///
    /// `hierarchy` closes the class hierarchy of what is being remapped, and is read only when one
    /// is: the classes being remapped extend types that live there, and an inherited member whose
    /// declaring type is missing from the index keeps its original name in an otherwise remapped
    /// archive — a silent wrong answer rather than a failure.
    ///
    /// Today a host supplies the archives resolved from `[dependencies]`, which is **not** the whole
    /// compile classpath: a jar a dependency's build script put there with `tasks.add_classpath`,
    /// and a `[build] classpath` entry that is a project file rather than a cached artifact, are
    /// both absent. A mixin-style class that names its target only through an annotation `Class`
    /// value is unaffected — the class table alone renames that — but a member inherited from such
    /// a jar is exactly the silent case above.
    ///
    /// # Errors
    /// A message naming what could not be resolved, packaged, or remapped.
    pub async fn run<F, S, C>(
        &self,
        exec: &Exec,
        fetcher: &F,
        storage: &mut ProjectStorage<S, C>,
        classes: &[(RelativePath, Vec<u8>)],
        hierarchy: &[CacheKey],
        main_class: Option<&str>,
    ) -> Result<Vec<u8>, String>
    where
        F: Fetcher,
        S: SourceBackend,
        C: CacheBackend,
    {
        let view = storage.view();

        // The remapper works on cached archives, so the compiled classes are packaged first. That
        // is not a detour: the jar is the deliverable, and packaging before rather than after means
        // the `Main-Class` in its manifest is rewritten by the same pass that rewrites every other
        // reference to the entry point.
        //
        // Resources ride along in the same archive. They are authored project files, so they come
        // out of the snapshot rather than off a host filesystem, which is what keeps this whole
        // step portable — and a remap leaves every non-class member untouched, so they survive it
        // byte for byte.
        let mut entries = classes.to_vec();
        entries.extend(self.resource_entries(&view));
        let staged = JarPackage::write(&entries, main_class)?;

        // No active mapping is the whole answer: the packaged jar already carries the names the
        // target runtime loads, and its manifest's `Main-Class` is correspondingly left alone
        // because nothing was obfuscated.
        let Some(mapping) = &self.mapping else {
            return Ok(staged);
        };

        let mappings = MappingResolver::text(fetcher, &view, storage.artifacts_mut(), mapping)
            .await
            .map_err(|warning| warning.to_string())?;

        let key = Self::stage_key(&staged);
        storage
            .artifacts_mut()
            .publish(&key, &staged)
            .await
            .map_err(|error| format!("staging the compiled classes failed: {error:?}"))?;

        let remapped = JarRemap::remap(
            exec,
            storage.artifacts_mut(),
            &key,
            &RemapRequest {
                mappings: &mappings,
                format: mapping.format,
                direction: RemapDirection::Reobfuscate,
                hierarchy,
            },
        )
        .await?;
        storage
            .artifacts_mut()
            .lookup(&remapped)
            .await
            .map_err(|error| format!("reading the remapped jar failed: {error:?}"))?
            .ok_or_else(|| "the remapped jar is not cached".to_owned())
    }

    /// Every `[build] resource-dirs` file in `view`, addressed by its path below the directory it
    /// was declared under — exactly as a class is addressed below `classes-dir`.
    ///
    /// Sorted by that path, per directory, because the jar's member order is part of its bytes.
    fn resource_entries(&self, view: &ProjectView) -> Vec<(RelativePath, Vec<u8>)> {
        let mut entries = Vec::new();
        for dir in &self.resources {
            // A declared directory that is not there is not a mistake: `[build] resource-dirs`
            // defaults onto every project, and most projects have no resources.
            if view.directory(dir).is_err() {
                continue;
            }
            let mut found: Vec<_> = view
                .tree()
                .files_under(dir)
                .filter_map(|file| {
                    let path = RelativePath::new(
                        file.key()
                            .path()
                            .segments()
                            .skip(dir.path().segments().len())
                            .cloned(),
                    );
                    (!path.is_root()).then(|| (path, file.bytes().to_vec()))
                })
                .collect();
            found.sort_by_key(|(path, _)| path.to_string());
            entries.extend(found);
        }
        entries
    }

    /// The cache key the staged (pre-remap) jar is published under.
    ///
    /// Content-addressed with no provenance beyond the step's own tag: the same compiled classes
    /// stage to the same artifact whatever produced them, which is what lets a rebuild that changed
    /// nothing reuse the remap through `JarRemap`'s own index.
    fn stage_key(bytes: &[u8]) -> CacheKey {
        let mut fold = ProvenanceFold::new(b"jals.build.remap-stage\0");
        fold.digest(ContentDigest::of(bytes));
        CacheKey::new(
            CacheNamespace::BuildTaskArtifact,
            fold.finish(),
            ContentDigest::of(bytes),
        )
    }
}

/// Namespace for the class bytes a host collects for [`RemapPlan::run`].
pub struct CompiledClasses;

impl CompiledClasses {
    /// Whether a compile's artifacts came back in memory.
    ///
    /// An in-process backend's output *is* its return value; a process-based one wrote its own
    /// through `javac -d` and hands back nothing. The distinction is the host's to resolve because
    /// only a host can read a directory, so this exists to keep the *question* spelled once.
    pub const fn are_in_memory(artifacts: &[(RelativePath, Vec<u8>)]) -> bool {
        !artifacts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(source: &str) -> Manifest {
        source.parse().expect("manifest is valid")
    }

    fn selection(source: &str, selected: &[&str]) -> RemapSelection {
        let manifest = manifest(source);
        let selected: Vec<String> = selected.iter().map(|name| (*name).to_owned()).collect();
        let features = manifest
            .resolve_build_features(&selected, false, false)
            .expect("selection is declared");
        RemapSelection::for_manifest(&manifest, &features)
    }

    const DECLARED: &str = "[build]\nremap = { with = \"mojmap\" }\n\
                            [package]\nname = \"mymod\"\nversion = \"0.1.0\"\n\
                            [mappings.mojmap]\nfile = \"maps/server.txt\"\n";

    #[test]
    fn a_declared_remap_resolves_its_jar_from_the_package() {
        let RemapSelection::Requested(plan) = selection(DECLARED, &[]) else {
            panic!("declared and active");
        };
        assert_eq!(plan.jar, "target/jals/remap/mymod-0.1.0.jar");
    }

    #[test]
    fn no_declaration_is_nothing_to_do() {
        assert!(matches!(
            selection("[package]\nname = \"x\"\n", &[]),
            RemapSelection::NotRequested
        ));
    }

    #[test]
    fn an_inactive_mapping_still_packages_a_jar() {
        // "This selection ships no mappings" is an outcome the manifest states, not a mistake a
        // host reports — and it says *do not rewrite the names*, not *produce nothing*. A release
        // that ships deobfuscated needs the same distributable as one that does not.
        let source = "[features]\nobfuscated = []\n\
                      [build]\nremap = { with = \"mojmap\" }\n\
                      [mappings.mojmap]\nfile = \"maps/server.txt\"\n\
                      required-features = [\"obfuscated\"]\n";
        let RemapSelection::Requested(plan) = selection(source, &[]) else {
            panic!("a declared step is requested whether or not a mapping applies");
        };
        assert!(plan.mapping.is_none());
        let RemapSelection::Requested(plan) = selection(source, &["obfuscated"]) else {
            panic!("declared and active");
        };
        assert!(plan.mapping.is_some());
    }

    #[test]
    fn alternatives_select_by_feature() {
        let source = "[features]\na = []\nb = []\n\
                      [build]\nremap = { with = \"mojmap\" }\n\
                      [[mappings.mojmap]]\nfile = \"maps/a.txt\"\nrequired-features = [\"a\"]\n\
                      [[mappings.mojmap]]\nfile = \"maps/b.txt\"\nrequired-features = [\"b\"]\n";
        let lowered = |selected: &[&str]| {
            let RemapSelection::Requested(plan) = selection(source, selected) else {
                panic!("a declared step is always requested");
            };
            plan.mapping
        };
        let first = lowered(&["a"]).expect("`a` activates the first alternative");
        let second = lowered(&["b"]).expect("`b` activates the second");
        assert_ne!(first, second);
        assert!(lowered(&[]).is_none());
    }

    #[test]
    fn resource_dirs_are_lowered_into_the_plan() {
        let RemapSelection::Requested(plan) = selection(DECLARED, &[]) else {
            panic!("declared and active");
        };
        // The default, lowered — the plan carries the directories rather than the host re-reading
        // `[build] resource-dirs` when it packages.
        assert_eq!(
            plan.resources,
            alloc::vec![DirKey::parse("src/main/resources").expect("constant is portable")]
        );

        let none = "[build]\nremap = { with = \"m\" }\nresource-dirs = []\n\
                    [mappings.m]\nfile = \"maps/server.txt\"\n";
        let RemapSelection::Requested(plan) = selection(none, &[]) else {
            panic!("declared and active");
        };
        assert!(plan.resources.is_empty());
    }

    #[test]
    fn an_ambiguous_selection_is_reported() {
        // `Manifest::validate` rejects a table where this is provable, so it takes an unvalidated
        // manifest to get here — and the host is told rather than handed whichever came first.
        let source = "[features]\na = []\nb = []\n\
                      [build]\nremap = { with = \"mojmap\" }\n\
                      [[mappings.mojmap]]\nfile = \"maps/a.txt\"\nrequired-features = [\"a\"]\n\
                      [[mappings.mojmap]]\nfile = \"maps/b.txt\"\nrequired-features = [\"b\"]\n";
        let RemapSelection::Ambiguous(ambiguous) = selection(source, &["a", "b"]) else {
            panic!("two active alternatives are ambiguous");
        };
        assert_eq!(ambiguous.name, "mojmap");
        assert_eq!((ambiguous.first, ambiguous.second), (1, 2));
    }

    #[test]
    fn a_wasm_backend_has_no_class_files_to_remap() {
        // Absence with a reason, not a failure raised later: one module for the whole project has
        // no constant pool to rewrite, and a host should learn that before it compiles.
        let source = "[build]\nbackend = { type = \"jals-wasm\" }\n\
                      remap = { with = \"mojmap\" }\n\
                      [mappings.mojmap]\nfile = \"maps/server.txt\"\n";
        let RemapSelection::Unsupported { backend, reason } = selection(source, &[]) else {
            panic!("wasm cannot remap");
        };
        assert_eq!(backend, "jals-wasm");
        assert_eq!(reason, RemapAbsence::NotClassFiles);
    }
}
