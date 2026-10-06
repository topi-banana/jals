//! Maven registry resolution: metadata, POMs, and the `Provider` that turns them into a graph.
//!
//! This is the host half of registry support. It parses the two XML documents a Maven repository
//! serves — `maven-metadata.xml` (the version list) and a POM (a package's declared graph) — and
//! implements [`jals_resolve::resolve::Provider`] over the crate's [`Fetcher`] seam. The
//! resolver itself stays pure; this module is what gives it bytes.
//!
//! What is modelled, and why each piece exists:
//!
//! - **Coordinates** are `group:artifact`. The artifact id is the package name a user writes;
//!   the group is either given explicitly or embedded in the key (`org.slf4j:slf4j-api`).
//! - **Parents** are fetched and merged: Maven inheritance contributes properties,
//!   `dependencyManagement`, and the parent's own dependencies. The chain is depth-bounded.
//! - **Properties** are interpolated (`${project.version}`, `${...}`) to a bounded fixpoint,
//!   with child POM values overriding parent ones.
//! - **`dependencyManagement`** supplies versions to dependencies that declare none. BOM imports
//!   (`<type>pom</type><scope>import</scope>`) are followed so a project that imports a platform
//!   POM resolves the versions that platform aligns. The current POM wins over imports; imports
//!   fill only what is still missing.
//! - **Scopes**: `compile` and `runtime` dependencies are transitive; `test`, `provided`, and
//!   `system` are not. `optional` transitive dependencies are skipped, as Maven skips them.
//!
//! Deliberately not modelled yet, and stated rather than approximated: POM `exclusions` (a
//! path-dependent filter the resolver has no vocabulary for), classifiers (a package id has no
//! classifier dimension), and checksum sidecars (the lock pins content after the first verified
//! download). Each is a documented next step in `jals-resolve/DESIGN.md`.
//!
//! **Parallelism.** The batch methods are overridden: every `maven-metadata.xml` in one batch is
//! fetched concurrently with [`jals_exec::join_ordered`], and every selected POM in a summary
//! batch is fetched the same way, then parsed and expanded in input order. The fetcher is a
//! shared reference, so overlapping reads needs no locking; the provider's memo maps are touched
//! only between the concurrent sections, which is what keeps the output deterministic.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use jals_progress::Task;
use jals_resolve::id::{PackageId, PackageName, RegistryId, SourceId};
use jals_resolve::lock::LockedPackage;
use jals_resolve::resolve::{CandidateRequest, Provider};
use jals_resolve::summary::Candidate;
use jals_resolve::summary::{DependencyKind, DependencyRequest, SourceRequest, Summary};
use jals_resolve::version::{Version, VersionReq};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::Fetcher;
use crate::io::Fetch;
use crate::resolve::ExternalLocator;

/// The largest `maven-metadata.xml` this provider will read.
const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;
/// The largest POM this provider will read.
const MAX_POM_BYTES: usize = 8 * 1024 * 1024;
/// How deep a parent chain or import chain may nest before the POM is rejected.
const MAX_INHERITANCE_DEPTH: usize = 16;
/// How many interpolation rounds a property value may take.
const MAX_INTERPOLATION_DEPTH: usize = 16;

/// A Maven `group:artifact` coordinate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Coordinate {
    group: String,
    artifact: String,
}

impl Coordinate {
    /// Split a package name into a coordinate, or `None` for a bare project name.
    pub(crate) fn parse(name: &PackageName) -> Option<Self> {
        let (group, artifact) = name.maven_parts()?;
        Some(Self {
            group: group.to_owned(),
            artifact: artifact.to_owned(),
        })
    }

    /// The package name this coordinate is addressed by.
    pub(crate) fn package_name(&self) -> PackageName {
        PackageName::new(format!("{}:{}", self.group, self.artifact))
            .expect("a parsed coordinate is a valid package name")
    }

    /// The repository directory (`org/slf4j/slf4j-api`).
    fn directory(&self) -> String {
        format!("{}/{}", self.group.replace('.', "/"), self.artifact)
    }

    /// The version directory.
    fn version_directory(&self, version: &Version) -> String {
        format!("{}/{}", self.directory(), version.as_str())
    }

    /// The artifact file name for a version and extension.
    fn artifact_file(&self, version: &Version, extension: &str) -> String {
        format!("{}-{}.{}", self.artifact, version.as_str(), extension)
    }

    /// The URL of this coordinate's main artifact in a repository rooted at `base`.
    pub(crate) fn artifact_url(&self, base: &str, version: &Version, extension: &str) -> String {
        format!(
            "{}/{}/{}",
            base.trim_end_matches('/'),
            self.version_directory(version),
            self.artifact_file(version, extension)
        )
    }
}

/// One POM dependency before property interpolation and management.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PomDependency {
    group_id: Option<String>,
    artifact_id: Option<String>,
    version: Option<String>,
    scope: Option<String>,
    optional: bool,
    type_: Option<String>,
}

impl PomDependency {
    /// The managed key: `group:artifact`.
    fn key(&self) -> Option<String> {
        Some(format!(
            "{}:{}",
            self.group_id.as_ref()?,
            self.artifact_id.as_ref()?
        ))
    }
}

/// One `<parent>` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParentRef {
    coordinate: Coordinate,
    version: String,
}

/// A parsed POM, exactly as written (no inheritance applied).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RawPom {
    parent: Option<ParentRef>,
    group_id: Option<String>,
    artifact_id: Option<String>,
    version: Option<String>,
    properties: BTreeMap<String, String>,
    dependency_management: Vec<PomDependency>,
    dependencies: Vec<PomDependency>,
}

/// One effective POM: inheritance merged, properties ready to interpolate.
#[derive(Debug, Clone)]
struct EffectivePom {
    coordinate: Coordinate,
    version: String,
    properties: BTreeMap<String, String>,
    /// Managed dependency by `group:artifact`.
    managed: BTreeMap<String, PomDependency>,
    /// Merged dependencies, child overriding parent by key.
    dependencies: Vec<PomDependency>,
}

