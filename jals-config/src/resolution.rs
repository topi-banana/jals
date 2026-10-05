//! Lowering a validated manifest into the resolver's vocabulary.
//!
//! This is the pure seam between `jals.toml` and `jals-resolve`: a [`Manifest`] becomes a
//! [`Summary`] (its dependency edges and its feature graph) plus the [`DependencyRequest`]s the
//! resolver walks. Nothing here reads a file, resolves a path, or touches a host: a `path`
//! dependency's location stays the string the manifest wrote, and interpreting it belongs to the
//! provider that knows which directory the declaring project was found in.
//!
//! The existing dependency forms lower as follows; the registry/git/dir source schema that
//! replaces them keeps this mapping in one place:
//!
//! | manifest form | resolver source |
//! | --- | --- |
//! | `jar` | [`SourceRequest::Direct`] with [`DirectKind::Jar`] |
//! | `wasm` | [`SourceRequest::Direct`] with [`DirectKind::Wasm`] |
//! | `git` | [`SourceRequest::Git`] (the `branch`/`tag`/`rev` selection classified by [`GitRef`]) |
//! | `path` | [`SourceRequest::Path`] |
//!
//! Version requirements are `*`: no current form declares one. Direct binaries are leaves — the
//! resolver carries them as packages so a lockfile can pin their checksums, but they contribute
//! no transitive edges. A companion `sources` jar is not a resolver package; it is the same
//! dependency's navigation artifact and stays a graph concern.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use jals_resolve::id::{DirectKind, NameError, PackageId, PackageName, RegistryId};
use jals_resolve::summary::{
    DependencyKind, DependencyRequest, FeatureValue, GitReference, SourceRequest, Summary,
};
use jals_resolve::version::VersionReq;

use crate::manifest::{Dependency, DependencyError, FeatureRef, GitRef, Manifest};

/// A dependency entry that cannot be lowered into a resolver request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveLowerError {
    /// The dependency's name (the `[dependencies]` key) is not a valid package name.
    Name {
        /// The offending key.
        name: String,
        /// Why it was rejected.
        reason: NameError,
    },
    /// The dependency's own value checks failed at lowering time.
    Dependency(DependencyError),
    /// A registry entry's `version` is not a version requirement.
    Version {
        /// The dependency's name.
        name: String,
        /// The parse failure.
        reason: jals_resolve::version::VersionError,
    },
    /// A registry entry names neither a `group` nor a `group:artifact` key.
    Coordinate {
        /// The dependency's name.
        name: String,
    },
    /// A registry entry names a malformed registry.
    Registry {
        /// The registry name as written.
        name: String,
        /// Why it was rejected.
        reason: NameError,
    },
}

impl fmt::Display for ResolveLowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name { name, reason } => {
                write!(
                    f,
                    "dependency name `{name}` is not a package name: {reason}"
                )
            }
            Self::Dependency(error) => error.fmt(f),
            Self::Version { name, reason } => {
                write!(f, "dependency `{name}` has an invalid version: {reason}")
            }
            Self::Coordinate { name } => write!(
                f,
                "registry dependency `{name}` names no coordinate (write `group = \"…\"` or use a `group:artifact` key)"
            ),
            Self::Registry { name, reason } => {
                write!(f, "`{name}` is not a valid registry name: {reason}")
            }
        }
    }
}

impl core::error::Error for ResolveLowerError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Name { reason, .. } | Self::Registry { reason, .. } => Some(reason),
            Self::Dependency(error) => Some(error),
            Self::Version { reason, .. } => Some(reason),
            Self::Coordinate { .. } => None,
        }
    }
}

impl Manifest {
    /// The resolver summary for this project under the identity its host assigned it.
    ///
    /// The host owns the id because identity is host-language: the native host names a member or
    /// canonical path, the in-memory host a project-relative location. This crate only states
    /// what the manifest contributes — edges and the feature graph.
    ///
    /// # Errors
    /// [`ResolveLowerError`] for a `[dependencies]` key that is not a package name, or a `git`
    /// entry whose refs are contradictory (a manifest that reached here unvalidated).
    pub fn resolver_summary(&self, id: PackageId) -> Result<Summary, ResolveLowerError> {
        Ok(Summary {
            id,
            dependencies: self.resolver_dependencies()?,
            features: self.resolver_features(),
        })
    }

