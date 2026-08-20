//! Archive adapter and classpath byte loading.
//!
//! Archive decoding is isolated in this module (over the in-house [`crate::zip`] reader): it
//! consumes portable readers from a project revision or artifact cache and never opens a host
//! path. Jars stream from their backing source ([`ArtifactCache::open_verified`] readers, or
//! in-memory cursors over project bytes) and `.class` members parse straight from the
//! decompressing member stream.
//!
//! Loading is a three-phase pipeline: a serial planning walk emits ordered [`DecodeTask`]s, the
//! tasks fan out over [`Exec::fan_out`] (jar members in fixed-size chunks, so the split never
//! depends on worker count), and a serial fold merges results in input order. The output is
//! therefore byte-identical to a sequential walk on any executor.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use jals_classfile::ClassFile;
use jals_decompile::ClassHierarchy;
use jals_exec::{Exec, LocalBoxFuture};
use jals_storage::io::{self as sio, Buffered, IoError, SeekFrom};
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, DirKey, FileKey, Name,
    ProjectView, ProvenanceFold, RelativePath,
};

use crate::skeleton::{SkeletonGroup, SkeletonMode};
use crate::zip::{CentralDirectory, MemberStream};
use crate::{LibrarySource, Warning, WarningOrigin};

/// What a jar-backing reader must satisfy: portable `Read + Seek` feeds the member streams, and
/// `Clone + Send + 'static` lets a fan-out chunk carry its own reader clone to a worker, every
/// clone reading at an independent position.
pub(crate) trait JarReader: sio::Read + sio::Seek + Clone + Send + 'static {}
impl<R: sio::Read + sio::Seek + Clone + Send + 'static> JarReader for R {}

const MAX_NESTED_JAR_DEPTH: usize = 64;

/// Members per fan-out decode chunk. Deliberately a fixed constant — never derived from the
/// worker count — so the task split (and therefore every diagnostic and result order) is
/// identical at any parallelism.
const JAR_CHUNK_MEMBERS: usize = 64;

/// The two places jar bytes live: an in-memory cursor over project bytes, or a verified cache
/// reader. Unifying them keeps the decode task type single-parameter.
#[derive(Clone)]
enum JarSource<R> {
    Memory(sio::Cursor<Arc<[u8]>>),
    Cache(R),
}

impl<R: sio::Read> sio::Read for JarSource<R> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        match self {
            Self::Memory(cursor) => cursor.read(buf).await,
            Self::Cache(reader) => reader.read(buf).await,
        }
    }
}

impl<R: sio::Seek> sio::Seek for JarSource<R> {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError> {
        match self {
            Self::Memory(cursor) => cursor.seek(pos).await,
            Self::Cache(reader) => reader.seek(pos).await,
        }
    }
}

/// One unit of fan-out decode work, planned serially in entry order. Everything inside is
/// `Send` plain data or a cloneable reader; the decode future itself is built on the worker.
enum DecodeTask<R> {
    /// A diagnostic discovered during planning, kept in position so the fold preserves the
    /// serial walk's warning order.
    Warn(Warning),
    /// A single resident `.class` (project file or directory member).
    ClassBytes {
        origin: WarningOrigin,
        bytes: Arc<[u8]>,
    },
    /// A single cached `.class` artifact, streamed from its verified reader.
    ClassReader { origin: WarningOrigin, reader: R },
    /// A fixed-size range of one jar's central-directory members.
    JarChunk {
        origin: WarningOrigin,
        reader: JarSource<R>,
        directory: Arc<CentralDirectory>,
        members: Range<usize>,
    },
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
    /// Entries are planned serially in order, decoded on `exec`'s fan-out (jar members in
    /// fixed-size chunks), and folded back in input order — the result is byte-identical to a
    /// sequential walk on any executor.
    pub async fn load<C: CacheBackend>(
        exec: &Exec,
        view: &ProjectView,
        cache: &ArtifactCache<C>,
        entries: &[ClasspathEntry],
    ) -> Self {
        let mut tasks: Vec<DecodeTask<C::Reader>> = Vec::new();
        for entry in entries {
            Self::plan_entry(view, cache, entry, &mut tasks).await;
        }
        let outcomes = exec.fan_out(tasks, archive::decode_task).await;
        let mut load = Self::default();
        for (classes, warnings) in outcomes {
            load.classes.extend(classes);
            load.warnings.extend(warnings);
        }
        load
    }