/// The quick-xml visitor over POM and metadata documents.
///
/// A hand-rolled visitor rather than a DOM: POMs are large and flat, and every branch below
/// either reads one leaf or skips a subtree, so a tree allocation would only be thrown away.
struct Xml;

impl Xml {
    /// Parse a POM document.
    fn pom(bytes: &[u8]) -> Result<RawPom, String> {
        let text =
            core::str::from_utf8(bytes).map_err(|error| format!("POM is not UTF-8: {error}"))?;
        let mut reader = Reader::from_str(text);
        reader.config_mut().trim_text(true);
        loop {
            match reader.read_event().map_err(|error| error.to_string())? {
                Event::Start(start) if start.local_name().as_ref() == "project" => {
                    return Self::project(&mut reader);
                }
                Event::Eof => return Err("POM has no <project> element".to_owned()),
                _ => {}
            }
        }
    }

    /// Parse every `<version>` under `<metadata>`; `<release>`/`<latest>` join as fallbacks.
    fn metadata_versions(bytes: &[u8]) -> Result<Vec<String>, String> {
        let text = core::str::from_utf8(bytes)
            .map_err(|error| format!("maven-metadata.xml is not UTF-8: {error}"))?;
        let mut reader = Reader::from_str(text);
        reader.config_mut().trim_text(true);
        let mut versions = Vec::new();
        loop {
            match reader.read_event().map_err(|error| error.to_string())? {
                Event::Start(start) => {
                    let name = start.local_name();
                    if name.as_ref() == "version"
                        || name.as_ref() == "release"
                        || name.as_ref() == "latest"
                    {
                        let value = Self::leaf(&mut reader, &start)?;
                        if !value.is_empty() && !versions.contains(&value) {
                            versions.push(value);
                        }
                    }
                    // A non-leaf element is not skipped: the reader is a linear stream, so its
                    // children arrive as the next events, and `<version>` lives under several
                    // wrappers (`versioning`, `versions`).
                }
                Event::Eof => return Ok(versions),
                _ => {}
            }
        }
    }

    fn project(reader: &mut Reader<&[u8]>) -> Result<RawPom, String> {
        let mut pom = RawPom::default();
        loop {
            match reader.read_event().map_err(|error| error.to_string())? {
                Event::Start(start) => match start.local_name().as_ref() {
                    "parent" => pom.parent = Some(Self::parent(reader)?),
                    "groupId" => pom.group_id = Some(Self::leaf(reader, &start)?),
                    "artifactId" => pom.artifact_id = Some(Self::leaf(reader, &start)?),
                    "version" => pom.version = Some(Self::leaf(reader, &start)?),
                    "properties" => pom.properties = Self::properties(reader)?,
                    "dependencyManagement" => {
                        pom.dependency_management = Self::dependency_management(reader)?;
                    }
                    "dependencies" => pom.dependencies = Self::dependencies(reader)?,
                    _ => {
                        reader
                            .read_to_end(start.to_end().name())
                            .map_err(|error| error.to_string())?;
                    }
                },
                Event::End(end) if end.local_name().as_ref() == "project" => return Ok(pom),
                Event::Eof => return Ok(pom),
                _ => {}
            }
        }
    }

    fn parent(reader: &mut Reader<&[u8]>) -> Result<ParentRef, String> {
        let mut group_id = None;
        let mut artifact_id = None;
        let mut version = None;
        loop {
            match reader.read_event().map_err(|error| error.to_string())? {
                Event::Start(start) => match start.local_name().as_ref() {
                    "groupId" => group_id = Some(Self::leaf(reader, &start)?),
                    "artifactId" => artifact_id = Some(Self::leaf(reader, &start)?),
                    "version" => version = Some(Self::leaf(reader, &start)?),
                    _ => {
                        reader
                            .read_to_end(start.to_end().name())
                            .map_err(|error| error.to_string())?;
                    }
                },
                Event::End(end) if end.local_name().as_ref() == "parent" => {
                    let coordinate = Coordinate {
                        group: group_id.ok_or("POM <parent> has no groupId")?,
                        artifact: artifact_id.ok_or("POM <parent> has no artifactId")?,
                    };
                    return Ok(ParentRef {
                        coordinate,
                        version: version.ok_or("POM <parent> has no version")?,
                    });
                }
                Event::Eof => return Err("POM <parent> is unterminated".to_owned()),
                _ => {}
            }
        }
    }

    fn properties(reader: &mut Reader<&[u8]>) -> Result<BTreeMap<String, String>, String> {
        let mut properties = BTreeMap::new();
        loop {
            match reader.read_event().map_err(|error| error.to_string())? {
                Event::Start(start) => {
                    let key = start.local_name().as_ref().to_owned();
                    let value = Self::leaf(reader, &start)?;
                    properties.insert(key, value);
                }
                Event::End(end) if end.local_name().as_ref() == "properties" => {
                    return Ok(properties);
                }
                Event::Eof => return Err("POM <properties> is unterminated".to_owned()),
                _ => {}
            }
        }
    }

    fn dependency_management(reader: &mut Reader<&[u8]>) -> Result<Vec<PomDependency>, String> {
        let mut entries = Vec::new();
        loop {
            match reader.read_event().map_err(|error| error.to_string())? {
                Event::Start(start) => match start.local_name().as_ref() {
                    "dependencies" => entries = Self::dependencies(reader)?,
                    _ => {
                        reader
                            .read_to_end(start.to_end().name())
                            .map_err(|error| error.to_string())?;
                    }
                },
                Event::End(end) if end.local_name().as_ref() == "dependencyManagement" => {
                    return Ok(entries);
                }
                Event::Eof => {
                    return Err("POM <dependencyManagement> is unterminated".to_owned());
                }
                _ => {}
            }
        }
    }

