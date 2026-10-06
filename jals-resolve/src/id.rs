//! Package and source identity.
//!
//! Identity is the whole point of this module: a package is `(name, version, resolved source)`,
//! so two declarations that reach the same bytes are the same package and two declarations that
//! only *look* similar are not. A locator string (`https://…/foo.jar`, a checkout directory, a
//! relative path) is an *acquisition instruction*, never an identity: the checkout path of a git
//! clone changes every run, and a path dependency's bytes change without its spelling moving.
//!
//! Registry names are the Maven coordinate `group:artifact`; source-project names are the
//! package's own `[package] name`. [`SourceId`] is the resolved half of that identity: a registry
//! name, a git `(url, commit, dir)`, a canonical path location, a content-addressed direct
//! binary, or a workspace member.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use core::fmt;
use core::hash::Hash;
use core::str::FromStr;

use crate::summary::SourceRequest;
use crate::version::Version;

/// A package name: a Maven `group:artifact` coordinate, or a source project's `[package] name`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageName(String);

impl PackageName {
    /// Validate and construct a package name.
    ///
    /// A name is non-empty, carries no whitespace or control characters, and contains at most one
    /// `:` separating two non-empty halves. `/` is reserved for `<dependency>/<feature>` routing
    /// and is rejected so a name can never be confused with a feature reference.
    ///
    /// # Errors
    /// [`NameError`] naming which rule was broken.
    pub fn new(raw: impl Into<String>) -> Result<Self, NameError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(NameError::Empty);
        }
        if raw.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
            return Err(NameError::Whitespace { raw });
        }
        if raw.contains('/') {
            return Err(NameError::Slash { raw });
        }
        match raw.split_once(':') {
            None => {}
            Some((group, artifact)) => {
                if group.is_empty() || artifact.is_empty() {
                    return Err(NameError::Coordinate { raw });
                }
                if artifact.contains(':') {
                    return Err(NameError::Coordinate { raw });
                }
            }
        }
        Ok(Self(raw))
    }

    /// The name as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `(group, artifact)` halves of a Maven coordinate, or `None` for a bare project name.
    pub fn maven_parts(&self) -> Option<(&str, &str)> {
        self.0.split_once(':')
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PackageName {
    type Err = NameError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::new(raw)
    }
}

/// A name that is not a valid package or registry name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    /// The name was empty.
    Empty,
    /// The name contained whitespace or a control character.
    Whitespace {
        /// The offending value.
        raw: String,
    },
    /// The name carried a `/`.
    Slash {
        /// The offending value.
        raw: String,
    },
    /// A `:` was used without two non-empty halves, or more than once.
    Coordinate {
        /// The offending value.
        raw: String,
    },
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a package name may not be empty"),
            Self::Whitespace { raw } => {
                write!(f, "`{raw}` contains whitespace or a control character")
            }
            Self::Slash { raw } => write!(
                f,
                "`{raw}` contains `/`, which names a feature reference (`<dependency>/<feature>`)"
            ),
            Self::Coordinate { raw } => write!(
                f,
                "`{raw}` is not a Maven coordinate: expected `group:artifact` with both halves non-empty"
            ),
        }
    }
}

impl core::error::Error for NameError {}

/// The name of a Maven registry (`maven-central`, `internal`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegistryId(String);

impl RegistryId {
    /// The implicit registry every Maven coordinate resolves from unless it names another.
    pub const MAVEN_CENTRAL: &'static str = "maven-central";

    /// Validate and construct a registry name.
    ///
    /// # Errors
    /// [`NameError::Empty`] or [`NameError::Whitespace`]; `:` and `/` are rejected for the same
    /// reason they are in [`PackageName`].
    pub fn new(raw: impl Into<String>) -> Result<Self, NameError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(NameError::Empty);
        }
        if raw.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
            return Err(NameError::Whitespace { raw });
        }
        if raw.contains('/') {
            return Err(NameError::Slash { raw });
        }
        if raw.contains(':') {
            return Err(NameError::Coordinate { raw });
        }
        Ok(Self(raw))
    }

    /// The registry name as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RegistryId {
    fn default() -> Self {
        Self(Self::MAVEN_CENTRAL.to_owned())
    }
}

