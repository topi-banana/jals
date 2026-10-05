//! What a manifest contributes to resolution.
//!
//! A [`Summary`] is the resolver-facing projection of one package's manifest: its dependency
//! requests and its feature graph. It deliberately carries nothing host-specific — no paths, no
//! bytes, no cache keys — because both the browser playground and the CLI feed the same resolver
//! from a `jals.toml` they each read their own way.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::id::{Checksum, DirectKind, PackageId, PackageName, RegistryId};
use crate::version::VersionReq;

/// One value of a `[features]` list, classified.
///
/// The three shapes are exactly the reference forms a manifest may write: a local feature name,
/// `dep:<dependency>` activation, and the cross-package `<dependency>/<feature>` forwarding form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureValue {
    /// Another feature of the same package.
    Feature(String),
    /// `dep:<dependency>`: activate an optional dependency.
    Dependency(PackageName),
    /// `<dependency>/<feature>`: enable `feature` in `dependency` when this package's feature is
    /// on. Forwarding does not activate an optional dependency by itself; an explicit
    /// `dep:<dependency>` (or the dependency's implicit feature) does, matching `jals-config`'s
    /// existing semantics.
    DependencyFeature {
        /// The dependency the feature is forwarded to.
        dependency: PackageName,
        /// The feature to enable there.
        feature: String,
    },
}

impl FeatureValue {
    /// The dependency this value names, if any.
    pub const fn dependency(&self) -> Option<&PackageName> {
        match self {
            Self::Feature(_) => None,
            Self::Dependency(dependency) | Self::DependencyFeature { dependency, .. } => {
                Some(dependency)
            }
        }
    }
}

/// Whether a dependency request comes from `[dependencies]` or `[dev-dependencies]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DependencyKind {
    /// A dependency every build resolves.
    Normal,
    /// A dependency only a test run (or an analysis host) resolves. Never transitive: a package's
    /// own dev-dependencies are not walked unless the package is a resolution root.
    Development,
}

/// Which checkout of a git dependency a request names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GitReference {
    /// The repository's default branch.
    Default,
    /// A branch.
    Branch(String),
    /// A tag.
    Tag(String),
    /// A commit id.
    Rev(String),
}

impl GitReference {
    /// The string a checkout command resolves; `None` leaves the clone on its default branch.
    pub fn checkout_arg(&self) -> Option<&str> {
        match self {
            Self::Default => None,
            Self::Branch(value) | Self::Tag(value) | Self::Rev(value) => Some(value),
        }
    }

    /// The stable label used for diagnostics and human-readable ids.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Branch(_) => "branch",
            Self::Tag(_) => "tag",
            Self::Rev(_) => "rev",
        }
    }
}

/// Where a dependency request says its package comes from.
///
/// A request is a *declaration*, not an identity: the provider turns it into one or more
/// [`Candidate`]s with resolved sources. `location` on the path/git variants is the string the
/// manifest wrote, and interpreting it (relative to which manifest, canonicalized how) is the
/// host provider's business.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceRequest {
    /// A Maven registry coordinate. The package being requested is the request's
    /// [`package`](DependencyRequest::package) name, which is a `group:artifact` coordinate.
    Registry {
        /// The registry to resolve from.
        registry: RegistryId,
    },
    /// A git repository.
    Git {
        /// The clone URL.
        url: String,
        /// The checkout selection.
        reference: GitReference,
        /// A source root within the checkout.
        dir: Option<String>,
    },
    /// A local directory project.
    Path {
        /// The directory, as declared (the provider resolves it).
        location: String,
        /// A source root within the directory.
        dir: Option<String>,
    },
    /// A precompiled binary read directly.
    Direct {
        /// Jar or wasm.
        kind: DirectKind,
        /// The URL or project-relative path.
        locator: String,
    },
    /// A workspace member.
    Workspace {
        /// The member's package name.
        member: String,
    },
}