    /// Phase A: turn one classpath entry into ordered decode tasks.
    async fn plan_entry<C: CacheBackend>(
        view: &ProjectView,
        cache: &ArtifactCache<C>,
        entry: &ClasspathEntry,
        tasks: &mut Vec<DecodeTask<C::Reader>>,
    ) {
        match entry {
            ClasspathEntry::ProjectFile(key) => Self::plan_project_file(view, key, tasks).await,
            ClasspathEntry::ProjectDirectory(key) => Self::plan_project_dir(view, key, tasks),
            ClasspathEntry::Artifact(key) => {
                let origin = WarningOrigin::Artifact(key.clone());
                match cache.open_verified(key).await {
                    Ok(Some(reader)) => {
                        Self::plan_jar(origin, JarSource::Cache(reader), tasks).await;
                    }
                    Ok(None) => tasks.push(DecodeTask::Warn(Warning::new(
                        origin,
                        "classpath artifact is not cached",
                    ))),
                    Err(error) => tasks.push(DecodeTask::Warn(Warning::new(
                        origin,
                        format!("classpath artifact is invalid: {error:?}"),
                    ))),
                }
            }
            ClasspathEntry::ArtifactFile { path, key } => {
                let origin = WarningOrigin::Artifact(key.clone());
                match cache.open_verified(key).await {
                    Ok(Some(reader)) => {
                        Self::plan_cached_file::<C>(path, origin, reader, tasks).await;
                    }
                    Ok(None) => tasks.push(DecodeTask::Warn(Warning::new(
                        origin,
                        "classpath file artifact is not cached",
                    ))),
                    Err(error) => tasks.push(DecodeTask::Warn(Warning::new(
                        origin,
                        format!("classpath file artifact is invalid: {error:?}"),
                    ))),
                }
            }
        }
    }

    async fn plan_project_file<R: JarReader>(
        view: &ProjectView,
        key: &FileKey,
        tasks: &mut Vec<DecodeTask<R>>,
    ) {
        let origin = WarningOrigin::ProjectFile(key.clone());
        let file = match view.file(key) {
            Ok(file) => file,
            Err(error) => {
                tasks.push(DecodeTask::Warn(Warning::new(
                    origin,
                    format!("classpath file cannot be read: {error}"),
                )));
                return;
            }
        };
        // `CodeFile` exposes only `&[u8]`, so shipping bytes to workers takes one copy into a
        // shared `Arc<[u8]>`; jar chunks then clone the `Arc`, not the bytes.
        let bytes: Arc<[u8]> = Arc::from(file.bytes());
        match key.extension() {
            Some(ext) if ext.eq_ignore_ascii_case("class") => {
                tasks.push(DecodeTask::ClassBytes { origin, bytes });
            }
            Some(ext) if ext.eq_ignore_ascii_case("jar") || ext.eq_ignore_ascii_case("zip") => {
                Self::plan_jar(origin, JarSource::Memory(sio::Cursor::new(bytes)), tasks).await;
            }
            _ => tasks.push(DecodeTask::Warn(Warning::new(
                origin,
                "unrecognized classpath file (expected `.class`, `.jar`, or `.zip`)",
            ))),
        }
    }

    fn plan_project_dir<R: JarReader>(
        view: &ProjectView,
        key: &DirKey,
        tasks: &mut Vec<DecodeTask<R>>,
    ) {
        if let Err(error) = view.directory(key) {
            tasks.push(DecodeTask::Warn(Warning::new(
                WarningOrigin::ProjectDirectory(key.clone()),
                format!("classpath directory cannot be read: {error}"),
            )));
            return;
        }
        for file in view
            .tree()
            .files_under(key)
            .filter(|file| file.key().has_extension("class"))
        {
            tasks.push(DecodeTask::ClassBytes {
                origin: WarningOrigin::ProjectFile(file.key().clone()),
                bytes: Arc::from(file.bytes()),
            });
        }
    }

    async fn plan_cached_file<C: CacheBackend>(
        path: &RelativePath,
        origin: WarningOrigin,
        reader: C::Reader,
        tasks: &mut Vec<DecodeTask<C::Reader>>,
    ) {
        match path.name().and_then(|name| name.as_str().rsplit_once('.')) {
            Some((_, ext)) if ext.eq_ignore_ascii_case("class") => {
                tasks.push(DecodeTask::ClassReader { origin, reader });
            }
            Some((_, ext))
                if ext.eq_ignore_ascii_case("jar") || ext.eq_ignore_ascii_case("zip") =>
            {
                Self::plan_jar(origin, JarSource::Cache(reader), tasks).await;
            }
            _ => tasks.push(DecodeTask::Warn(Warning::new(
                origin,
                "unrecognized cached classpath file (expected `.class`, `.jar`, or `.zip`)",
            ))),
        }
    }