impl fmt::Display for RegistryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RegistryId {
    type Err = NameError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::new(raw)
    }
}

/// A content checksum pinned in the lockfile.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Checksum {
    algorithm: ChecksumAlgorithm,
    hex: String,
}

/// A checksum algorithm a lock entry may pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChecksumAlgorithm {
    /// SHA-1, what Maven repositories publish beside most artifacts.
    Sha1,
    /// SHA-256, what jals publishes in its own cache.
    Sha256,
    /// SHA-512.
    Sha512,
}

impl ChecksumAlgorithm {
    /// The spelling used in manifests and the lockfile.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    /// The number of hex digits a digest of this algorithm has.
    const fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }
}

impl fmt::Display for ChecksumAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Checksum {
    /// Construct a checksum from an algorithm and a lowercase-or-uppercase hex digest.
    ///
    /// # Errors
    /// [`ChecksumError`] when the digest is not hex or has the wrong length for the algorithm.
    pub fn new(algorithm: ChecksumAlgorithm, hex: &str) -> Result<Self, ChecksumError> {
        if hex.len() != algorithm.hex_len() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ChecksumError::Digest {
                algorithm,
                hex: hex.to_owned(),
            });
        }
        Ok(Self {
            algorithm,
            hex: hex.to_ascii_lowercase(),
        })
    }

    /// The algorithm.
    pub const fn algorithm(&self) -> ChecksumAlgorithm {
        self.algorithm
    }

    /// The lowercase hex digest.
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// Parse the `sha256:<hex>` spelling the lockfile uses.
    ///
    /// # Errors
    /// [`ChecksumError::Algorithm`] for an unknown algorithm, or [`ChecksumError::Digest`].
    pub fn parse(text: &str) -> Result<Self, ChecksumError> {
        let (algorithm, hex) = text.split_once(':').ok_or_else(|| ChecksumError::Format {
            value: text.to_owned(),
        })?;
        let algorithm = match algorithm {
            "sha1" => ChecksumAlgorithm::Sha1,
            "sha256" => ChecksumAlgorithm::Sha256,
            "sha512" => ChecksumAlgorithm::Sha512,
            other => {
                return Err(ChecksumError::Algorithm {
                    value: other.to_owned(),
                });
            }
        };
        Self::new(algorithm, hex)
    }

    /// The `sha256:<hex>` spelling.
    pub fn render(&self) -> String {
        format!("{}:{}", self.algorithm, self.hex)
    }
}

impl fmt::Display for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.hex)
    }
}

impl FromStr for Checksum {
    type Err = ChecksumError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// A checksum that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChecksumError {
    /// The value was not `<algorithm>:<hex>`.
    Format {
        /// The offending value.
        value: String,
    },
    /// The algorithm was unknown.
    Algorithm {
        /// The offending algorithm.
        value: String,
    },
    /// The digest was not hex, or had the wrong length.
    Digest {
        /// The algorithm that was named.
        algorithm: ChecksumAlgorithm,
        /// The offending digest.
        hex: String,
    },
}

impl fmt::Display for ChecksumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format { value } => {
                write!(f, "`{value}` is not `<algorithm>:<hex>`")
            }
            Self::Algorithm { value } => write!(
                f,
                "`{value}` is not a known checksum algorithm (`sha1`, `sha256`, `sha512`)"
            ),
            Self::Digest { algorithm, hex } => write!(
                f,
                "`{hex}` is not a {} hex digest ({} digits)",
                algorithm,
                algorithm.hex_len()
            ),
        }
    }
}

impl core::error::Error for ChecksumError {}

/// Which kind of precompiled binary a direct dependency names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DirectKind {
    /// A `.jar`.
    Jar,
    /// A `.wasm` module.
    Wasm,
}

impl DirectKind {
    /// The spelling used in sources and the lockfile.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jar => "jar",
            Self::Wasm => "wasm",
        }
    }
}

impl fmt::Display for DirectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DirectKind {
    type Err = DirectKindError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "jar" => Ok(Self::Jar),
            "wasm" => Ok(Self::Wasm),
            _ => Err(DirectKindError {
                value: text.to_owned(),
            }),
        }
    }
}

/// An unknown [`DirectKind`] spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectKindError {
    /// The offending value.
    pub value: String,
}