    fn dependencies(reader: &mut Reader<&[u8]>) -> Result<Vec<PomDependency>, String> {
        let mut entries = Vec::new();
        loop {
            match reader.read_event().map_err(|error| error.to_string())? {
                Event::Start(start) => match start.local_name().as_ref() {
                    "dependency" => entries.push(Self::dependency(reader)?),
                    _ => {
                        reader
                            .read_to_end(start.to_end().name())
                            .map_err(|error| error.to_string())?;
                    }
                },
                Event::End(end) if end.local_name().as_ref() == "dependencies" => {
                    return Ok(entries);
                }
                Event::Eof => return Err("POM <dependencies> is unterminated".to_owned()),
                _ => {}
            }
        }
    }

    fn dependency(reader: &mut Reader<&[u8]>) -> Result<PomDependency, String> {
        let mut dependency = PomDependency::default();
        loop {
            match reader.read_event().map_err(|error| error.to_string())? {
                Event::Start(start) => match start.local_name().as_ref() {
                    "groupId" => dependency.group_id = Some(Self::leaf(reader, &start)?),
                    "artifactId" => dependency.artifact_id = Some(Self::leaf(reader, &start)?),
                    "version" => dependency.version = Some(Self::leaf(reader, &start)?),
                    "scope" => dependency.scope = Some(Self::leaf(reader, &start)?),
                    "type" => dependency.type_ = Some(Self::leaf(reader, &start)?),
                    "optional" => {
                        dependency.optional =
                            Self::leaf(reader, &start)?.eq_ignore_ascii_case("true");
                    }
                    _ => {
                        reader
                            .read_to_end(start.to_end().name())
                            .map_err(|error| error.to_string())?;
                    }
                },
                Event::End(end) if end.local_name().as_ref() == "dependency" => {
                    return Ok(dependency);
                }
                Event::Eof => return Err("POM <dependency> is unterminated".to_owned()),
                _ => {}
            }
        }
    }

    /// The text of a leaf element, entity-decoded and trimmed.
    fn leaf(reader: &mut Reader<&[u8]>, start: &BytesStart<'_>) -> Result<String, String> {
        let raw = reader
            .read_text(start.to_end().name())
            .map_err(|error| error.to_string())?
            .into_inner()
            .into_owned();
        let decoded = quick_xml::escape::unescape(&raw)
            .map(alloc::borrow::Cow::into_owned)
            .unwrap_or(raw);
        Ok(decoded.trim().to_owned())
    }
}

/// A resolver provider over one or more Maven registries.
pub(crate) struct MavenProvider<'f, F: Fetcher> {
    fetcher: &'f F,
    registries: BTreeMap<String, String>,
    metadata: BTreeMap<(Coordinate, RegistryId), Vec<Version>>,
    poms: BTreeMap<(Coordinate, String, RegistryId), RawPom>,
    effective: BTreeMap<(Coordinate, String, RegistryId), EffectivePom>,
}

impl<'f, F: Fetcher> MavenProvider<'f, F> {
    /// A provider over `registries` (name → base URL); `maven-central` is added when absent.
    pub(crate) fn new(fetcher: &'f F, registries: BTreeMap<String, String>) -> Self {
        let mut registries = registries;
        registries
            .entry(RegistryId::MAVEN_CENTRAL.to_owned())
            .or_insert_with(|| "https://repo1.maven.org/maven2".to_owned());
        Self {
            fetcher,
            registries,
            metadata: BTreeMap::new(),
            poms: BTreeMap::new(),
            effective: BTreeMap::new(),
        }
    }