    /// Parse the jar's central directory and emit one chunk task per fixed-size member range.
    async fn plan_jar<R: JarReader>(
        origin: WarningOrigin,
        reader: JarSource<R>,
        tasks: &mut Vec<DecodeTask<R>>,
    ) {
        match archive::open(reader).await {
            Ok((reader, directory)) => {
                for members in archive::chunk_ranges(directory.members.len()) {
                    tasks.push(DecodeTask::JarChunk {
                        origin: origin.clone(),
                        reader: reader.clone(),
                        directory: Arc::clone(&directory),
                        members,
                    });
                }
            }
            Err(message) => tasks.push(DecodeTask::Warn(Warning::new(origin, message))),
        }
    }
}

/// Which of a set of package prefixes a classpath actually defines a class under.
///
/// This answers a question [`ClasspathLoad`] is too expensive to be asked. A dependency's
/// `navigation` publications are routed away from the compiler *because* the classpath is expected
/// to define the same types, and a compile build never loads the classpath at all, so nothing on
/// that path can check the expectation.
///
/// No member is ever decompressed. An archive is answered from its central directory alone, because
/// a member's name already *is* its binary name, and a classpath directory's member path is too.
/// The only class files parsed are loose `.class`, whose package cannot be read off their path.
/// That bounds the *parsing*, not the reading — see [`add_cached_artifact`](Self::add_cached_artifact)
/// for what reaching an entry's bytes costs.
///
/// Entries fold in one at a time because a caller assembling a classpath rarely holds it in one
/// shape: some are bytes it already captured, and some are keys in a verified cache.
#[derive(Debug)]
pub struct ClasspathCoverage {
    wanted: BTreeSet<RelativePath>,
    covered: BTreeSet<RelativePath>,
    warnings: Vec<Warning>,
}

/// What a classpath entry's bytes turned out to be, chosen by extension.
enum EntryKind {
    Class,
    Archive,
}

impl ClasspathCoverage {
    /// Seek `prefixes`. An empty set is already [complete](Self::is_complete), so a caller with
    /// nothing to ask about should not build one.
    pub fn seeking(prefixes: impl IntoIterator<Item = RelativePath>) -> Self {
        Self {
            wanted: prefixes.into_iter().collect(),
            covered: BTreeSet::new(),
            warnings: Vec::new(),
        }
    }

    /// Whether every sought prefix is covered, at which point no further entry can change the
    /// answer and folding one in is pure cost.
    pub fn is_complete(&self) -> bool {
        self.covered.len() == self.wanted.len()
    }

    /// Whether anything folded in so far defines a class under `prefix`. Always `false` for a
    /// prefix that was never sought: this distinguishes covered from uncovered, not from unasked.
    pub fn covers(&self, prefix: &RelativePath) -> bool {
        self.covered.contains(prefix)
    }

    /// Entries that could not be inspected.
    ///
    /// An unreadable entry is not an entry that defines nothing, so a caller concluding anything
    /// from an *uncovered* prefix has to weigh this first. Answering the question does not retract
    /// what went unread on the way to it — the scan stopping early only means nothing was added
    /// after the answer arrived, never that something already reported has been taken back.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    /// Fold in one class already addressed by its binary name — an archive member, or a file under
    /// a classpath directory. A no-op once the answer is settled.
    pub fn add_class(&mut self, binary_path: &RelativePath) {
        if self.is_complete() {
            return;
        }
        for prefix in &self.wanted {
            if !self.covered.contains(prefix) && binary_path.starts_with(prefix) {
                self.covered.insert(prefix.clone());
            }
        }
    }

    /// Fold in one entry whose bytes the caller already holds — a host file a graph node captured,
    /// or a build script's registered classpath output.
    ///
    /// `path` decides how the bytes are read, so it must keep the entry's extension; it is never
    /// used as the class's package. Borrowed, not owned: only the central directory is wanted and
    /// nothing outlives the call, so a caller with the bytes in hand pays no copy to ask.
    pub async fn add_resident(&mut self, origin: WarningOrigin, path: &RelativePath, bytes: &[u8]) {
        if self.is_complete() {
            return;
        }
        match Self::kind(path) {
            Some(EntryKind::Class) => match archive::read_class(bytes).await {
                Ok(class) => self.add_parsed_class(&class),
                Err(message) => self.warnings.push(Warning::new(origin, message)),
            },
            Some(EntryKind::Archive) => {
                self.add_archive(origin, sio::Cursor::new(bytes)).await;
            }
            None => self.warnings.push(Warning::new(
                origin,
                "unrecognized classpath file (expected `.class`, `.jar`, or `.zip`)",
            )),
        }
    }

