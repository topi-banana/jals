//! Archive adapter and classpath byte loading.
//!
//! `zip` is isolated in this module: it consumes portable readers from a project revision or
//! artifact cache and never opens a host path. Jars stream from their backing source
//! ([`ArtifactCache::open_verified`] readers, or in-memory cursors over project bytes) and
//! `.class` members parse straight from the decompressing member stream.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use jals_classfile::ClassFile;
use jals_storage::io::{self as sio, Buffered, StdReader, ToStd};
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, DirKey, FileKey, Name, ProjectView,
    RelativePath,
};

use crate::{DependencyResolver, LibrarySource, Warning, WarningOrigin};

/// What a jar-backing reader must satisfy: portable `Read + Seek` feeds `zip` through
/// [`ToStd`], and `Clone + Send + Sync` lets the parallel walker clone one open archive per
/// rayon worker, every clone reading at an independent position.
trait JarReader: sio::Read + sio::Seek + Clone + Send + Sync {}
impl<R: sio::Read + sio::Seek + Clone + Send + Sync> JarReader for R {}

const MAX_NESTED_JAR_DEPTH: usize = 64;

/// Order-preserving execution seam: with the `parallel` feature the maps fan out on the rayon
/// pool, without it they run inline. Both yield results in input order, so loads stay
/// deterministic either way.
mod exec {
    use alloc::vec::Vec;

    #[cfg(feature = "parallel")]
    pub(super) fn map<I: Sync, T: Send>(items: &[I], f: impl Fn(&I) -> T + Send + Sync) -> Vec<T> {
        use rayon::prelude::*;
        items.par_iter().map(f).collect()
    }

    #[cfg(not(feature = "parallel"))]
    pub(super) fn map<I, T>(items: &[I], f: impl Fn(&I) -> T) -> Vec<T> {
        items.iter().map(f).collect()
    }

    /// Map over `0..len` with a scratch value cloned from `state`: one clone per worker under
    /// `parallel`, one for the whole loop inline.
    #[cfg(feature = "parallel")]
    pub(super) fn map_with<S: Clone + Sync, T: Send>(
        len: usize,
        state: &S,
        f: impl Fn(&mut S, usize) -> T + Send + Sync,
    ) -> Vec<T> {
        use rayon::prelude::*;
        (0..len)
            .into_par_iter()
            .map_init(|| state.clone(), f)
            .collect()
    }

    #[cfg(not(feature = "parallel"))]
    pub(super) fn map_with<S: Clone, T>(
        len: usize,
        state: &S,
        f: impl Fn(&mut S, usize) -> T,
    ) -> Vec<T> {
        let mut state = state.clone();
        (0..len).map(|index| f(&mut state, index)).collect()
    }
}

/// A typed classpath input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClasspathEntry {
    ProjectFile(FileKey),
    ProjectDirectory(DirKey),
    Artifact(CacheKey),
    /// A host classpath file published into the verified cache by the native adapter. Its logical
    /// path retains the extension needed to distinguish `.class` from archive bytes.
    ArtifactFile {
        path: RelativePath,
        key: CacheKey,
    },
}

/// Parsed class files plus non-fatal per-entry diagnostics.
#[derive(Debug, Default)]
pub struct ClasspathLoad {
    pub classes: Vec<ClassFile>,
    pub warnings: Vec<Warning>,
}

impl ClasspathLoad {
    /// Load from exactly one immutable project revision and a verified artifact cache.
    ///
    /// With the `parallel` feature, entries and archive members are decoded on the rayon pool;
    /// classes and warnings are still merged in entry order, so the result is identical to the
    /// inline walk.
    pub fn load<C: CacheBackend>(
        view: &ProjectView,
        cache: &ArtifactCache<C>,
        entries: &[ClasspathEntry],
    ) -> Self {
        exec::map(entries, |entry| Self::load_entry(view, cache, entry))
            .into_iter()
            .reduce(|mut load, entry_load| {
                load.classes.extend(entry_load.classes);
                load.warnings.extend(entry_load.warnings);
                load
            })
            .unwrap_or_default()
    }

