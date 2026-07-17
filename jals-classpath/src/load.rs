//! Archive adapter and classpath byte loading.
//!
//! `zip` is isolated in this module: it consumes bytes from a project revision or artifact cache and
//! never opens a host path.

use std::io::{Cursor, Read};

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_classfile::ClassFile;
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, DirKey, FileKey, Name, ProjectView,
    RelativePath,
};

use crate::{DependencyResolver, LibrarySource, Warning, WarningOrigin};

const MAX_NESTED_JAR_DEPTH: usize = 64;

/// A typed classpath input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClasspathEntry {
    ProjectFile(FileKey),
    ProjectDirectory(DirKey),
    Artifact(CacheKey),
}

/// Parsed class files plus non-fatal per-entry diagnostics.
#[derive(Debug, Default)]
pub struct ClasspathLoad {
    pub classes: Vec<ClassFile>,
    pub warnings: Vec<Warning>,
}

impl ClasspathLoad {
    /// Load from exactly one immutable project revision and a verified artifact cache.
    pub fn load<C: CacheBackend>(
        view: &ProjectView,
        cache: &ArtifactCache<C>,
        entries: &[ClasspathEntry],
    ) -> Self {
        let mut load = Self::default();
        for entry in entries {
            match entry {
                ClasspathEntry::ProjectFile(key) => load.load_project_file(view, key),
                ClasspathEntry::ProjectDirectory(key) => load.load_project_dir(view, key),
                ClasspathEntry::Artifact(key) => match cache.lookup(key) {
                    Ok(Some(bytes)) => {
                        load.load_jar_bytes(&WarningOrigin::Artifact(key.clone()), &bytes);
                    }
                    Ok(None) => load.warn(
                        WarningOrigin::Artifact(key.clone()),
                        "classpath artifact is not cached",
                    ),
                    Err(error) => load.warn(
                        WarningOrigin::Artifact(key.clone()),
                        &format!("classpath artifact is invalid: {error:?}"),
                    ),
                },
            }
        }
        load
    }

    fn load_project_file(&mut self, view: &ProjectView, key: &FileKey) {
        let origin = WarningOrigin::ProjectFile(key.clone());
        let file = match view.file(key) {
            Ok(file) => file,
            Err(error) => {
                return self.warn(origin, &format!("classpath file cannot be read: {error}"));
            }
        };
        match key.extension() {
            Some(ext) if ext.eq_ignore_ascii_case("class") => {
                self.parse_into(origin, file.bytes());
            }
            Some(ext) if ext.eq_ignore_ascii_case("jar") || ext.eq_ignore_ascii_case("zip") => {
                self.load_jar_bytes(&origin, file.bytes());
            }
            _ => self.warn(
                origin,
                "unrecognized classpath file (expected `.class`, `.jar`, or `.zip`)",
            ),
        }
    }

    fn load_project_dir(&mut self, view: &ProjectView, key: &DirKey) {
        if let Err(error) = view.directory(key) {
            self.warn(
                WarningOrigin::ProjectDirectory(key.clone()),
                &format!("classpath directory cannot be read: {error}"),
            );
            return;
        }
        for file in view.tree().files_under(key) {
            if file.key().has_extension("class") {
                self.parse_into(WarningOrigin::ProjectFile(file.key().clone()), file.bytes());
            }
        }
    }

    fn load_jar_bytes(&mut self, origin: &WarningOrigin, bytes: &[u8]) {
        let classes = &mut self.classes;
        Archive::walk_members(
            bytes,
            origin,
            |name| Archive::extension(name).is_some_and(|ext| ext.eq_ignore_ascii_case("class")),
            |_, bytes| match ClassFile::read(bytes) {
                Ok(class) => {
                    classes.push(class);
                    Ok(())
                }
                Err(error) => Err(format!("failed to parse class file: {error}")),
            },
            &mut self.warnings,
        );
    }

    fn parse_into(&mut self, origin: WarningOrigin, bytes: &[u8]) {
        match ClassFile::read(bytes) {
            Ok(class) => self.classes.push(class),
            Err(error) => self.warn(origin, &format!("failed to parse class file: {error}")),
        }
    }

    fn warn(&mut self, origin: WarningOrigin, message: &str) {
        self.warnings.push(Warning::new(origin, message));
    }
}

/// A nested jar stored in the nested-jar namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedJar {
    pub member: RelativePath,
    pub key: CacheKey,
}

/// Archive extraction result. Invalid members are skipped, never partially published.
#[derive(Debug)]
pub struct JarExtraction<T> {
    pub artifacts: Vec<T>,
    pub warnings: Vec<Warning>,
}

impl<T> Default for JarExtraction<T> {
    fn default() -> Self {
        Self {
            artifacts: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

impl JarExtraction<LibrarySource> {
    /// Extract `.java` members from verified jar artifacts.
    pub fn sources<C: CacheBackend>(cache: &mut ArtifactCache<C>, jars: &[CacheKey]) -> Self {
        let mut out = Self::default();
        for jar in jars {
            let bytes = match cache.lookup(jar) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    out.warn(jar, "sources jar is not cached");
                    continue;
                }
                Err(error) => {
                    out.warn(jar, &format!("sources jar is invalid: {error:?}"));
                    continue;
                }
            };
            let prefix = RelativePath::new([
                Name::new(jar.provenance().to_hex()).expect("digest hex is a portable name")
            ]);
            Archive::extract_members(
                &bytes,
                jar,
                "java",
                |member, bytes| {
                    let key =
                        Archive::member_key(CacheNamespace::ExtractedSource, jar, &member, bytes);
                    let path = prefix.concat(&member);
                    cache
                        .publish(&key, bytes)
                        .map_err(|error| format!("cache publish failed: {error:?}"))?;
                    out.artifacts.push(LibrarySource { path, key });
                    Ok(())
                },
                &mut out.warnings,
            );
        }
        out
    }
}

impl JarExtraction<CachedJar> {
    /// Recursively extract nested jars, deepest first, with a bounded recursion depth.
    pub fn nested<C: CacheBackend>(cache: &mut ArtifactCache<C>, jar: &CacheKey) -> Self {
        let mut out = Self::default();
        out.extract_nested(cache, jar, 0);
        out
    }