    /// Fold in one archive published into the verified cache, such as a build task's JAR.
    ///
    /// The expensive kind, and the reason the guard above it is worth keeping at the call site
    /// rather than only here: [`ArtifactCache::open_verified`] digests the whole artifact before it
    /// hands out a reader, so reading a jar's central directory this way is cheap only relative to
    /// decoding its members, never a couple of seeks. Fold entries in an order that settles the
    /// answer early and stop on [`is_complete`](Self::is_complete).
    pub async fn add_cached_artifact<C: CacheBackend>(
        &mut self,
        cache: &ArtifactCache<C>,
        key: &CacheKey,
    ) {
        if self.is_complete() {
            return;
        }
        let origin = WarningOrigin::Artifact(key.clone());
        match cache.open_verified(key).await {
            Ok(Some(reader)) => self.add_archive(origin, reader).await,
            Ok(None) => self
                .warnings
                .push(Warning::new(origin, "classpath artifact is not cached")),
            Err(error) => self.warnings.push(Warning::new(
                origin,
                format!("classpath artifact is invalid: {error:?}"),
            )),
        }
    }

    async fn add_archive<R: sio::Read + sio::Seek>(&mut self, origin: WarningOrigin, reader: R) {
        let directory = match archive::open(reader).await {
            Ok((_, directory)) => directory,
            Err(message) => {
                self.warnings.push(Warning::new(origin, message));
                return;
            }
        };
        // An archive member's name is its binary name, so the central directory alone already
        // spells every package this jar defines. A game jar has tens of thousands of them and the
        // answer is usually settled by the first few, so this stops mid-directory rather than
        // checking once per entry and walking the rest for nothing.
        let mut yielder = jals_exec::Yielder::new();
        for member in &directory.members {
            if self.is_complete() {
                return;
            }
            yielder.tick().await;
            if member.is_dir
                || !archive::extension(&member.name)
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("class"))
            {
                continue;
            }
            if let Ok(path) = RelativePath::parse(&member.name) {
                self.add_class(&path);
            }
        }
    }

    /// A loose `.class` sits wherever its holder put it, so its package comes from the constant
    /// pool rather than from the path the file happens to have.
    fn add_parsed_class(&mut self, class: &ClassFile) {
        let Some(internal) = class.constant_pool.class_name(class.this_class) else {
            return;
        };
        if let Ok(path) = RelativePath::parse(internal.as_ref()) {
            self.add_class(&path);
        }
    }

    fn kind(path: &RelativePath) -> Option<EntryKind> {
        let extension = archive::extension(path.name()?.as_str())?;
        if extension.eq_ignore_ascii_case("class") {
            Some(EntryKind::Class)
        } else if extension.eq_ignore_ascii_case("jar") || extension.eq_ignore_ascii_case("zip") {
            Some(EntryKind::Archive)
        } else {
            None
        }
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

/// A deterministic tree of source artifacts whose paths are relative to a requested archive prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceTree {
    pub files: Vec<LibrarySource>,
}

/// Decompressed source-tree resource limits checked from the central directory before decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceTreeLimits {
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
}

/// Strict source-tree extraction for build tasks.
pub mod source_tree_extraction {
    use super::{
        ArtifactCache, BTreeMap, CacheBackend, CacheKey, CacheNamespace, ClassFile, ClassHierarchy,
        Exec, LibrarySource, RelativePath, SkeletonGroup, SkeletonMode, SourceTree,
        SourceTreeLimits, String, ToOwned, Vec, archive, format,
    };