    /// This project's dependency edges, `[dependencies]` then `[dev-dependencies]`, in name
    /// order within each table.
    ///
    /// Both tables are lowered: the [`DependencyKind`] on each request is what tells the
    /// resolver that a dev edge exists only for a root and never transitively. The
    /// [`DependencyScope`](crate::DependencyScope) a host states is therefore *derived* by the
    /// resolver from the edge kind rather than filtered here, so one lowering serves `jals
    /// build` and `jals test`.
    ///
    /// # Errors
    /// [`ResolveLowerError`] as in [`resolver_summary`](Manifest::resolver_summary).
    pub fn resolver_dependencies(&self) -> Result<Vec<DependencyRequest>, ResolveLowerError> {
        let mut requests =
            Vec::with_capacity(self.dependencies.len() + self.dev_dependencies.len());
        for (label, dependency) in &self.dependencies {
            requests.push(dependency.request(label, DependencyKind::Normal)?);
        }
        for (label, dependency) in &self.dev_dependencies {
            requests.push(dependency.request(label, DependencyKind::Development)?);
        }
        Ok(requests)
    }

    /// The `[features]` table as a resolver feature graph.
    ///
    /// Infallible by design: a malformed entry becomes an opaque local feature name that expands
    /// to nothing, which is exactly how `expand_build_features` treats one when it arrives in a
    /// dependency's set — and `validate` has already rejected every malformed shape a manifest
    /// can write, so this only decides what to do with a manifest that never passed it.
    pub fn resolver_features(&self) -> BTreeMap<String, Vec<FeatureValue>> {
        fn feature_value(entry: &str) -> FeatureValue {
            match FeatureRef::parse(entry) {
                Ok(FeatureRef::Local(name)) => FeatureValue::Feature(name.to_owned()),
                Ok(FeatureRef::Activation(dependency)) => PackageName::new(dependency).map_or_else(
                    |_| FeatureValue::Feature(entry.to_owned()),
                    FeatureValue::Dependency,
                ),
                Ok(FeatureRef::Dependency {
                    dependency,
                    feature,
                }) => PackageName::new(dependency).map_or_else(
                    |_| FeatureValue::Feature(entry.to_owned()),
                    |dependency| FeatureValue::DependencyFeature {
                        dependency,
                        feature: feature.to_owned(),
                    },
                ),
                Err(_) => FeatureValue::Feature(entry.to_owned()),
            }
        }
        self.features
            .iter()
            .map(|(name, entries)| {
                (
                    name.clone(),
                    entries.iter().map(|entry| feature_value(entry)).collect(),
                )
            })
            .collect()
    }
}