impl fmt::Display for DirectKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not `jar` or `wasm`", self.value)
    }
}

impl core::error::Error for DirectKindError {}

/// A git source, resolved to the commit the lock pins.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitSource {
    /// The repository URL, as the manifest declared it.
    pub url: String,
    /// The full commit id the checkout resolved to.
    pub commit: String,
    /// The source root *within* the checkout, when one was selected.
    pub dir: Option<String>,
}

/// A path source, resolved to a stable location string.
///
/// The location is opaque to this crate: the native host canonicalizes a directory, the
/// in-memory host uses a project-relative path. It is stable for one project tree, which is what
/// identity requires; it is deliberately not interpreted here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathSource {
    /// The stable location.
    pub location: String,
}

/// A direct binary source, content-addressed once resolved.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DirectSource {
    /// Whether the binary is a jar or a wasm module.
    pub kind: DirectKind,
    /// The locator the manifest wrote (a URL or a project-relative path).
    pub locator: String,
    /// The content digest, once known.
    pub digest: Option<Checksum>,
}

/// A workspace member as a package source.
///
/// Members are resolution roots, not locked packages, so this source never appears in
/// `jals.lock`; it exists so an edge that points at a member is distinguishable from an edge to
/// an ordinary `path` dependency, and so `ResolveGraph` can name the member a dependency reaches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceSource {
    /// The member's stable key: its package name.
    pub member: String,
}

/// Where a resolved package's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceId {
    /// A Maven registry coordinate.
    Registry(RegistryId),
    /// A git checkout at a commit.
    Git(GitSource),
    /// A local directory.
    Path(PathSource),
    /// A precompiled binary.
    Direct(DirectSource),
    /// A workspace member.
    Workspace(WorkspaceSource),
}

impl SourceId {
    /// Whether a locked source could satisfy a declaration's source request.
    ///
    /// This is the lock-preference test: the lock's *resolved* source (a git commit, a content
    /// digest) is accepted when it could have come from the declared source. The version
    /// requirement is checked separately by the resolver.
    pub fn satisfies(&self, request: &SourceRequest) -> bool {
        match (self, request) {
            (Self::Registry(resolved), SourceRequest::Registry { registry, .. }) => {
                resolved == registry
            }
            (Self::Git(resolved), SourceRequest::Git { url, .. }) => resolved.url == *url,
            (Self::Path(resolved), SourceRequest::Path { location, .. }) => {
                resolved.location == *location
            }
            (Self::Direct(resolved), SourceRequest::Direct { kind, locator }) => {
                resolved.kind == *kind && resolved.locator == *locator
            }
            (Self::Workspace(resolved), SourceRequest::Workspace { member }) => {
                resolved.member == *member
            }
            _ => false,
        }
    }

    /// The stable string the lockfile stores.
    pub fn render(&self) -> String {
        match self {
            Self::Registry(registry) => format!("registry+{registry}"),
            Self::Git(git) => {
                let mut rendered = format!(
                    "git+{}#{}",
                    Self::encode(&git.url),
                    Self::encode(&git.commit)
                );
                if let Some(dir) = &git.dir {
                    rendered.push_str("?dir=");
                    rendered.push_str(&Self::encode(dir));
                }
                rendered
            }
            Self::Path(path) => format!("path+{}", Self::encode(&path.location)),
            Self::Direct(direct) => {
                let mut rendered =
                    format!("{}+{}", direct.kind.as_str(), Self::encode(&direct.locator));
                if let Some(digest) = &direct.digest {
                    rendered.push('#');
                    rendered.push_str(&digest.render());
                }
                rendered
            }
            Self::Workspace(workspace) => format!("workspace+{}", Self::encode(&workspace.member)),
        }
    }

