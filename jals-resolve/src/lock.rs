//! The `jals.lock` model.
//!
//! The lock is a *complete* record of the external packages a workspace resolved to: each entry
//! is one [`PackageId`](crate::id::PackageId), its optional checksum, and the package edges it
//! was resolved with. Root workspace members are deliberately absent — their manifests are the
//! input to resolution, not its output — so an edit to a member's `jals.toml` re-resolves against
//! the same pinned external set.
//!
//! Parsing uses `toml` + serde; rendering is a hand-written deterministic writer, because the
//! workspace's `toml` dependency deliberately has no `display` feature (see `jals-config`'s
//! manifest: schema tests walk JSON for the same reason). Byte-stability matters here: the
//! resolver fingerprint hashes this rendering, so a collection order that leaks into the file
//! would move every cache key.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;

use serde::Deserialize;

use crate::id::{Checksum, PackageId, PackageName, SourceId};
use crate::version::Version;

/// The lockfile format version this crate writes.
pub const LOCK_VERSION: u32 = 1;

/// The file name a workspace root carries.
pub const LOCK_FILE_NAME: &str = "jals.lock";

/// A parsed `jals.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    /// The format version.
    pub version: u32,
    /// Every locked package, sorted by id.
    pub packages: Vec<LockedPackage>,
}

/// One locked package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    /// The exact package identity.
    pub id: PackageId,
    /// The artifact checksum, when known.
    pub checksum: Option<Checksum>,
    /// The package edges this lock entry was resolved with.
    pub dependencies: Vec<PackageId>,
}

impl LockedPackage {
    /// Find a locked entry by package name.
    pub fn find<'a>(packages: &'a [Self], name: &PackageName) -> Option<&'a Self> {
        packages.iter().find(|package| &package.id.name == name)
    }
}

impl Lockfile {
    /// An empty lock at the current format version.
    pub const fn new() -> Self {
        Self {
            version: LOCK_VERSION,
            packages: Vec::new(),
        }
    }

    /// The locked entry for `name`, if any.
    pub fn find(&self, name: &PackageName) -> Option<&LockedPackage> {
        LockedPackage::find(&self.packages, name)
    }

    /// Parse a lockfile.
    ///
    /// # Errors
    /// [`LockError::Parse`] for a TOML error, [`LockError::Version`] for a format version this
    /// crate does not understand, and [`LockError::Malformed`] for a syntactically valid file
    /// with a bad name, version, source, or edge.
    pub fn parse(text: &str) -> Result<Self, LockError> {
        let raw: RawLockfile = toml::from_str(text).map_err(LockError::Parse)?;
        if raw.version != LOCK_VERSION {
            return Err(LockError::Version {
                found: raw.version,
                expected: LOCK_VERSION,
            });
        }
        let mut packages = Vec::with_capacity(raw.package.len());
        for package in raw.package {
            let name = PackageName::new(&package.name).map_err(|_| LockError::Malformed {
                value: format!("package name `{}`", package.name),
            })?;
            let version = Version::parse(&package.version).map_err(|_| LockError::Malformed {
                value: format!("version `{}` of `{}`", package.version, package.name),
            })?;
            let source = SourceId::parse(&package.source).map_err(|_| LockError::Malformed {
                value: format!("source `{}` of `{}`", package.source, package.name),
            })?;
            let checksum = match &package.checksum {
                Some(text) => Some(Checksum::parse(text).map_err(|_| LockError::Malformed {
                    value: format!("checksum `{text}` of `{}`", package.name),
                })?),
                None => None,
            };
            let mut dependencies = Vec::with_capacity(package.dependencies.len());
            for dependency in package.dependencies {
                dependencies.push(PackageId::parse_edge(&dependency).map_err(|_| {
                    LockError::Malformed {
                        value: format!("dependency `{dependency}` of `{}`", package.name),
                    }
                })?);
            }
            dependencies.sort();
            packages.push(LockedPackage {
                id: PackageId::new(name, version, source),
                checksum,
                dependencies,
            });
        }
        packages.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(Self {
            version: raw.version,
            packages,
        })
    }

    /// Render the lockfile deterministically: packages by id, dependencies by id, so two equal
    /// locks always render byte-identically and the resolver fingerprint never moves for a
    /// collection-order change.
    pub fn render(&self) -> String {
        let mut packages: Vec<&LockedPackage> = self.packages.iter().collect();
        packages.sort_by(|left, right| left.id.cmp(&right.id));
        let mut out = String::new();
        out.push_str("# This file is automatically generated by jals.\n");
        out.push_str("# It is not intended for manual editing.\n");
        out.push_str("version = ");
        out.push_str(&self.version.to_string());
        out.push('\n');
        for package in packages {
            out.push('\n');
            out.push_str("[[package]]\n");
            out.push_str("name = ");
            out.push_str(&Self::quote(package.id.name.as_str()));
            out.push('\n');
            out.push_str("version = ");
            out.push_str(&Self::quote(package.id.version.as_str()));
            out.push('\n');
            out.push_str("source = ");
            out.push_str(&Self::quote(&package.id.source.render()));
            out.push('\n');
            if let Some(checksum) = &package.checksum {
                out.push_str("checksum = ");
                out.push_str(&Self::quote(&checksum.render()));
                out.push('\n');
            }
            let dependencies: BTreeSet<&PackageId> = package.dependencies.iter().collect();
            if dependencies.is_empty() {
                out.push_str("dependencies = []\n");
            } else {
                out.push_str("dependencies = [\n");
                for dependency in &dependencies {
                    out.push_str("    ");
                    out.push_str(&Self::quote(&dependency.render_edge()));
                    out.push_str(",\n");
                }
                out.push_str("]\n");
            }
        }
        out
    }
}