    /// Extract every `.java` member below `prefix`, stripping that prefix from result paths.
    /// Any unsafe, duplicate, corrupt, or unpublishable matching member fails the whole operation.
    pub async fn java<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        jar: &CacheKey,
        prefix: &RelativePath,
        limits: SourceTreeLimits,
    ) -> Result<SourceTree, String> {
        let reader = cache
            .open_verified(jar)
            .await
            .map_err(|error| format!("source jar is invalid: {error:?}"))?
            .ok_or_else(|| "source jar is not cached".to_owned())?;
        let members = archive::decode_matching_bounded(exec, reader, "java", limits).await?;
        let prefix_len = prefix.segments().len();
        let mut files = BTreeMap::new();
        for (name, outcome) in members {
            let member = archive::safe_relative(&name)
                .ok_or_else(|| format!("unsafe Java archive member `{name}`"))?;
            if !member.starts_with(prefix) {
                continue;
            }
            let relative = RelativePath::new(member.segments().skip(prefix_len).cloned());
            if relative.is_root() {
                return Err(format!(
                    "Java archive member `{name}` has no relative file name"
                ));
            }
            let bytes = outcome?;
            if files.insert(relative, (member, bytes)).is_some() {
                return Err(format!("duplicate Java archive member `{name}`"));
            }
        }
        let mut tree = SourceTree::default();
        for (path, (member, bytes)) in files {
            let key = archive::member_key(CacheNamespace::BuildTaskSource, jar, &member, &bytes);
            cache
                .publish(&key, &bytes)
                .await
                .map_err(|error| format!("source member `{member}` publish failed: {error:?}"))?;
            tree.files.push(LibrarySource { path, key });
        }
        Ok(tree)
    }

    /// Decompile every `.class` member of `jar` whose internal binary name sits under `prefix` into compile-safe skeleton `.java` sources, stripping that prefix from result paths.
    ///
    /// Any render/parse/publish failure fails the whole operation.
    pub async fn decompile<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        jar: &CacheKey,
        prefix: &RelativePath,
        limits: SourceTreeLimits,
    ) -> Result<SourceTree, String> {
        let reader = cache
            .open_verified(jar)
            .await
            .map_err(|error| format!("decompile jar is invalid: {error:?}"))?
            .ok_or_else(|| "decompile jar is not cached".to_owned())?;
        let members = archive::decode_matching_bounded(exec, reader, "class", limits).await?;

        let mut classes = Vec::new();
        for (name, outcome) in members {
            let bytes = outcome
                .map_err(|error| format!("failed to read archive member `{name}`: {error}"))?;
            let cf = ClassFile::read(bytes.as_slice())
                .await
                .map_err(|error| format!("failed to parse archive member `{name}`: {error}"))?;
            // Filter by internal binary name against the requested prefix.
            let Some(internal) = cf.constant_pool.class_name(cf.this_class) else {
                continue;
            };
            let Ok(internal_path) = RelativePath::parse(internal.as_ref()) else {
                continue;
            };
            if !prefix.is_root() && !internal_path.starts_with(prefix) {
                continue;
            }
            classes.push(cf);
        }

        let prefix_len = prefix.segments().len();
        let mut files = BTreeMap::new();
        let hierarchy = ClassHierarchy::new(&classes);
        let mut yielder = jals_exec::Yielder::every(1);
        for group in SkeletonGroup::groups(&classes, SkeletonMode::Compile) {
            yielder.tick().await;
            let rel = group.rel_path();
            let full = RelativePath::parse(&rel)
                .map_err(|_| format!("generated source path is not portable: {rel}"))?;
            if !prefix.is_root() && !full.starts_with(prefix) {
                continue;
            }
            let relative = if prefix.is_root() {
                full.clone()
            } else {
                RelativePath::new(full.segments().skip(prefix_len).cloned())
            };
            if relative.is_root() {
                return Err(format!(
                    "decompiled source `{rel}` has no relative file name under the requested prefix"
                ));
            }
            let bytes = group.render(&hierarchy).await.into_bytes();
            if bytes.len() > limits.max_file_bytes {
                return Err(format!(
                    "decompiled source `{rel}` has {} bytes, exceeding the limit of {}",
                    bytes.len(),
                    limits.max_file_bytes
                ));
            }
            if files.insert(relative, (full, bytes)).is_some() {
                return Err(format!("duplicate decompiled source `{rel}`"));
            }
        }
        if files.len() > limits.max_files {
            return Err(format!(
                "decompiled tree has {} files, exceeding the limit of {}",
                files.len(),
                limits.max_files
            ));
        }
        let total: usize = files.values().map(|(_, b)| b.len()).sum();
        if total > limits.max_total_bytes {
            return Err(format!(
                "decompiled tree has {total} bytes, exceeding the limit of {}",
                limits.max_total_bytes
            ));
        }

        let mut tree = SourceTree::default();
        for (path, (member, bytes)) in files {
            let key = archive::member_key(CacheNamespace::BuildTaskSource, jar, &member, &bytes);
            cache.publish(&key, &bytes).await.map_err(|error| {
                format!("decompiled source `{member}` publish failed: {error:?}")
            })?;
            tree.files.push(LibrarySource { path, key });
        }
        Ok(tree)
    }
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
    /// Extract `.java` members from verified jar artifacts: members decode on the fan-out, then
    /// a serial loop publishes them in member order.
    pub async fn sources<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        jars: &[CacheKey],
    ) -> Self {
        let mut out = Self::default();
        for jar in jars {
            // The reader owns its backing resource, so the cache stays free for the publishes
            // below.
            let reader = match cache.open_verified(jar).await {
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
            let members = match archive::decode_matching(exec, reader, "java").await {
                Ok(members) => members,
                Err(message) => {
                    out.warn(jar, &message);
                    continue;
                }
            };
            for (name, outcome) in members {
                match archive::publish_member(
                    cache,
                    CacheNamespace::ExtractedSource,
                    jar,
                    &name,
                    outcome,
                )
                .await
                {
                    Ok((member, key)) => out.artifacts.push(LibrarySource {
                        path: prefix.concat(&member),
                        key,
                    }),
                    Err(message) => out.warn(jar, &message),
                }
            }
        }
        out
    }
}

