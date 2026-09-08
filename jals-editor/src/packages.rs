//! Reading the packages a **project** declares, out of the project's own storage.
//!
//! `[packages]` is the third route a package name resolves through, and the one that needs a
//! filesystem: the Java is the project's, so somebody has to read it before it can be a package.
//! That somebody is here rather than in each host, for the reason the fidelity lowering is here —
//! three hosts index packages (the CLI's `jals lint`, the language server, the browser playground),
//! none depends on the other two, and a reader written per host is three copies of a rule.
//!
//! It reads through [`ProjectView`], so the browser gets the same answer as the CLI over an
//! in-memory tree. No host path is named anywhere in this file.

use alloc::borrow::ToOwned;
use alloc::string::ToString;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use jals_config::{ProjectPackage, ProjectPackageKind};
use jals_native::{SourceKind, SourceResolver};
use jals_storage::{CacheBackend, ProjectStorage, SourceBackend};

/// A warning about a `[packages]` entry that could not be read.
///
/// Carried rather than raised, and *not* fatal: a host's policy for a package it cannot resolve is
/// the host's — `jals build` refuses, the language server degrades — and this layer's job is to say
/// what it found, not to decide what that means. A package whose directory holds no `.java` at all
/// is one of these rather than an empty package, because an empty package resolves fine and then
/// every reference into it reads as an unresolved name with nothing said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageWarning {
    /// The `[packages]` key.
    pub name: String,
    /// What was wrong, as a sentence fragment following the name.
    pub problem: String,
}

impl core::fmt::Display for PackageWarning {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "`[packages.\"{}\"]` {}", self.name, self.problem)
    }
}

/// The packages a project declares, read into a resolver.
pub struct ProjectPackages;

impl ProjectPackages {
    /// Read every declared package's `.java` and build the route that offers them.
    ///
    /// Files are taken in sorted path order, which is what makes the resulting package — and every
    /// digest over it — independent of the order a tree happened to enumerate.
    ///
    /// A [`ProjectPackageKind`] becomes a [`SourceKind`] here, and this is the only place the two
    /// vocabularies meet: the manifest schema does not depend on the package vocabulary, because
    /// `jals-config` is what a project writes and `jals-native` is what a package author writes.
    pub fn resolver<S: SourceBackend, C: CacheBackend>(
        storage: &ProjectStorage<S, C>,
        declared: &BTreeMap<String, ProjectPackage>,
    ) -> (SourceResolver, Vec<PackageWarning>) {
        let mut resolver = SourceResolver::new("declared by this project");
        let mut warnings = Vec::new();
        if declared.is_empty() {
            return (resolver, warnings);
        }
        let view = storage.view();
        for (name, package) in declared {
            let Ok(root) = jals_storage::DirKey::parse(&package.java) else {
                warnings.push(PackageWarning {
                    name: name.clone(),
                    problem: alloc::format!(
                        "names `{}`, which is not a project-relative directory",
                        package.java
                    ),
                });
                continue;
            };
            let mut paths: Vec<_> = view
                .tree()
                .files_under(&root)
                .filter(|file| file.key().has_extension("java"))
                .map(|file| file.key().clone())
                .collect();
            paths.sort();

            let mut sources = Vec::with_capacity(paths.len());
            for key in paths {
                // A file the snapshot captured but cannot decode is a *warning*, not a silent
                // omission: a package missing one class is a package whose every reference into
                // that class reads as an unresolved name, with nothing saying why.
                let Ok(text) = view.file_text(&key) else {
                    warnings.push(PackageWarning {
                        name: name.clone(),
                        problem: alloc::format!("could not read `{}`", key.path()),
                    });
                    continue;
                };
                sources.push((
                    key.path().to_string(),
                    text.to_owned(),
                    Self::kind(package.kind),
                ));
            }

            if sources.is_empty() {
                warnings.push(PackageWarning {
                    name: name.clone(),
                    problem: alloc::format!("holds no `.java` under `{}`", package.java),
                });
                continue;
            }
            resolver.declare(name, sources);
        }
        (resolver, warnings)
    }

    const fn kind(kind: ProjectPackageKind) -> SourceKind {
        match kind {
            ProjectPackageKind::Implementation => SourceKind::Implementation,
            ProjectPackageKind::Signatures => SourceKind::Signatures,
        }
    }
}