    /// Parse the lockfile spelling produced by [`render`](SourceId::render).
    ///
    /// # Errors
    /// [`SourceError`] when the scheme is unknown or a component is malformed.
    pub fn parse(text: &str) -> Result<Self, SourceError> {
        if let Some(rest) = text.strip_prefix("registry+") {
            return RegistryId::new(rest)
                .map(Self::Registry)
                .map_err(|_| SourceError::Malformed {
                    value: text.to_owned(),
                });
        }
        if let Some(rest) = text.strip_prefix("git+") {
            let (locator, rest) = rest.split_once('#').ok_or_else(|| SourceError::Malformed {
                value: text.to_owned(),
            })?;
            let (commit, dir) = match rest.split_once("?dir=") {
                Some((commit, dir)) => (commit, Some(Self::decode(dir))),
                None => (rest, None),
            };
            return Ok(Self::Git(GitSource {
                url: Self::decode(locator),
                commit: Self::decode(commit),
                dir,
            }));
        }
        if let Some(rest) = text.strip_prefix("path+") {
            return Ok(Self::Path(PathSource {
                location: Self::decode(rest),
            }));
        }
        if let Some(rest) = text.strip_prefix("workspace+") {
            return Ok(Self::Workspace(WorkspaceSource {
                member: Self::decode(rest),
            }));
        }
        for kind in [DirectKind::Jar, DirectKind::Wasm] {
            if let Some(rest) = text.strip_prefix(&format!("{}+", kind.as_str())) {
                let (locator, digest) = match rest.split_once('#') {
                    Some((locator, digest)) => (
                        locator,
                        Some(Checksum::parse(digest).map_err(|_| SourceError::Malformed {
                            value: text.to_owned(),
                        })?),
                    ),
                    None => (rest, None),
                };
                return Ok(Self::Direct(DirectSource {
                    kind,
                    locator: Self::decode(locator),
                    digest,
                }));
            }
        }
        Err(SourceError::Scheme {
            value: text.to_owned(),
        })
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

/// A source string that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// The string named no known source scheme.
    Scheme {
        /// The offending value.
        value: String,
    },
    /// The scheme was known but a component was missing or malformed.
    Malformed {
        /// The offending value.
        value: String,
    },
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scheme { value } => write!(
                f,
                "`{value}` names no known source (`registry+`, `git+`, `path+`, `jar+`, `wasm+`, `workspace+`)"
            ),
            Self::Malformed { value } => write!(f, "`{value}` is a malformed source string"),
        }
    }
}

impl core::error::Error for SourceError {}

impl SourceId {
    /// Escape the characters that separate a source string's components.
    fn encode(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for ch in text.chars() {
            match ch {
                '%' => out.push_str("%25"),
                '#' => out.push_str("%23"),
                '?' => out.push_str("%3F"),
                other => out.push(other),
            }
        }
        out
    }

    /// Reverse [`SourceId::encode`].
    fn decode(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            if ch != '%' {
                out.push(ch);
                continue;
            }
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                out.push(byte as char);
            } else {
                out.push('%');
                out.push_str(&hex);
            }
        }
        out
    }
}

/// A fully identified package: name, version, and resolved source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageId {
    /// The package's own name (`group:artifact` for registry packages).
    pub name: PackageName,
    /// The selected version.
    pub version: Version,
    /// The resolved source.
    pub source: SourceId,
}

impl PackageId {
    /// Construct an id from its parts.
    pub const fn new(name: PackageName, version: Version, source: SourceId) -> Self {
        Self {
            name,
            version,
            source,
        }
    }

    /// The lockfile spelling of a dependency edge: `name version (source)`.
    pub fn render_edge(&self) -> String {
        format!("{} {} ({})", self.name, self.version, self.source)
    }

    /// Parse [`render_edge`](PackageId::render_edge).
    ///
    /// # Errors
    /// [`EdgeError`] when the three components are not present.
    pub fn parse_edge(text: &str) -> Result<Self, EdgeError> {
        let (name, rest) = text.split_once(' ').ok_or_else(|| EdgeError {
            value: text.to_owned(),
        })?;
        let rest = rest.trim();
        let (version, source) = rest.rsplit_once(" (").ok_or_else(|| EdgeError {
            value: text.to_owned(),
        })?;
        let source = source.strip_suffix(')').ok_or_else(|| EdgeError {
            value: text.to_owned(),
        })?;
        Ok(Self {
            name: PackageName::new(name).map_err(|_| EdgeError {
                value: text.to_owned(),
            })?,
            version: Version::parse(version).map_err(|_| EdgeError {
                value: text.to_owned(),
            })?,
            source: SourceId::parse(source).map_err(|_| EdgeError {
                value: text.to_owned(),
            })?,
        })
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} ({})", self.name, self.version, self.source)
    }
}