impl JarExtraction<CachedJar> {
    /// Recursively extract nested jars, deepest first, with a bounded recursion depth.
    pub async fn nested<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        jar: &CacheKey,
    ) -> Self {
        let mut out = Self::default();
        out.extract_nested(exec, cache, jar, 0).await;
        out
    }

    fn extract_nested<'a, C: CacheBackend>(
        &'a mut self,
        exec: &'a Exec,
        cache: &'a mut ArtifactCache<C>,
        jar: &'a CacheKey,
        depth: usize,
    ) -> LocalBoxFuture<'a, ()> {
        Box::pin(async move {
            if depth >= MAX_NESTED_JAR_DEPTH {
                self.warn(jar, "nested jar recursion too deep; not unpacking further");
                return;
            }
            let reader = match cache.open_verified(jar).await {
                Ok(Some(reader)) => reader,
                Ok(None) => return self.warn(jar, "nested jar parent is not cached"),
                Err(error) => {
                    return self.warn(jar, &format!("nested jar parent is invalid: {error:?}"));
                }
            };
            let members = match archive::decode_matching(exec, reader, "jar").await {
                Ok(members) => members,
                Err(message) => return self.warn(jar, &message),
            };
            let mut level = Vec::new();
            for (name, outcome) in members {
                match archive::publish_member(cache, CacheNamespace::NestedJar, jar, &name, outcome)
                    .await
                {
                    Ok((member, key)) => level.push(CachedJar { member, key }),
                    Err(message) => self.warn(jar, &message),
                }
            }
            for nested in level {
                self.extract_nested(exec, cache, &nested.key, depth + 1)
                    .await;
                self.artifacts.push(nested);
            }
        })
    }
}

impl<T> JarExtraction<T> {
    fn warn(&mut self, key: &CacheKey, message: &str) {
        self.warnings
            .push(Warning::new(WarningOrigin::Artifact(key.clone()), message));
    }
}

pub(crate) mod archive {
    use super::{
        Arc, ArtifactCache, Buffered, CacheBackend, CacheKey, CacheNamespace, CentralDirectory,
        ClassFile, ContentDigest, DecodeTask, Exec, JAR_CHUNK_MEMBERS, JarReader, JarSource,
        MemberStream, ProvenanceFold, Range, RelativePath, SourceTreeLimits, String, ToOwned,
        ToString, Vec, Warning, WarningOrigin, format, sio, vec,
    };

    /// The fixed-size chunk split of `0..len`.
    pub(crate) fn chunk_ranges(len: usize) -> impl Iterator<Item = Range<usize>> {
        (0..len)
            .step_by(JAR_CHUNK_MEMBERS)
            .map(move |start| start..(start + JAR_CHUNK_MEMBERS).min(len))
    }

    /// Phase B: decode one planned task on a fan-out worker.
    pub(super) async fn decode_task<R: JarReader>(
        task: DecodeTask<R>,
    ) -> (Vec<ClassFile>, Vec<Warning>) {
        match task {
            DecodeTask::Warn(warning) => (Vec::new(), vec![warning]),
            DecodeTask::ClassBytes { origin, bytes } => decode_class(origin, &*bytes).await,
            DecodeTask::ClassReader { origin, reader } => {
                decode_class(origin, Buffered::new(reader)).await
            }
            DecodeTask::JarChunk {
                origin,
                reader,
                directory,
                members,
            } => decode_jar_chunk(&origin, reader, &directory, members).await,
        }
    }