impl Dependency {
    /// One dependency entry lowered into a resolver request.
    ///
    /// `label` is the `[dependencies]` key. `kind` says which table it came from.
    fn request(
        &self,
        label: &str,
        kind: DependencyKind,
    ) -> Result<DependencyRequest, ResolveLowerError> {
        let name = PackageName::new(label).map_err(|reason| ResolveLowerError::Name {
            name: label.to_owned(),
            reason,
        })?;
        let any = || VersionReq::parse("*").expect("`*` is a valid requirement");
        let (package, source, version) = match self {
            Self::Jar(jar) => (
                name.clone(),
                SourceRequest::Direct {
                    kind: DirectKind::Jar,
                    locator: jar.jar.clone(),
                },
                any(),
            ),
            Self::Wasm(wasm) => (
                name.clone(),
                SourceRequest::Direct {
                    kind: DirectKind::Wasm,
                    locator: wasm.wasm.clone(),
                },
                any(),
            ),
            Self::Git(git) => {
                let reference = match git.git_ref(label).map_err(ResolveLowerError::Dependency)? {
                    GitRef::Default => GitReference::Default,
                    GitRef::Branch(value) => GitReference::Branch(value),
                    GitRef::Tag(value) => GitReference::Tag(value),
                    GitRef::Rev(value) => GitReference::Rev(value),
                };
                (
                    name.clone(),
                    SourceRequest::Git {
                        url: git.git.clone(),
                        reference,
                        dir: git.dir.clone(),
                    },
                    any(),
                )
            }
            Self::Path(path) => (
                name.clone(),
                SourceRequest::Path {
                    location: path.path.clone(),
                    dir: path.dir.clone(),
                },
                any(),
            ),
            Self::Registry(_) | Self::RegistryVersion(_) => {
                let coordinate = self.registry_coordinate(label).ok_or_else(|| {
                    ResolveLowerError::Coordinate {
                        name: label.to_owned(),
                    }
                })?;
                let package =
                    PackageName::new(coordinate).map_err(|reason| ResolveLowerError::Name {
                        name: label.to_owned(),
                        reason,
                    })?;
                let raw = self.registry_version().unwrap_or("*");
                let version =
                    VersionReq::parse(raw).map_err(|reason| ResolveLowerError::Version {
                        name: label.to_owned(),
                        reason,
                    })?;
                let registry_name = self.registry_name().unwrap_or(RegistryId::MAVEN_CENTRAL);
                let registry = RegistryId::new(registry_name).map_err(|reason| {
                    ResolveLowerError::Registry {
                        name: registry_name.to_owned(),
                        reason,
                    }
                })?;
                (package, SourceRequest::Registry { registry }, version)
            }
        };
        Ok(DependencyRequest {
            package,
            name,
            source,
            version,
            features: self.features().iter().cloned().collect(),
            default_features: self.default_features(),
            optional: self.is_optional(),
            kind,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jals_resolve::id::{RegistryId, SourceId};
    use jals_resolve::version::Version;

    fn manifest(text: &str) -> Manifest {
        text.parse().unwrap()
    }

    fn root_id(name: &str) -> PackageId {
        PackageId::new(
            PackageName::new(name).unwrap(),
            Version::parse("1.0.0").unwrap(),
            SourceId::Registry(RegistryId::default()),
        )
    }

    #[test]
    fn every_dependency_form_lowers_to_its_source() {
        let manifest = manifest(
            r#"
            [build]
            backend = { type = "jals-wasm" }

            [dependencies]
            legacy = { jar = "libs/legacy.jar" }
            host = { wasm = "libs/host.wasm", foreign = true }
            sdk = { git = "https://github.com/example/sdk", tag = "v1.2.3", dir = "core" }
            sibling = { path = "../sibling" }
            "#,
        );
        let summary = manifest.resolver_summary(root_id("app")).unwrap();
        assert_eq!(summary.dependencies.len(), 4);
        let by_name = |name: &str| summary.dependency(name).unwrap();
        assert_eq!(
            by_name("legacy").source,
            SourceRequest::Direct {
                kind: DirectKind::Jar,
                locator: "libs/legacy.jar".to_owned(),
            }
        );
        assert_eq!(
            by_name("host").source,
            SourceRequest::Direct {
                kind: DirectKind::Wasm,
                locator: "libs/host.wasm".to_owned(),
            }
        );
        assert_eq!(
            by_name("sdk").source,
            SourceRequest::Git {
                url: "https://github.com/example/sdk".to_owned(),
                reference: GitReference::Tag("v1.2.3".to_owned()),
                dir: Some("core".to_owned()),
            }
        );
        assert_eq!(
            by_name("sibling").source,
            SourceRequest::Path {
                location: "../sibling".to_owned(),
                dir: None,
            }
        );
        assert_eq!(by_name("sdk").version.as_str(), "*");
    }

    #[test]
    fn dev_edges_carry_the_development_kind() {
        let manifest = manifest(
            r#"
            [dependencies]
            runtime = { jar = "libs/runtime.jar" }

            [dev-dependencies]
            harness = { path = "../harness" }
            "#,
        );
        let summary = manifest.resolver_summary(root_id("app")).unwrap();
        assert_eq!(
            summary.dependency("runtime").unwrap().kind,
            DependencyKind::Normal
        );
        assert_eq!(
            summary.dependency("harness").unwrap().kind,
            DependencyKind::Development
        );
    }

    #[test]
    fn optional_and_feature_fields_are_carried() {
        let manifest = manifest(
            r#"
            [dependencies]
            optional-lib = { path = "../optional", optional = true, features = ["fast"], default-features = false }
            "#,
        );
        let summary = manifest.resolver_summary(root_id("app")).unwrap();
        let request = summary.dependency("optional-lib").unwrap();
        assert!(request.optional);
        assert!(!request.default_features);
        assert!(request.features.contains("fast"));
    }

    #[test]
    fn the_feature_graph_classifies_every_reference_shape() {
        let manifest = manifest(
            r#"
            [features]
            default = ["base"]
            base = []
            client = ["dep:minecraft", "minecraft/release", "extra"]
            extra = []

            [dependencies]
            minecraft = { path = "../minecraft", optional = true }
            "#,
        );
        let features = manifest.resolver_features();
        assert_eq!(
            features["default"],
            alloc::vec![FeatureValue::Feature("base".to_owned())]
        );
        let client = &features["client"];
        assert!(client.contains(&FeatureValue::Dependency(
            PackageName::new("minecraft").unwrap()
        )));
        assert!(client.contains(&FeatureValue::DependencyFeature {
            dependency: PackageName::new("minecraft").unwrap(),
            feature: "release".to_owned(),
        }));
        assert!(client.contains(&FeatureValue::Feature("extra".to_owned())));
    }

    #[test]
    fn a_dependency_key_that_is_not_a_package_name_is_rejected_at_lowering() {
        let manifest: Manifest = toml::from_str(
            r#"
            [dependencies]
            "not a name" = { jar = "libs/x.jar" }
            "#,
        )
        .unwrap();
        let error = manifest.resolver_summary(root_id("app")).unwrap_err();
        assert!(matches!(error, ResolveLowerError::Name { .. }));
    }

    #[test]
    fn contradictory_git_refs_are_rejected_at_lowering() {
        // `validate` rejects this too; lowering states its own answer for an unvalidated
        // manifest, which deserialization alone accepts.
        let manifest: Manifest = toml::from_str(
            r#"
            [dependencies]
            sdk = { git = "https://example.test/sdk", branch = "main", tag = "v1" }
            "#,
        )
        .unwrap();
        let error = manifest.resolver_summary(root_id("app")).unwrap_err();
        assert!(matches!(error, ResolveLowerError::Dependency(_)));
    }

    #[test]
    fn registry_entries_lower_to_registry_requests() {
        let manifest = manifest(
            r#"
            [registries.internal]
            url = "https://nexus.example/repository/maven-public"

            [dependencies]
            "org.slf4j:slf4j-api" = "2.0.16"
            guava = { group = "com.google.guava", version = "33.4.0-jre" }
            optional-lib = { group = "com.example", version = "[1.0,2.0)", registry = "internal", optional = true }
            "#,
        );
        let summary = manifest.resolver_summary(root_id("app")).unwrap();
        let by_name = |name: &str| summary.dependency(name).unwrap();
        assert_eq!(
            by_name("org.slf4j:slf4j-api").package,
            PackageName::new("org.slf4j:slf4j-api").unwrap()
        );
        assert_eq!(
            by_name("guava").package,
            PackageName::new("com.google.guava:guava").unwrap()
        );
        assert_eq!(by_name("guava").version.as_str(), "33.4.0-jre");
        assert_eq!(
            by_name("optional-lib").source,
            SourceRequest::Registry {
                registry: RegistryId::new("internal").unwrap(),
            }
        );
        assert!(by_name("optional-lib").optional);
        assert_eq!(by_name("optional-lib").version.as_str(), "[1.0,2.0)");
        assert_eq!(
            by_name("org.slf4j:slf4j-api").source,
            SourceRequest::Registry {
                registry: RegistryId::default(),
            }
        );
    }

    #[test]
    fn registry_validation_rejects_what_cannot_resolve() {
        // An undeclared registry.
        let error = toml::from_str::<Manifest>(
            r#"
            [dependencies]
            lib = { group = "com.example", version = "1", registry = "nowhere" }
            "#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert!(matches!(
            error,
            crate::manifest::ValidationError::UndeclaredRegistry { .. }
        ));
        // A version that is not a requirement.
        let error = toml::from_str::<Manifest>(
            r#"
            [dependencies]
            lib = { group = "com.example", version = "not a version" }
            "#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert!(matches!(
            error,
            crate::manifest::ValidationError::Dependency(
                crate::manifest::DependencyError::RegistryRequirement { .. }
            )
        ));
        // Neither a group nor a coordinate key.
        let error = toml::from_str::<Manifest>(
            r#"
            [dependencies]
            lib = { version = "1" }
            "#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert!(matches!(
            error,
            crate::manifest::ValidationError::Dependency(
                crate::manifest::DependencyError::RegistryCoordinate { .. }
            )
        ));
        // A registry URL that is not http(s).
        let error = toml::from_str::<Manifest>(
            r#"
            [registries.internal]
            url = "ftp://nexus.example"

            [dependencies]
            lib = { group = "com.example", version = "1", registry = "internal" }
            "#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert!(matches!(
            error,
            crate::manifest::ValidationError::InvalidRegistryUrl { .. }
        ));
    }
}