/// A package edge string that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeError {
    /// The offending value.
    pub value: String,
}

impl fmt::Display for EdgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not `name version (source)`", self.value)
    }
}

impl core::error::Error for EdgeError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn pkg_id(name: &str, version: &str, source: SourceId) -> PackageId {
        PackageId::new(pkg(name), Version::parse(version).unwrap(), source)
    }

    #[test]
    fn package_names_validate_coordinates() {
        assert!(PackageName::new("guava").is_ok());
        assert!(PackageName::new("com.google.guava:guava").is_ok());
        assert!(PackageName::new("").is_err());
        assert!(PackageName::new("a:b:c").is_err());
        assert!(PackageName::new(":guava").is_err());
        assert!(PackageName::new("guava:").is_err());
        assert!(PackageName::new("a b").is_err());
        assert!(PackageName::new("dep/feature").is_err());
    }

    #[test]
    fn maven_parts_split_once() {
        assert_eq!(
            pkg("com.google.guava:guava").maven_parts(),
            Some(("com.google.guava", "guava"))
        );
        assert_eq!(pkg("guava").maven_parts(), None);
    }

    #[test]
    fn source_ids_round_trip() {
        let sources = [
            SourceId::Registry(RegistryId::new("maven-central").unwrap()),
            SourceId::Git(GitSource {
                url: "https://github.com/example/sdk".to_owned(),
                commit: "0f1e2d3c4b5a69788796a5b4c3d2e1f009182736".to_owned(),
                dir: Some("core/lib".to_owned()),
            }),
            SourceId::Path(PathSource {
                location: "/home/user/projects/sdk".to_owned(),
            }),
            SourceId::Direct(DirectSource {
                kind: DirectKind::Jar,
                locator: "libs/legacy.jar".to_owned(),
                digest: Some(Checksum::new(ChecksumAlgorithm::Sha256, &"a".repeat(64)).unwrap()),
            }),
            SourceId::Direct(DirectSource {
                kind: DirectKind::Wasm,
                locator: "https://example.test/host.wasm".to_owned(),
                digest: None,
            }),
            SourceId::Workspace(WorkspaceSource {
                member: "app".to_owned(),
            }),
        ];
        for source in sources {
            let rendered = source.render();
            assert_eq!(SourceId::parse(&rendered), Ok(source), "{rendered}");
        }
    }

    #[test]
    fn source_strings_escape_separators() {
        let source = SourceId::Git(GitSource {
            url: "https://example.test/a#b?c%25".to_owned(),
            commit: "abc".to_owned(),
            dir: Some("x#y".to_owned()),
        });
        let rendered = source.render();
        assert_eq!(
            rendered,
            "git+https://example.test/a%23b%3Fc%2525#abc?dir=x%23y"
        );
        assert_eq!(SourceId::parse(&rendered).unwrap(), source);
    }

    #[test]
    fn package_ids_round_trip_through_edge_strings() {
        let id = pkg_id(
            "org.slf4j:slf4j-api",
            "2.0.16",
            SourceId::Registry(RegistryId::default()),
        );
        let rendered = id.render_edge();
        assert_eq!(
            rendered,
            "org.slf4j:slf4j-api 2.0.16 (registry+maven-central)"
        );
        assert_eq!(PackageId::parse_edge(&rendered).unwrap(), id);
    }

    #[test]
    fn checksums_validate_length_and_hex() {
        assert!(Checksum::new(ChecksumAlgorithm::Sha1, &"a".repeat(40)).is_ok());
        assert!(Checksum::new(ChecksumAlgorithm::Sha1, &"a".repeat(39)).is_err());
        assert!(Checksum::new(ChecksumAlgorithm::Sha256, &"g".repeat(64)).is_err());
        let checksum = Checksum::parse(&format!("sha256:{}", "AB".repeat(32))).unwrap();
        assert_eq!(checksum.render(), format!("sha256:{}", "ab".repeat(32)));
    }

    #[test]
    fn unknown_sources_are_rejected() {
        assert!(matches!(
            SourceId::parse("npm+lodash"),
            Err(SourceError::Scheme { .. })
        ));
        assert!(SourceId::parse("git+https://example.test/repo").is_err());
    }
}