    async fn decode_class<R: sio::Read>(
        origin: WarningOrigin,
        source: R,
    ) -> (Vec<ClassFile>, Vec<Warning>) {
        match read_class(source).await {
            Ok(class) => (vec![class], Vec::new()),
            Err(message) => (Vec::new(), vec![Warning::new(origin, message)]),
        }
    }

    async fn decode_jar_chunk<R: JarReader>(
        origin: &WarningOrigin,
        reader: JarSource<R>,
        directory: &CentralDirectory,
        members: Range<usize>,
    ) -> (Vec<ClassFile>, Vec<Warning>) {
        let mut classes = Vec::new();
        let mut warnings = Vec::new();
        for member in &directory.members[members] {
            if member.is_dir
                || !extension(&member.name).is_some_and(|ext| ext.eq_ignore_ascii_case("class"))
            {
                continue;
            }
            let stream = match MemberStream::open(reader.clone(), member).await {
                Ok(stream) => stream,
                Err(message) => {
                    warnings.push(Warning::new(origin.clone(), message));
                    continue;
                }
            };
            match ClassFile::read(Buffered::new(stream)).await {
                Ok(class) => classes.push(class),
                Err(error) => warnings.push(Warning::new(
                    origin.clone(),
                    format!("failed to parse archive member `{}`: {error}", member.name),
                )),
            }
        }
        (classes, warnings)
    }

    /// Parse a class file from any portable byte source, tagging a failure with the shared
    /// diagnostic message.
    pub(crate) async fn read_class<R: sio::Read>(source: R) -> Result<ClassFile, String> {
        ClassFile::read(source)
            .await
            .map_err(|error| format!("failed to parse class file: {error}"))
    }

    /// Open a portable reader as a zip archive. The parsed central directory is plain data
    /// shared behind an `Arc`, so fan-out chunks clone one directory and one reader handle and
    /// only the reader position is per-clone state.
    ///
    /// Deliberately weaker than [`JarReader`]: reading a directory needs no clone and outlives
    /// nothing, so a caller that only asks *what* an archive contains may hand over a borrowed
    /// cursor instead of copying the bytes into an `Arc` to satisfy `'static`. Decoding members is
    /// what needs the stronger bound, and every caller that goes on to do it already has it.
    pub(crate) async fn open<R: sio::Read + sio::Seek>(
        mut reader: R,
    ) -> Result<(R, Arc<CentralDirectory>), String> {
        let directory = CentralDirectory::parse(&mut reader)
            .await
            .map_err(|message| format!("failed to read archive: {message}"))?;
        Ok((reader, Arc::new(directory)))
    }

    /// Fully materialize every regular member with `wanted_extension`, decoding fixed-size
    /// member chunks on the fan-out. Results arrive in member order as
    /// `(raw name, bytes or diagnostic)`; extraction targets must be whole because their cache
    /// key derives from a digest of the complete content before a write-once publish.
    pub(crate) async fn decode_matching<R: JarReader>(
        exec: &Exec,
        reader: R,
        wanted_extension: &'static str,
    ) -> Result<Vec<(String, Result<Vec<u8>, String>)>, String> {
        decode_matching_bounded(
            exec,
            reader,
            wanted_extension,
            SourceTreeLimits {
                max_files: usize::MAX,
                max_file_bytes: usize::MAX,
                max_total_bytes: usize::MAX,
            },
        )
        .await
    }

    pub(crate) async fn decode_matching_bounded<R: JarReader>(
        exec: &Exec,
        reader: R,
        wanted_extension: &'static str,
        limits: SourceTreeLimits,
    ) -> Result<Vec<(String, Result<Vec<u8>, String>)>, String> {
        decode_bounded(exec, reader, Some(wanted_extension), limits).await
    }

    /// Fully materialize every regular member (no extension filter), decoding fixed-size member
    /// chunks on the fan-out. Results arrive in member order as `(raw name, bytes or diagnostic)`;
    /// the limits are checked against the central directory before any byte is decoded.
    pub(crate) async fn decode_all_bounded<R: JarReader>(
        exec: &Exec,
        reader: R,
        limits: SourceTreeLimits,
    ) -> Result<Vec<(String, Result<Vec<u8>, String>)>, String> {
        decode_bounded(exec, reader, None, limits).await
    }