impl Default for Lockfile {
    fn default() -> Self {
        Self::new()
    }
}

impl FromStr for Lockfile {
    type Err = LockError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// The serde shape of a lockfile; converted into validated types by [`Lockfile::parse`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLockfile {
    version: u32,
    #[serde(default, rename = "package")]
    package: Vec<RawPackage>,
}

/// One `[[package]]` table before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawPackage {
    name: String,
    version: String,
    source: String,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

/// A lockfile that could not be read.
#[derive(Debug)]
pub enum LockError {
    /// TOML syntax or schema error.
    Parse(toml::de::Error),
    /// A format version this crate does not understand.
    Version {
        /// The version found in the file.
        found: u32,
        /// The version this crate writes.
        expected: u32,
    },
    /// A field was present but malformed.
    Malformed {
        /// What was malformed.
        value: String,
    },
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "`{LOCK_FILE_NAME}` is not valid TOML: {error}"),
            Self::Version { found, expected } => write!(
                f,
                "`{LOCK_FILE_NAME}` has format version {found}, but this jals writes version {expected}"
            ),
            Self::Malformed { value } => write!(f, "`{LOCK_FILE_NAME}` has a malformed {value}"),
        }
    }
}

impl core::error::Error for LockError {}

impl Lockfile {
    /// Quote a string as a TOML basic string.
    fn quote(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                ch if ch.is_control() => {
                    out.push_str("\\u");
                    let mut buffer = [0u16; 2];
                    for unit in ch.encode_utf16(&mut buffer) {
                        let _ = core::fmt::Write::write_fmt(&mut out, format_args!("{unit:04X}"));
                    }
                }
                ch => out.push(ch),
            }
        }
        out.push('"');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{ChecksumAlgorithm, DirectKind, DirectSource, RegistryId};
    use alloc::borrow::ToOwned as _;

    fn package(name: &str, version: &str) -> LockedPackage {
        LockedPackage {
            id: PackageId::new(
                PackageName::new(name).unwrap(),
                Version::parse(version).unwrap(),
                SourceId::Registry(RegistryId::default()),
            ),
            checksum: None,
            dependencies: Vec::new(),
        }
    }

    #[test]
    fn rendering_is_deterministic_and_parses_back() {
        let lock = Lockfile {
            version: LOCK_VERSION,
            packages: alloc::vec![
                LockedPackage {
                    checksum: Some(
                        Checksum::new(ChecksumAlgorithm::Sha256, &"a".repeat(64)).unwrap(),
                    ),
                    dependencies: alloc::vec![package("org.slf4j:slf4j-api", "2.0.16").id],
                    ..package("com.example:app", "1.0.0")
                },
                package("org.slf4j:slf4j-api", "2.0.16"),
            ],
        };
        let rendered = lock.render();
        let parsed = Lockfile::parse(&rendered).unwrap();
        assert_eq!(parsed, lock);
        assert_eq!(parsed.render(), rendered);
    }

    #[test]
    fn sources_with_quotes_and_backslashes_round_trip() {
        let lock = Lockfile {
            version: LOCK_VERSION,
            packages: alloc::vec![LockedPackage {
                id: PackageId::new(
                    PackageName::new("a:b").unwrap(),
                    Version::parse("1").unwrap(),
                    SourceId::Path(crate::id::PathSource {
                        location: "C:\\odd\"path".to_owned(),
                    }),
                ),
                checksum: None,
                dependencies: Vec::new(),
            }],
        };
        let parsed = Lockfile::parse(&lock.render()).unwrap();
        assert_eq!(parsed, lock);
    }

    #[test]
    fn unknown_fields_and_versions_are_rejected() {
        assert!(matches!(
            Lockfile::parse("version = 1\nunexpected = true\n"),
            Err(LockError::Parse(_))
        ));
        assert!(matches!(
            Lockfile::parse("version = 99\n"),
            Err(LockError::Version { found: 99, .. })
        ));
        assert!(matches!(
            Lockfile::parse(
                "version = 1\n[[package]]\nname = \"a\"\nversion = \"1\"\nsource = \"nope\"\ndependencies = []\n"
            ),
            Err(LockError::Malformed { .. })
        ));
    }

    #[test]
    fn direct_sources_keep_their_digest_in_the_lock() {
        let lock = Lockfile {
            version: LOCK_VERSION,
            packages: alloc::vec![LockedPackage {
                id: PackageId::new(
                    PackageName::new("legacy").unwrap(),
                    Version::parse("0.0.0").unwrap(),
                    SourceId::Direct(DirectSource {
                        kind: DirectKind::Jar,
                        locator: "libs/legacy.jar".to_owned(),
                        digest: Some(
                            Checksum::new(ChecksumAlgorithm::Sha256, &"b".repeat(64)).unwrap(),
                        ),
                    }),
                ),
                checksum: None,
                dependencies: Vec::new(),
            }],
        };
        let parsed = Lockfile::parse(&lock.render()).unwrap();
        assert_eq!(parsed, lock);
    }
}