    fn base_url<'s>(&'s self, registry: &RegistryId) -> Result<&'s str, String> {
        self.registries
            .get(registry.as_str())
            .map(String::as_str)
            .ok_or_else(|| format!("registry `{registry}` is not declared"))
    }

    fn join(base: &str, path: &str) -> String {
        format!("{}/{}", base.trim_end_matches('/'), path)
    }

    fn locator(url: &str) -> ExternalLocator {
        ExternalLocator::new(url)
    }

    /// Read one document under a byte ceiling through the crate's gated fetch.
    async fn read(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        Fetch::bounded(
            self.fetcher,
            &Self::locator(url),
            max_bytes,
            &Task::silent(),
        )
        .await
    }

    /// The POM URL for one coordinate/version in one registry.
    fn pom_url(coordinate: &Coordinate, version: &Version, base: &str) -> String {
        Self::join(
            base,
            &format!(
                "{}/{}",
                coordinate.version_directory(version),
                coordinate.artifact_file(version, "pom")
            ),
        )
    }

    /// The raw POM for one coordinate/version/registry, memoized.
    async fn raw_pom(
        &mut self,
        coordinate: &Coordinate,
        version: &Version,
        registry: &RegistryId,
    ) -> Result<RawPom, String> {
        let key = (
            coordinate.clone(),
            version.as_str().to_owned(),
            registry.clone(),
        );
        if let Some(pom) = self.poms.get(&key) {
            return Ok(pom.clone());
        }
        let base = self.base_url(registry)?.to_owned();
        let url = Self::pom_url(coordinate, version, &base);
        let bytes = self.read(&url, MAX_POM_BYTES).await?;
        let pom = Xml::pom(&bytes)?;
        self.poms.insert(key, pom.clone());
        Ok(pom)
    }

    /// Merge a POM with its parent chain into one effective POM. Boxed at the recursive call
    /// sites only, never on the straight-line path.
    fn effective<'s>(
        &'s mut self,
        coordinate: Coordinate,
        version: Version,
        registry: RegistryId,
        depth: usize,
    ) -> Pin<Box<dyn Future<Output = Result<EffectivePom, String>> + 's>> {
        Box::pin(async move {
            if depth > MAX_INHERITANCE_DEPTH {
                return Err(format!(
                    "POM parent chain for `{coordinate:?}` exceeds {MAX_INHERITANCE_DEPTH} levels"
                ));
            }
            let cache_key = (
                coordinate.clone(),
                version.as_str().to_owned(),
                registry.clone(),
            );
            if let Some(cached) = self.effective.get(&cache_key) {
                return Ok(cached.clone());
            }
            let raw = self.raw_pom(&coordinate, &version, &registry).await?;
            let mut effective = EffectivePom {
                coordinate: coordinate.clone(),
                version: version.as_str().to_owned(),
                properties: BTreeMap::new(),
                managed: BTreeMap::new(),
                dependencies: Vec::new(),
            };
            if let Some(parent) = raw.parent.clone() {
                let parent_version = parent
                    .version
                    .parse::<Version>()
                    .map_err(|error| error.to_string())?;
                let parent_effective = Box::pin(self.effective(
                    parent.coordinate.clone(),
                    parent_version,
                    registry.clone(),
                    depth + 1,
                ))
                .await?;
                effective.properties = parent_effective.properties;
                effective.managed = parent_effective.managed;
                effective.dependencies = parent_effective.dependencies;
            }
            for (key, value) in &raw.properties {
                effective.properties.insert(key.clone(), value.clone());
            }
            // Maven's built-ins win over same-named user properties.
            effective
                .properties
                .insert("project.groupId".to_owned(), coordinate.group.clone());
            effective
                .properties
                .insert("project.artifactId".to_owned(), coordinate.artifact.clone());
            effective
                .properties
                .insert("project.version".to_owned(), version.as_str().to_owned());
            // Dependency management: own entries win over imported BOMs, which fill only what
            // the parent chain left unmanaged.
            for entry in &raw.dependency_management {
                let Some(key) = entry.key() else { continue };
                let is_import = entry.scope.as_deref() == Some("import")
                    && entry.type_.as_deref() == Some("pom");
                if is_import {
                    let Some(import_version) = entry.version.as_deref() else {
                        continue;
                    };
                    let import_version = Self::interpolate(import_version, &effective.properties);
                    let Ok(import_version) = import_version.parse::<Version>() else {
                        continue;
                    };
                    let Some(import_coordinate) = Self::coordinate_from_key(&key) else {
                        continue;
                    };
                    if let Ok(imported) = Box::pin(self.effective(
                        import_coordinate,
                        import_version,
                        registry.clone(),
                        depth + 1,
                    ))
                    .await
                    {
                        for (managed_key, managed) in imported.managed {
                            effective.managed.entry(managed_key).or_insert(managed);
                        }
                    }
                    continue;
                }
                effective.managed.insert(key, entry.clone());
            }
            // Dependencies: parent's first, the POM's own overriding by key.
            let mut merged: BTreeMap<String, PomDependency> = effective
                .dependencies
                .iter()
                .filter_map(|dependency| Some((dependency.key()?, dependency.clone())))
                .collect();
            for dependency in &raw.dependencies {
                if let Some(key) = dependency.key() {
                    merged.insert(key, dependency.clone());
                }
            }
            effective.dependencies = merged.into_values().collect();
            self.effective.insert(cache_key, effective.clone());
            Ok(effective)
        })
    }

    fn coordinate_from_key(key: &str) -> Option<Coordinate> {
        let (group, artifact) = key.split_once(':')?;
        Some(Coordinate {
            group: group.to_owned(),
            artifact: artifact.to_owned(),
        })
    }

    /// Interpolate `${...}` references to a bounded fixpoint; an unresolved reference is left
    /// verbatim, which later makes the dependency skippable rather than wrong.
    fn interpolate(value: &str, properties: &BTreeMap<String, String>) -> String {
        let mut current = value.to_owned();
        for _ in 0..MAX_INTERPOLATION_DEPTH {
            if !current.contains("${") {
                break;
            }
            let mut next = String::with_capacity(current.len());
            let mut rest = current.as_str();
            while let Some(start) = rest.find("${") {
                next.push_str(&rest[..start]);
                let after = &rest[start + 2..];
                if let Some(end) = after.find('}') {
                    let name = &after[..end];
                    if let Some(replacement) = properties.get(name) {
                        next.push_str(replacement);
                    } else {
                        next.push_str("${");
                        next.push_str(name);
                        next.push('}');
                    }
                    rest = &after[end + 1..];
                } else {
                    next.push_str("${");
                    rest = after;
                }
            }
            next.push_str(rest);
            if next == current {
                break;
            }
            current = next;
        }
        current
    }

    /// Turn one effective POM into a resolver summary.
    fn summary_of(registry: &RegistryId, effective: &EffectivePom) -> Result<Summary, String> {
        let name = effective.coordinate.package_name();
        let version = effective
            .version
            .parse::<Version>()
            .map_err(|error| error.to_string())?;
        let mut dependencies = Vec::new();
        for dependency in &effective.dependencies {
            let Some(group) = dependency
                .group_id
                .as_deref()
                .map(|value| Self::interpolate(value, &effective.properties))
            else {
                continue;
            };
            let Some(artifact) = dependency
                .artifact_id
                .as_deref()
                .map(|value| Self::interpolate(value, &effective.properties))
            else {
                continue;
            };
            let key = format!("{group}:{artifact}");
            let Some(version_text) = dependency
                .version
                .as_deref()
                .map(|value| Self::interpolate(value, &effective.properties))
                .or_else(|| {
                    effective
                        .managed
                        .get(&key)
                        .and_then(|managed| managed.version.clone())
                        .map(|value| Self::interpolate(&value, &effective.properties))
                })
            else {
                continue;
            };
            let scope = dependency.scope.as_deref().unwrap_or("compile");
            if !matches!(scope, "compile" | "runtime") {
                continue;
            }
            if dependency.optional {
                continue;
            }
            if dependency.type_.as_deref() == Some("pom") {
                continue;
            }
            if version_text.is_empty() || version_text.contains("${") {
                continue;
            }
            // A POM version is exact in Maven unless it is a range.
            let requirement = if version_text.starts_with(['[', '(']) {
                version_text.clone()
            } else {
                format!("={version_text}")
            };
            let Ok(requirement) = VersionReq::parse(&requirement) else {
                continue;
            };
            let Ok(package) = PackageName::new(key) else {
                continue;
            };
            if package == name {
                continue;
            }
            dependencies.push(DependencyRequest {
                name: package.clone(),
                package,
                source: SourceRequest::Registry {
                    registry: registry.clone(),
                },
                version: requirement,
                features: BTreeSet::default(),
                default_features: true,
                optional: false,
                kind: DependencyKind::Normal,
            });
        }
        Ok(Summary {
            id: PackageId::new(name, version, SourceId::Registry(registry.clone())),
            dependencies,
            features: BTreeMap::new(),
        })
    }

    /// Build candidate ids from a version list, appending a lock pin that is no longer listed.
    fn candidates_from(
        coordinate: &Coordinate,
        registry: &RegistryId,
        versions: &[Version],
        locked: Option<&LockedPackage>,
        request: &DependencyRequest,
    ) -> Vec<Candidate> {
        let name = coordinate.package_name();
        let mut candidates: Vec<Candidate> = versions
            .iter()
            .map(|version| {
                Candidate::new(PackageId::new(
                    name.clone(),
                    version.clone(),
                    SourceId::Registry(registry.clone()),
                ))
            })
            .collect();
        if let Some(locked) = locked
            && locked.id.source.satisfies(&request.source)
            && !candidates.iter().any(|candidate| candidate.id == locked.id)
        {
            candidates.push(Candidate::new(locked.id.clone()));
        }
        candidates
    }

    /// Parse fetched metadata bytes into a descending version list.
    fn parse_versions(bytes: &[u8]) -> Result<Vec<Version>, String> {
        let mut parsed: Vec<Version> = Xml::metadata_versions(bytes)?
            .into_iter()
            .filter_map(|text| text.parse::<Version>().ok())
            .collect();
        parsed.sort_by(|left, right| right.cmp(left));
        parsed.dedup();
        Ok(parsed)
    }
}

