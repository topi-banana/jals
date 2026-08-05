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
use jals_config::{BackendKind, Manifest, ResolvedBuildFeatures};
use jals_exec::Exec;
use jals_storage::{
    CacheBackend, CacheKey, CacheNamespace, ContentDigest, ProjectStorage, ProvenanceFold,
    RelativePath, SourceBackend,
};

/// Whether this project asks for a post-compile remap, and whether its backend can supply one.
///
/// Absence is a value carrying a reason rather than a failure raised at the end of a doomed
/// pipeline, for the same reason `BackendSelection` is: a host calls this once and never matches on
/// `[build] remap` itself, so the decision table lives in one place.
#[derive(Debug)]
pub enum RemapSelection {
    /// Nothing to do: the manifest declares no `[build] remap`, or the mapping set it names is
    /// inactive under this selection.
    ///
    /// The two collapse deliberately. An unmet `required-features` is how a manifest says "this
    /// selection ships no mappings", and a host that had to tell the two apart would be deciding
    /// something the manifest already decided.
    NotRequested,
    /// Reobfuscate the compiled classes and write the jar.
    Requested(RemapPlan),
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

/// A resolved `[build] remap`: which mapping set, and where the jar goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemapPlan {
    mapping: MappingSpec,
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
        let Some(source) = manifest.mappings.get(&remap.with) else {
            // `Manifest::validate` rejects an undeclared reference, so reaching this is a manifest
            // that was never validated. Nothing to remap with is nothing to do.
            return Self::NotRequested;
        };
        if !source.is_active(features.features()) {
            return Self::NotRequested;
        }
        let mut warnings = Vec::new();
        let Some(mapping) = MappingSpec::lower(manifest, &remap.with, &mut warnings) else {
            return Self::NotRequested;
        };
        Self::Requested(RemapPlan {
            mapping,
            jar: remap.jar_path(&manifest.package),
        })
    }
}

impl RemapPlan {
    /// Reobfuscate `classes` and return the jar's bytes.
    ///
    /// `hierarchy` is the resolved compile classpath. It is not optional in practice: the classes
    /// being remapped extend types that live there, and an inherited member whose declaring type is
    /// missing from the index keeps its original name in an otherwise remapped archive — a silent
    /// wrong answer rather than a failure.
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
        let mappings =
            MappingResolver::text(fetcher, &view, storage.artifacts_mut(), &self.mapping)
                .await
                .map_err(|warning| warning.to_string())?;

        // The remapper works on cached archives, so the compiled classes are packaged first. That
        // is not a detour: the jar is the deliverable, and packaging before rather than after means
        // the `Main-Class` in its manifest is rewritten by the same pass that rewrites every other
        // reference to the entry point.
        let staged = JarPackage::write(classes, main_class)?;
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
                format: self.mapping.format,
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
    fn an_unmet_required_feature_is_a_skip_rather_than_a_failure() {
        // The same rule `dependencies.*.remap` follows: "this selection ships no mappings" is an
        // outcome the manifest states, not a mistake a host reports.
        let source = "[features]\nobfuscated = []\n\
                      [build]\nremap = { with = \"mojmap\" }\n\
                      [mappings.mojmap]\nfile = \"maps/server.txt\"\n\
                      required-features = [\"obfuscated\"]\n";
        assert!(matches!(
            selection(source, &[]),
            RemapSelection::NotRequested
        ));
        assert!(matches!(
            selection(source, &["obfuscated"]),
            RemapSelection::Requested(_)
        ));
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