    /// Whether `member` is a regular member the extension filter selects; `None` selects every
    /// non-directory member. The bounds pass and the decode pass must agree on this exactly, or the
    /// limits would be checked against a different member set than the one decoded.
    fn member_selected(member: &crate::zip::MemberRecord, wanted_extension: Option<&str>) -> bool {
        !member.is_dir && wanted_extension.is_none_or(|ext| extension(&member.name) == Some(ext))
    }

    async fn decode_bounded<R: JarReader>(
        exec: &Exec,
        reader: R,
        wanted_extension: Option<&'static str>,
        limits: SourceTreeLimits,
    ) -> Result<Vec<(String, Result<Vec<u8>, String>)>, String> {
        let (reader, directory) = open(reader).await?;
        let mut count = 0usize;
        let mut total = 0usize;
        for member in &directory.members {
            if !member_selected(member, wanted_extension) {
                continue;
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| "archive member count overflow".to_owned())?;
            let size = usize::try_from(member.uncompressed_size())
                .map_err(|_| format!("archive member `{}` is too large", member.name))?;
            if size > limits.max_file_bytes {
                return Err(format!(
                    "archive member `{}` has {size} bytes, exceeding the limit of {}",
                    member.name, limits.max_file_bytes
                ));
            }
            total = total
                .checked_add(size)
                .ok_or_else(|| "archive output size overflow".to_owned())?;
        }
        if count > limits.max_files {
            return Err(format!(
                "archive has {count} matching members, exceeding the limit of {}",
                limits.max_files
            ));
        }
        if total > limits.max_total_bytes {
            return Err(format!(
                "archive matching members have {total} bytes, exceeding the limit of {}",
                limits.max_total_bytes
            ));
        }
        let chunks: Vec<_> = chunk_ranges(directory.members.len())
            .map(|members| (reader.clone(), Arc::clone(&directory), members))
            .collect();
        let decoded = exec
            .fan_out(chunks, move |(reader, directory, members)| async move {
                let mut out = Vec::new();
                for member in &directory.members[members] {
                    if !member_selected(member, wanted_extension) {
                        continue;
                    }
                    let outcome = read_member(reader.clone(), member).await;
                    out.push((member.name.clone(), outcome));
                }
                out
            })
            .await;
        Ok(decoded.into_iter().flatten().collect())
    }

    /// Read one member whole through its verifying stream (the crc32 check runs at EOF).
    async fn read_member<R: JarReader>(
        reader: R,
        member: &crate::zip::MemberRecord,
    ) -> Result<Vec<u8>, String> {
        let mut stream = MemberStream::open(reader, member).await?;
        let mut contents = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            match sio::Read::read(&mut stream, &mut chunk).await {
                Ok(0) => return Ok(contents),
                Ok(n) => contents.extend_from_slice(&chunk[..n]),
                Err(error) => {
                    return Err(format!(
                        "failed to read archive member `{}`: {error}",
                        member.name
                    ));
                }
            }
        }
    }

    /// Publish one decoded member under `namespace` after the archive-safety check. The error
    /// string is the warning to record when the member is skipped.
    pub(crate) async fn publish_member<C: CacheBackend>(
        cache: &mut ArtifactCache<C>,
        namespace: CacheNamespace,
        parent: &CacheKey,
        name: &str,
        outcome: Result<Vec<u8>, String>,
    ) -> Result<(RelativePath, CacheKey), String> {
        let (member, bytes) = outcome.and_then(|bytes| {
            safe_relative(name)
                .map(|path| (path, bytes))
                .ok_or_else(|| format!("skipped unsafe archive member `{name}`"))
        })?;
        let key = member_key(namespace, parent, &member, &bytes);
        cache
            .publish(&key, &bytes)
            .await
            .map_err(|error| format!("cache publish failed: {error:?}"))?;
        Ok((member, key))
    }

    pub(crate) fn member_key(
        namespace: CacheNamespace,
        parent: &CacheKey,
        member: &RelativePath,
        bytes: &[u8],
    ) -> CacheKey {
        let mut fold = ProvenanceFold::new(b"archive-member\0");
        fold.parent(parent).bytes(member.to_string().as_bytes());
        CacheKey::new(namespace, fold.finish(), ContentDigest::of(bytes))
    }

    pub(crate) fn extension(value: &str) -> Option<&str> {
        let name = value.rsplit('/').next().unwrap_or(value);
        match name.rfind('.') {
            Some(0) | None => None,
            Some(index) => Some(&name[index + 1..]),
        }
    }

    pub(crate) fn safe_relative(value: &str) -> Option<RelativePath> {
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