impl fmt::Display for SourceRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry { registry } => write!(f, "registry+{registry}"),
            Self::Git {
                url,
                reference,
                dir,
            } => {
                write!(f, "git+{url}")?;
                if let Some(arg) = reference.checkout_arg() {
                    write!(f, "#{}={arg}", reference.label())?;
                }
                if let Some(dir) = dir {
                    write!(f, "?dir={dir}")?;
                }
                Ok(())
            }
            Self::Path { location, dir } => {
                write!(f, "path+{location}")?;
                if let Some(dir) = dir {
                    write!(f, "?dir={dir}")?;
                }
                Ok(())
            }
            Self::Direct { kind, locator } => write!(f, "{kind}+{locator}"),
            Self::Workspace { member } => write!(f, "workspace+{member}"),
        }
    }
}

/// One dependency edge, as its declaring package wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyRequest {
    /// The name the declaring manifest used for this edge. It is a *label* for diagnostics and
    /// for feature routing (`<label>/<feature>`); the package it denotes is
    /// [`package`](DependencyRequest::package).
    pub name: PackageName,
    /// The package this edge actually names. Equal to `name` unless the manifest renamed it with
    /// `package = "…"`.
    pub package: PackageName,
    /// Where the package comes from.
    pub source: SourceRequest,
    /// The version requirement. [`VersionReq`] with a `*` requirement is the default for
    /// sources that have no meaningful version (direct binaries, and path/git projects whose
    /// manifest declares none).
    pub version: VersionReq,
    /// Features this edge enables in the target.
    pub features: BTreeSet<String>,
    /// Whether the target's own `default` feature list applies through this edge.
    pub default_features: bool,
    /// Whether this edge is present only when an activating feature is on.
    pub optional: bool,
    /// Whether this edge is a build or a dev dependency.
    pub kind: DependencyKind,
}

impl DependencyRequest {
    /// Whether this edge's version requirement admits `version`.
    pub fn admits(&self, version: &crate::version::Version) -> bool {
        self.version.matches(version)
    }
}

/// Everything the resolver needs to know about one exact package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The package this summary describes, with its resolved source.
    pub id: PackageId,
    /// Its dependency edges, as declared.
    pub dependencies: Vec<DependencyRequest>,
    /// The feature graph: each feature to the values it enables.
    pub features: BTreeMap<String, Vec<FeatureValue>>,
}

impl Summary {
    /// The values a feature enables, or an empty slice.
    pub fn feature_values(&self, feature: &str) -> &[FeatureValue] {
        self.features.get(feature).map_or(&[], Vec::as_slice)
    }

    /// The dependency edges whose label is `name`.
    pub fn dependency(&self, name: &str) -> Option<&DependencyRequest> {
        self.dependencies
            .iter()
            .find(|dependency| dependency.name.as_str() == name)
    }

    /// Whether the package declares the feature `name`.
    pub fn declares_feature(&self, name: &str) -> bool {
        self.features.contains_key(name)
    }

    /// A one-line label for diagnostics.
    pub fn label(&self) -> String {
        format!("{} {}", self.id.name, self.id.version)
    }
}

/// One version a source can provide, as the provider found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The fully resolved package identity, including the source. A git candidate therefore
    /// carries the commit it resolved to.
    pub id: PackageId,
    /// The artifact checksum, when the source published one (Maven's `.sha1`/`.sha256` files) or
    /// an earlier verified download recorded it.
    pub checksum: Option<Checksum>,
}

impl Candidate {
    /// A candidate with no known checksum.
    pub const fn new(id: PackageId) -> Self {
        Self { id, checksum: None }
    }

    /// A candidate with a checksum.
    pub const fn with_checksum(id: PackageId, checksum: Checksum) -> Self {
        Self {
            id,
            checksum: Some(checksum),
        }
    }
}

/// A resolution root: a workspace member (or a single project) with its own manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRequest {
    /// The member's own summary. Its id's source is a workspace/path identity, and member roots
    /// are not lockfile entries.
    pub summary: Summary,
    /// Features selected on the command line (or by `--all-features`).
    pub features: BTreeSet<String>,
    /// Whether the root's own `default` feature list applies. `--no-default-features` says no.
    pub default_features: bool,
    /// Whether this root's `[dev-dependencies]` are resolved (a test/analysis run).
    pub include_dev: bool,
}

impl RootRequest {
    /// A root with no explicitly selected features and defaults on.
    pub const fn new(summary: Summary) -> Self {
        Self {
            summary,
            features: BTreeSet::new(),
            default_features: true,
            include_dev: false,
        }
    }
}