    fn load_entry<C: CacheBackend>(
        view: &ProjectView,
        cache: &ArtifactCache<C>,
        entry: &ClasspathEntry,
    ) -> Self {
        let mut load = Self::default();
        match entry {
            ClasspathEntry::ProjectFile(key) => load.load_project_file(view, key),
            ClasspathEntry::ProjectDirectory(key) => load.load_project_dir(view, key),
            ClasspathEntry::Artifact(key) => match cache.open_verified(key) {
                Ok(Some(reader)) => {
                    load.load_jar_reader(&WarningOrigin::Artifact(key.clone()), reader);
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
            ClasspathEntry::ArtifactFile { path, key } => match cache.open_verified(key) {
                Ok(Some(reader)) => load.load_cached_reader(path, key, reader),
                Ok(None) => load.warn(
                    WarningOrigin::Artifact(key.clone()),
                    "classpath file artifact is not cached",
                ),
                Err(error) => load.warn(
                    WarningOrigin::Artifact(key.clone()),
                    &format!("classpath file artifact is invalid: {error:?}"),
                ),
            },
        }
        load
    }

    fn load_cached_reader<R: JarReader>(&mut self, path: &RelativePath, key: &CacheKey, reader: R) {
        let origin = WarningOrigin::Artifact(key.clone());
        match path.name().and_then(|name| name.as_str().rsplit_once('.')) {
            Some((_, ext)) if ext.eq_ignore_ascii_case("class") => {
                self.parse_into(origin, reader);
            }
            Some((_, ext))
                if ext.eq_ignore_ascii_case("jar") || ext.eq_ignore_ascii_case("zip") =>
            {
                self.load_jar_reader(&origin, reader);
            }
            _ => self.warn(
                origin,
                "unrecognized cached classpath file (expected `.class`, `.jar`, or `.zip`)",
            ),
        }
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
                // Project files are already resident (`CodeTree` snapshot), so the jar reader
                // is an in-memory cursor over the existing bytes — no copy.
                self.load_jar_reader(&origin, sio::Cursor::new(file.bytes()));
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
        let files: Vec<_> = view
            .tree()
            .files_under(key)
            .filter(|file| file.key().has_extension("class"))
            .collect();
        let parsed = exec::map(&files, |file| Self::read_class(file.bytes()));
        for (file, outcome) in files.into_iter().zip(parsed) {
            match outcome {
                Ok(class) => self.classes.push(class),
                Err(message) => {
                    self.warn(WarningOrigin::ProjectFile(file.key().clone()), &message);
                }
            }
        }
    }

    fn load_jar_reader<R: JarReader>(&mut self, origin: &WarningOrigin, reader: R) {
        let archive = match Archive::open(reader) {
            Ok(archive) => archive,
            Err(message) => return self.warn(origin.clone(), &message),
        };
        let is_class = |name: &str| {
            Archive::extension(name).is_some_and(|ext| ext.eq_ignore_ascii_case("class"))
        };
        let parsed = exec::map_with(archive.len(), &archive, |archive, index| {
            Archive::parse_class_member(archive, index, &is_class)
        });
        for outcome in parsed {
            match outcome {
                Ok(Some(class)) => self.classes.push(class),
                Ok(None) => {}
                Err(message) => self.warn(origin.clone(), &message),
            }
        }
    }

    /// Parse a class file from any portable byte source, tagging a failure with the shared
    /// diagnostic message.
    fn read_class<R: sio::Read>(source: R) -> Result<ClassFile, String> {
        ClassFile::read(source).map_err(|error| format!("failed to parse class file: {error}"))
    }

    fn parse_into<R: sio::Read>(&mut self, origin: WarningOrigin, source: R) {
        match Self::read_class(source) {
            Ok(class) => self.classes.push(class),
            Err(message) => self.warn(origin, &message),
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
            // The reader owns its backing resource, so the cache stays free for the publishes
            // inside `accept`.
            let reader = match cache.open_verified(jar) {
                Ok(Some(reader)) => reader,
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
                reader,
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
        let reader = match cache.open_verified(jar) {
            Ok(Some(reader)) => reader,
            Ok(None) => return self.warn(jar, "nested jar parent is not cached"),
            Err(error) => {
                return self.warn(jar, &format!("nested jar parent is invalid: {error:?}"));
            }
        };
        let mut level = Vec::new();
        Archive::extract_members(
            reader,
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
    /// Open a portable reader as a zip archive. The parsed central directory is shared behind
    /// the archive handle, so parallel walkers clone one open archive per worker and only the
    /// reader position is per-clone state.
    fn open<R: JarReader>(reader: R) -> Result<zip::ZipArchive<ToStd<R>>, String> {
        zip::ZipArchive::new(ToStd(reader))
            .map_err(|error| format!("failed to read archive: {error}"))
    }

    /// Parse the `.class`-shaped member at `index` straight from its decompressing stream —
    /// the member is never materialized whole. `Ok(None)` is a directory or a filtered-out
    /// name; `Err` is an unreadable or unparsable member, diagnosed with its name.
    fn parse_class_member<R: JarReader>(
        archive: &mut zip::ZipArchive<ToStd<R>>,
        index: usize,
        matches: &impl Fn(&str) -> bool,
    ) -> Result<Option<ClassFile>, String> {
        let member = archive
            .by_index(index)
            .map_err(|error| format!("failed to read archive entry {index}: {error}"))?;
        if member.is_dir() || !matches(member.name()) {
            return Ok(None);
        }
        let name = member.name().to_owned();
        ClassFile::read(Buffered::new(StdReader(member)))
            .map(Some)
            .map_err(|error| format!("failed to parse archive member `{name}`: {error}"))
    }

    /// Read the regular member at `index` whole if its name passes `matches`. Extraction
    /// targets must be fully materialized: their cache key derives from a digest of the
    /// complete content before a write-once publish. `Ok(None)` is a directory or a
    /// filtered-out name; `Err` is an unreadable entry.
    fn read_member<R: JarReader>(
        archive: &mut zip::ZipArchive<ToStd<R>>,
        index: usize,
        matches: &impl Fn(&str) -> bool,
    ) -> Result<Option<(String, Vec<u8>)>, String> {
        let mut member = archive
            .by_index(index)
            .map_err(|error| format!("failed to read archive entry {index}: {error}"))?;
        if member.is_dir() || !matches(member.name()) {
            return Ok(None);
        }
        let name = member.name().to_owned();
        let mut contents = Vec::with_capacity(usize::try_from(member.size()).unwrap_or(0));
        if let Err(error) = std::io::copy(&mut member, &mut contents) {
            return Err(format!("failed to read archive member `{name}`: {error}"));
        }
        Ok(Some((name, contents)))
    }

    /// Walk a zip archive, feeding every regular member whose name passes `matches` through
    /// `accept`. An unreadable archive/entry and a rejected member are diagnosed into
    /// `warnings` under `origin`; nothing aborts the walk.
    fn walk_members<R: JarReader>(
        reader: R,
        origin: &WarningOrigin,
        matches: impl Fn(&str) -> bool,
        mut accept: impl FnMut(&str, &[u8]) -> Result<(), String>,
        warnings: &mut Vec<Warning>,
    ) {
        let mut archive = match Self::open(reader) {
            Ok(archive) => archive,
            Err(message) => {
                warnings.push(Warning::new(origin.clone(), message));
                return;
            }
        };
        for index in 0..archive.len() {
            let result = Self::read_member(&mut archive, index, &matches).and_then(|member| {
                member.map_or(Ok(()), |(name, contents)| accept(&name, &contents))
            });
            if let Err(message) = result {
                warnings.push(Warning::new(origin.clone(), message));
            }
        }
    }

    /// [`walk_members`](Self::walk_members) restricted to members with `wanted_extension` whose
    /// names lower to safe relative paths (unsafe ones are diagnosed and skipped, never
    /// partially published).
    fn extract_members<R: JarReader>(
        reader: R,
        jar: &CacheKey,
        wanted_extension: &str,
        mut accept: impl FnMut(RelativePath, &[u8]) -> Result<(), String>,
        warnings: &mut Vec<Warning>,
    ) {
        Self::walk_members(
            reader,
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