    fn extract_nested<C: CacheBackend>(
        &mut self,
        cache: &mut ArtifactCache<C>,
        jar: &CacheKey,
        depth: usize,
    ) {
        if depth >= MAX_NESTED_JAR_DEPTH {
            self.warn(jar, "nested jar recursion too deep; not unpacking further");
            return;
        }
        let bytes = match cache.lookup(jar) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return self.warn(jar, "nested jar parent is not cached"),
            Err(error) => {
                return self.warn(jar, &format!("nested jar parent is invalid: {error:?}"));
            }
        };
        let mut level = Vec::new();
        Archive::extract_members(
            &bytes,
            jar,
            "jar",
            |member, bytes| {
                let key = Archive::member_key(CacheNamespace::NestedJar, jar, &member, bytes);
                cache
                    .publish(&key, bytes)
                    .map_err(|error| format!("cache publish failed: {error:?}"))?;
                level.push(CachedJar { member, key });
                Ok(())
            },
            &mut self.warnings,
        );
        for nested in level {
            self.extract_nested(cache, &nested.key, depth + 1);
            self.artifacts.push(nested);
        }
    }
}

impl<T> JarExtraction<T> {
    fn warn(&mut self, key: &CacheKey, message: &str) {
        self.warnings
            .push(Warning::new(WarningOrigin::Artifact(key.clone()), message));
    }
}

struct Archive;

impl Archive {
    /// Walk `bytes` as a zip archive, feeding every regular member whose name passes `matches`
    /// through `accept`. An unreadable archive/entry and a rejected member are diagnosed into
    /// `warnings` under `origin`; nothing aborts the walk.
    fn walk_members(
        bytes: &[u8],
        origin: &WarningOrigin,
        matches: impl Fn(&str) -> bool,
        mut accept: impl FnMut(&str, &[u8]) -> Result<(), String>,
        warnings: &mut Vec<Warning>,
    ) {
        let mut archive = match zip::ZipArchive::new(Cursor::new(bytes)) {
            Ok(archive) => archive,
            Err(error) => {
                warnings.push(Warning::new(
                    origin.clone(),
                    format!("failed to read archive: {error}"),
                ));
                return;
            }
        };
        for index in 0..archive.len() {
            let mut member = match archive.by_index(index) {
                Ok(member) => member,
                Err(error) => {
                    warnings.push(Warning::new(
                        origin.clone(),
                        format!("failed to read archive entry {index}: {error}"),
                    ));
                    continue;
                }
            };
            if member.is_dir() || !matches(member.name()) {
                continue;
            }
            let mut contents = Vec::with_capacity(usize::try_from(member.size()).unwrap_or(0));
            if let Err(error) = member.read_to_end(&mut contents) {
                warnings.push(Warning::new(
                    origin.clone(),
                    format!("failed to read archive member `{}`: {error}", member.name()),
                ));
                continue;
            }
            if let Err(message) = accept(member.name(), &contents) {
                warnings.push(Warning::new(origin.clone(), message));
            }
        }
    }

    /// [`walk_members`](Self::walk_members) restricted to members with `wanted_extension` whose
    /// names lower to safe relative paths (unsafe ones are diagnosed and skipped, never
    /// partially published).
    fn extract_members(
        bytes: &[u8],
        jar: &CacheKey,
        wanted_extension: &str,
        mut accept: impl FnMut(RelativePath, &[u8]) -> Result<(), String>,
        warnings: &mut Vec<Warning>,
    ) {
        Self::walk_members(
            bytes,
            &WarningOrigin::Artifact(jar.clone()),
            |name| Self::extension(name) == Some(wanted_extension),
            |name, contents| {
                let path = Self::safe_relative(name)
                    .ok_or_else(|| format!("skipped unsafe archive member `{name}`"))?;
                accept(path, contents)
            },
            warnings,
        );
    }

    fn member_key(
        namespace: CacheNamespace,
        parent: &CacheKey,
        member: &RelativePath,
        bytes: &[u8],
    ) -> CacheKey {
        let mut provenance = Vec::new();
        provenance.extend_from_slice(parent.provenance().as_bytes());
        provenance.extend_from_slice(parent.content().as_bytes());
        provenance.extend_from_slice(member.to_string().as_bytes());
        DependencyResolver::cache_key(namespace, b"archive-member\0", &provenance, bytes)
    }

    fn extension(value: &str) -> Option<&str> {
        let name = value.rsplit('/').next().unwrap_or(value);
        match name.rfind('.') {
            Some(0) | None => None,
            Some(index) => Some(&name[index + 1..]),
        }
    }

    fn safe_relative(value: &str) -> Option<RelativePath> {
        if value.starts_with('/') || value.starts_with('\\') {
            return None;
        }
        let mut normalized = String::new();
        for component in value.split('/') {
            match component {
                "" | "." => {}
                ".." => return None,
                component => {
                    if !normalized.is_empty() {
                        normalized.push('/');
                    }
                    normalized.push_str(component);
                }
            }
        }
        if normalized.is_empty() {
            None
        } else {
            RelativePath::parse(&normalized).ok()
        }
    }
}