impl<F: Fetcher> Provider for MavenProvider<'_, F> {
    type Error = String;

    async fn candidates(
        &mut self,
        request: &DependencyRequest,
        locked: Option<&LockedPackage>,
    ) -> Result<Vec<Candidate>, Self::Error> {
        let coordinate = Coordinate::parse(&request.package)
            .ok_or_else(|| format!("`{}` is not a Maven coordinate", request.package))?;
        let registry = match &request.source {
            SourceRequest::Registry { registry } => registry.clone(),
            other => {
                return Err(format!(
                    "the Maven provider cannot answer a request from `{other}`"
                ));
            }
        };
        let key = (coordinate.clone(), registry.clone());
        let versions = if let Some(cached) = self.metadata.get(&key) {
            cached.clone()
        } else {
            let base = self.base_url(&registry)?.to_owned();
            let url = Self::join(
                &base,
                &format!("{}/maven-metadata.xml", coordinate.directory()),
            );
            let fetched = match self.read(&url, MAX_METADATA_BYTES).await {
                Ok(bytes) => Self::parse_versions(&bytes)?,
                // A repository may serve an artifact without a version index. A pinned
                // requirement still resolves to its base; a lock pin still resolves.
                Err(error) => match (request.version.pinned_base().cloned(), locked) {
                    (Some(version), _) => vec![version],
                    (None, Some(locked)) => vec![locked.id.version.clone()],
                    (None, None) => return Err(error),
                },
            };
            self.metadata.insert(key, fetched.clone());
            fetched
        };
        Ok(Self::candidates_from(
            &coordinate,
            &registry,
            &versions,
            locked,
            request,
        ))
    }

    async fn summary(&mut self, id: &PackageId) -> Result<Summary, Self::Error> {
        let coordinate = Coordinate::parse(&id.name)
            .ok_or_else(|| format!("`{}` is not a Maven coordinate", id.name))?;
        let registry = match &id.source {
            SourceId::Registry(registry) => registry.clone(),
            other => return Err(format!("the Maven provider cannot answer `{other}`")),
        };
        let effective =
            Box::pin(self.effective(coordinate, id.version.clone(), registry, 0)).await?;
        let SourceId::Registry(registry) = &id.source else {
            unreachable!("checked above");
        };
        Self::summary_of(registry, &effective)
    }

    async fn candidates_batch(
        &mut self,
        requests: Vec<CandidateRequest>,
    ) -> Vec<Result<Vec<Candidate>, Self::Error>> {
        // Plan every URL first; each fetch future then borrows only the shared fetcher and its
        // own owned locator, which is what lets `join_ordered` overlap them.
        let plans: Vec<Result<(Coordinate, RegistryId, ExternalLocator), String>> = requests
            .iter()
            .map(|entry| {
                let coordinate = Coordinate::parse(&entry.request.package).ok_or_else(|| {
                    format!("`{}` is not a Maven coordinate", entry.request.package)
                })?;
                let registry = match &entry.request.source {
                    SourceRequest::Registry { registry } => registry.clone(),
                    other => {
                        return Err(format!(
                            "the Maven provider cannot answer a request from `{other}`"
                        ));
                    }
                };
                let base = self.base_url(&registry)?.to_owned();
                let url = Self::join(
                    &base,
                    &format!("{}/maven-metadata.xml", coordinate.directory()),
                );
                Ok((coordinate, registry, Self::locator(&url)))
            })
            .collect();
        let fetchable: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter_map(|(index, plan)| plan.as_ref().ok().map(|_| index))
            .collect();
        let tasks: Vec<Task> = fetchable.iter().map(|_| Task::silent()).collect();
        let fetcher = self.fetcher;
        let futures = fetchable
            .iter()
            .zip(&tasks)
            .map(|(index, task)| match &plans[*index] {
                Ok((_, _, locator)) => {
                    let locator = locator.clone();
                    async move { Fetch::bounded(fetcher, &locator, MAX_METADATA_BYTES, task).await }
                }
                Err(_) => unreachable!("only planned entries are fetched"),
            });
        let fetched = jals_exec::join_ordered(futures).await;
        let mut bodies: BTreeMap<usize, Result<Vec<u8>, String>> = BTreeMap::new();
        for (index, result) in fetchable.into_iter().zip(fetched) {
            bodies.insert(index, result);
        }
        let mut results = Vec::with_capacity(requests.len());
        for (index, plan) in plans.into_iter().enumerate() {
            let result = match plan {
                Err(error) => Err(error),
                Ok((coordinate, registry, _)) => {
                    let versions = match bodies.remove(&index) {
                        Some(Ok(bytes)) => match Self::parse_versions(&bytes) {
                            Ok(versions) => versions,
                            Err(error) => {
                                results.push(Err(error));
                                continue;
                            }
                        },
                        Some(Err(error)) => {
                            match (
                                requests[index].request.version.pinned_base().cloned(),
                                requests[index].locked.as_ref(),
                            ) {
                                (Some(version), _) => vec![version],
                                (None, Some(locked)) => vec![locked.id.version.clone()],
                                (None, None) => {
                                    results.push(Err(error));
                                    continue;
                                }
                            }
                        }
                        // A plan that named no URL cannot happen; an empty list is honest.
                        None => Vec::new(),
                    };
                    self.metadata
                        .insert((coordinate.clone(), registry.clone()), versions.clone());
                    Ok(Self::candidates_from(
                        &coordinate,
                        &registry,
                        &versions,
                        requests[index].locked.as_ref(),
                        &requests[index].request,
                    ))
                }
            };
            results.push(result);
        }
        results
    }

    async fn summaries_batch(&mut self, ids: &[PackageId]) -> Vec<Result<Summary, Self::Error>> {
        // Fetch every direct POM concurrently, memoize the parsed documents, then build
        // effective POMs in input order. Parents and BOM imports hit the memo, so shared
        // ancestry is fetched once even across a wide batch.
        let plans: Vec<Result<(Coordinate, RegistryId, ExternalLocator), String>> = ids
            .iter()
            .map(|id| {
                let coordinate = Coordinate::parse(&id.name)
                    .ok_or_else(|| format!("`{}` is not a Maven coordinate", id.name))?;
                let registry = match &id.source {
                    SourceId::Registry(registry) => registry.clone(),
                    other => return Err(format!("the Maven provider cannot answer `{other}`")),
                };
                let base = self.base_url(&registry)?.to_owned();
                let url = Self::pom_url(&coordinate, &id.version, &base);
                Ok((coordinate, registry, Self::locator(&url)))
            })
            .collect();
        let fetchable: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter_map(|(index, plan)| plan.as_ref().ok().map(|_| index))
            .collect();
        let tasks: Vec<Task> = fetchable.iter().map(|_| Task::silent()).collect();
        let fetcher = self.fetcher;
        let futures = fetchable
            .iter()
            .zip(&tasks)
            .map(|(index, task)| match &plans[*index] {
                Ok((_, _, locator)) => {
                    let locator = locator.clone();
                    async move { Fetch::bounded(fetcher, &locator, MAX_POM_BYTES, task).await }
                }
                Err(_) => unreachable!("only planned entries are fetched"),
            });
        let fetched = jals_exec::join_ordered(futures).await;
        let mut bodies: BTreeMap<usize, Result<Vec<u8>, String>> = BTreeMap::new();
        for (index, result) in fetchable.into_iter().zip(fetched) {
            bodies.insert(index, result);
        }
        let mut results = Vec::with_capacity(ids.len());
        for (index, plan) in plans.into_iter().enumerate() {
            let (coordinate, registry) = match plan {
                Ok((coordinate, registry, _)) => (coordinate, registry),
                Err(error) => {
                    results.push(Err(error));
                    continue;
                }
            };
            if let Some(Ok(bytes)) = bodies.remove(&index) {
                match Xml::pom(&bytes) {
                    Ok(raw) => {
                        self.poms.insert(
                            (
                                coordinate.clone(),
                                ids[index].version.as_str().to_owned(),
                                registry.clone(),
                            ),
                            raw,
                        );
                    }
                    Err(error) => {
                        results.push(Err(error));
                        continue;
                    }
                }
            } else if let Some(Err(error)) = bodies.remove(&index) {
                results.push(Err(error));
                continue;
            }
            let effective =
                Box::pin(self.effective(coordinate, ids[index].version.clone(), registry, 0)).await;
            results.push(effective.and_then(|effective| {
                let SourceId::Registry(registry) = &ids[index].source else {
                    unreachable!("a registry id was checked while planning");
                };
                Self::summary_of(registry, &effective)
            }));
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use jals_resolve::summary::RootRequest;

    const BASE: &str = "https://repo.test/maven2";

    /// A repository served from memory: path (below `BASE`) → bytes.
    struct MapFetcher {
        files: BTreeMap<String, Vec<u8>>,
    }

    impl MapFetcher {
        fn new() -> Self {
            Self {
                files: BTreeMap::new(),
            }
        }

        fn add(&mut self, path: &str, body: &str) {
            self.files
                .insert(format!("{BASE}/{path}"), body.as_bytes().to_vec());
        }
    }

    // Matches the trait's async signatures; the bodies never await.
    #[allow(clippy::unused_async_trait_impl)]
    impl Fetcher for MapFetcher {
        fn network(&self) -> crate::NetworkPolicy {
            crate::NetworkPolicy::Online
        }

        fn retry(&self) -> crate::RetrySchedule {
            crate::RetrySchedule::none()
        }

        async fn delay(&self, _millis: u32) {}

        async fn fetch_admitted(
            &self,
            locator: &str,
            _report: &Task,
        ) -> Result<Vec<u8>, crate::FetchError> {
            self.files
                .get(locator)
                .cloned()
                .ok_or_else(|| crate::FetchError::permanent(format!("no fixture for `{locator}`")))
        }
    }

    fn registry() -> RegistryId {
        RegistryId::default()
    }

    fn registries() -> BTreeMap<String, String> {
        BTreeMap::from([(RegistryId::MAVEN_CENTRAL.to_owned(), BASE.to_owned())])
    }

    fn request(coordinate: &str, requirement: &str) -> DependencyRequest {
        let package = PackageName::new(coordinate).unwrap();
        DependencyRequest {
            name: package.clone(),
            package,
            source: SourceRequest::Registry {
                registry: registry(),
            },
            version: VersionReq::parse(requirement).unwrap(),
            features: BTreeSet::default(),
            default_features: true,
            optional: false,
            kind: DependencyKind::Normal,
        }
    }

    fn provider(fetcher: &MapFetcher) -> MavenProvider<'_, MapFetcher> {
        MavenProvider::new(fetcher, registries())
    }

    fn add_lib_repository(fetcher: &mut MapFetcher) {
        fetcher.add(
            "com/example/lib/maven-metadata.xml",
            r"<metadata><versioning><versions>
                 <version>1.0.0</version><version>1.1.0</version>
               </versions><release>1.1.0</release></versioning></metadata>",
        );
        for version in ["1.0.0", "1.1.0"] {
            fetcher.add(
                &format!("com/example/lib/{version}/lib-{version}.pom"),
                &format!(
                    r"<project>
                         <modelVersion>4.0.0</modelVersion>
                         <groupId>com.example</groupId>
                         <artifactId>lib</artifactId>
                         <version>{version}</version>
                         <properties><dep.version>2.0.0</dep.version></properties>
                         <dependencyManagement><dependencies>
                           <dependency><groupId>com.example</groupId><artifactId>managed</artifactId><version>${{dep.version}}</version></dependency>
                         </dependencies></dependencyManagement>
                         <dependencies>
                           <dependency><groupId>com.example</groupId><artifactId>direct</artifactId><version>1.0.0</version></dependency>
                           <dependency><groupId>com.example</groupId><artifactId>managed</artifactId></dependency>
                           <dependency><groupId>com.example</groupId><artifactId>test-only</artifactId><version>1.0.0</version><scope>test</scope></dependency>
                           <dependency><groupId>com.example</groupId><artifactId>provided-only</artifactId><version>1.0.0</version><scope>provided</scope></dependency>
                           <dependency><groupId>com.example</groupId><artifactId>optional-lib</artifactId><version>1.0.0</version><optional>true</optional></dependency>
                           <dependency><groupId>com.example</groupId><artifactId>bom-like</artifactId><version>1.0.0</version><type>pom</type></dependency>
                         </dependencies>
                       </project>"
                ),
            );
        }
    }

    #[test]
    fn parses_a_pom_into_its_raw_shape() {
        let pom = Xml::pom(
            br#"<project xmlns="http://maven.apache.org/POM/4.0.0">
                  <modelVersion>4.0.0</modelVersion>
                  <parent><groupId>com.example</groupId><artifactId>parent</artifactId><version>1</version></parent>
                  <groupId>com.example</groupId><artifactId>lib</artifactId><version>1.0.0</version>
                  <properties><a>1</a><b>x &amp; y</b></properties>
                  <dependencyManagement><dependencies>
                    <dependency><groupId>g</groupId><artifactId>a</artifactId><version>2</version></dependency>
                  </dependencies></dependencyManagement>
                  <dependencies>
                    <dependency><groupId>g</groupId><artifactId>b</artifactId><version>3</version><scope>runtime</scope><optional>true</optional></dependency>
                  </dependencies>
                </project>"#,
        )
        .unwrap();
        assert_eq!(pom.group_id.as_deref(), Some("com.example"));
        assert_eq!(pom.artifact_id.as_deref(), Some("lib"));
        assert_eq!(pom.version.as_deref(), Some("1.0.0"));
        assert_eq!(pom.properties.get("b").map(String::as_str), Some("x & y"));
        assert_eq!(pom.parent.unwrap().coordinate.group, "com.example");
        assert_eq!(pom.dependency_management.len(), 1);
        assert_eq!(pom.dependencies.len(), 1);
        assert!(pom.dependencies[0].optional);
        assert_eq!(pom.dependencies[0].scope.as_deref(), Some("runtime"));
    }

    #[test]
    fn a_summary_keeps_compile_and_runtime_and_drops_everything_else() {
        let mut fetcher = MapFetcher::new();
        add_lib_repository(&mut fetcher);
        let mut provider = provider(&fetcher);
        let id = PackageId::new(
            PackageName::new("com.example:lib").unwrap(),
            Version::parse("1.1.0").unwrap(),
            SourceId::Registry(registry()),
        );
        let summary = jals_exec::block_on_inline(provider.summary(&id)).unwrap();
        let names: Vec<String> = summary
            .dependencies
            .iter()
            .map(|dependency| dependency.package.to_string())
            .collect();
        assert_eq!(names, vec!["com.example:direct", "com.example:managed"]);
        assert_eq!(
            summary.dependencies[1].version.as_str(),
            "=2.0.0",
            "the managed version is interpolated from the property"
        );
    }

    #[test]
    fn a_parent_contributes_properties_management_and_dependencies() {
        let mut fetcher = MapFetcher::new();
        fetcher.add(
            "com/example/parent/1.0.0/parent-1.0.0.pom",
            r"<project>
                 <groupId>com.example</groupId><artifactId>parent</artifactId><version>1.0.0</version>
                 <properties><managed.version>3.0.0</managed.version></properties>
                 <dependencyManagement><dependencies>
                   <dependency><groupId>com.example</groupId><artifactId>par-managed</artifactId><version>${managed.version}</version></dependency>
                 </dependencies></dependencyManagement>
                 <dependencies>
                   <dependency><groupId>com.example</groupId><artifactId>inherited</artifactId><version>9.9.9</version></dependency>
                 </dependencies>
               </project>",
        );
        fetcher.add(
            "com/example/child/2.0.0/child-2.0.0.pom",
            r"<project>
                 <parent><groupId>com.example</groupId><artifactId>parent</artifactId><version>1.0.0</version></parent>
                 <artifactId>child</artifactId><version>2.0.0</version>
                 <dependencies>
                   <dependency><groupId>com.example</groupId><artifactId>par-managed</artifactId></dependency>
                 </dependencies>
               </project>",
        );
        let mut provider = provider(&fetcher);
        let id = PackageId::new(
            PackageName::new("com.example:child").unwrap(),
            Version::parse("2.0.0").unwrap(),
            SourceId::Registry(registry()),
        );
        let summary = jals_exec::block_on_inline(provider.summary(&id)).unwrap();
        let names: Vec<String> = summary
            .dependencies
            .iter()
            .map(|dependency| dependency.package.to_string())
            .collect();
        assert_eq!(
            names,
            vec!["com.example:inherited", "com.example:par-managed"]
        );
        let managed = &summary.dependencies[1];
        assert_eq!(managed.version.as_str(), "=3.0.0");
    }

    #[test]
    fn a_bom_import_supplies_missing_versions() {
        let mut fetcher = MapFetcher::new();
        fetcher.add(
            "com/example/platform/1.0.0/platform-1.0.0.pom",
            r"<project>
                 <groupId>com.example</groupId><artifactId>platform</artifactId><version>1.0.0</version>
                 <packaging>pom</packaging>
                 <dependencyManagement><dependencies>
                   <dependency><groupId>com.example</groupId><artifactId>platform-lib</artifactId><version>5.0.0</version></dependency>
                 </dependencies></dependencyManagement>
               </project>",
        );
        fetcher.add(
            "com/example/app/1.0.0/app-1.0.0.pom",
            r"<project>
                 <groupId>com.example</groupId><artifactId>app</artifactId><version>1.0.0</version>
                 <dependencyManagement><dependencies>
                   <dependency><groupId>com.example</groupId><artifactId>platform</artifactId><version>1.0.0</version><type>pom</type><scope>import</scope></dependency>
                 </dependencies></dependencyManagement>
                 <dependencies>
                   <dependency><groupId>com.example</groupId><artifactId>platform-lib</artifactId></dependency>
                 </dependencies>
               </project>",
        );
        let mut provider = provider(&fetcher);
        let id = PackageId::new(
            PackageName::new("com.example:app").unwrap(),
            Version::parse("1.0.0").unwrap(),
            SourceId::Registry(registry()),
        );
        let summary = jals_exec::block_on_inline(provider.summary(&id)).unwrap();
        assert_eq!(summary.dependencies.len(), 1);
        assert_eq!(summary.dependencies[0].version.as_str(), "=5.0.0");
    }

    #[test]
    fn candidates_come_from_metadata_and_keep_a_lock_pin() {
        let mut fetcher = MapFetcher::new();
        add_lib_repository(&mut fetcher);
        let mut provider = provider(&fetcher);
        let candidates =
            jals_exec::block_on_inline(provider.candidates(&request("com.example:lib", "1"), None))
                .unwrap();
        let versions: Vec<String> = candidates
            .iter()
            .map(|candidate| candidate.id.version.to_string())
            .collect();
        assert_eq!(versions, vec!["1.1.0", "1.0.0"]);

        let locked = LockedPackage {
            id: PackageId::new(
                PackageName::new("com.example:lib").unwrap(),
                Version::parse("0.9.0").unwrap(),
                SourceId::Registry(registry()),
            ),
            checksum: None,
            dependencies: Vec::new(),
        };
        let candidates = jals_exec::block_on_inline(
            provider.candidates(&request("com.example:lib", "1"), Some(&locked)),
        )
        .unwrap();
        assert!(
            candidates.iter().any(|candidate| candidate.id == locked.id),
            "a pin the index no longer lists is still a candidate"
        );
    }

    #[test]
    fn a_pinned_requirement_resolves_without_a_version_index() {
        let fetcher = MapFetcher::new();
        let mut provider = provider(&fetcher);
        let candidates = jals_exec::block_on_inline(
            provider.candidates(&request("com.example:lib", "=2.0.16"), None),
        )
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id.version.as_str(), "2.0.16");
    }

    #[test]
    fn the_resolver_walks_a_maven_graph_end_to_end() {
        let mut fetcher = MapFetcher::new();
        add_lib_repository(&mut fetcher);
        fetcher.add(
            "com/example/direct/1.0.0/direct-1.0.0.pom",
            r"<project><groupId>com.example</groupId><artifactId>direct</artifactId><version>1.0.0</version></project>",
        );
        fetcher.add(
            "com/example/managed/2.0.0/managed-2.0.0.pom",
            r"<project><groupId>com.example</groupId><artifactId>managed</artifactId><version>2.0.0</version></project>",
        );
        let mut provider = provider(&fetcher);
        let root = RootRequest::new(Summary {
            id: PackageId::new(
                PackageName::new("app").unwrap(),
                Version::parse("0.0.0").unwrap(),
                SourceId::Workspace(jals_resolve::id::WorkspaceSource {
                    member: "app".to_owned(),
                }),
            ),
            dependencies: vec![request("com.example:lib", "1")],
            features: BTreeMap::new(),
        });
        let graph = jals_exec::block_on_inline(
            jals_resolve::resolve::Resolver::new(&mut provider).resolve(&[root], None),
        )
        .unwrap();
        let names: Vec<String> = graph
            .packages
            .iter()
            .map(|package| format!("{} {}", package.id.name, package.id.version))
            .collect();
        assert_eq!(
            names,
            vec![
                "com.example:direct 1.0.0",
                "com.example:lib 1.1.0",
                "com.example:managed 2.0.0",
            ]
        );
    }
}
