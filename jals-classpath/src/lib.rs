//! Host-side classpath loading: turn the classpath *entries* a `jals.toml` lists (jars and
//! directories of `.class` files, resolved by [`jals_build::Manifest::classpath_entries`]) into the
//! parsed [`ClassFile`]s that `jals-hir`'s classpath bridge consumes
//! ([`ProjectIndex::build_with_classpath`]).
//!
//! This is the missing connective tissue between [`jals_classfile`] (a pure `.class` codec) and
//! `jals-hir`: the bridge in `jals-hir` is pure and `wasm32`-compatible, so it takes
//! already-parsed class files and never touches the filesystem. Reading those bytes — walking a
//! classes directory, unzipping a jar — is exactly the host I/O that belongs here, in a host-only
//! crate (like `jals-cli`/`jals-lsp`), not in the pure analysis layers.
//!
//! Loading is **error-resilient**: an unreadable jar, a corrupt `.class`, or an entry that does not
//! exist is recorded as a [`Warning`] and skipped, never aborting the load — a project should still
//! get analysis from the dependencies that *did* load. The caller decides whether to surface the
//! warnings.
//!
//! [`ProjectIndex::build_with_classpath`]: https://docs.rs/jals-hir

mod resolve;
mod skeleton;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use jals_classfile::ClassFile;
use walkdir::WalkDir;

pub use resolve::{
    ResolvedDependencies, cached_jar_path, resolve_dependencies, resolve_project_dependencies,
    resolve_project_source_deps, resolve_project_sources, synthesize_classpath_sources,
};

/// The outcome of loading a classpath: every `.class` file that parsed, plus any non-fatal
/// [`Warning`]s for entries that could not be read.
#[derive(Debug, Default)]
pub struct ClasspathLoad {
    /// The parsed class files, ready to hand to `ProjectIndex::build_with_classpath`. The order
    /// follows the classpath entries (and, within a directory/jar, the filesystem/archive order).
    pub classes: Vec<ClassFile>,
    /// One per entry or member file that could not be read or parsed. Loading continues regardless.
    pub warnings: Vec<Warning>,
}

/// A single classpath entry or member file that could not be loaded. Advisory only — the rest of
/// the classpath still loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// The path that failed: a classpath entry, or a specific `.class` file inside a directory/jar.
    pub path: PathBuf,
    /// A human-readable reason, suitable for a CLI/LSP diagnostic.
    pub message: String,
}

impl Warning {
    /// Build a [`Warning`] for `path` with `message`, owning both. The single construction site
    /// shared by the load (`lib.rs`) and resolve (`resolve.rs`) halves of this crate.
    pub(crate) fn new(path: &Path, message: &str) -> Warning {
        Warning {
            path: path.to_path_buf(),
            message: message.to_owned(),
        }
    }
}

/// Load every `.class` file reachable from `entries` (a project's resolved classpath).
///
/// Each entry is classified by what it is on disk:
/// - a **directory** is walked recursively for `*.class` files;
/// - a **jar** (`.jar`/`.zip`, a zip archive) has its `*.class` members read;
/// - a bare **`.class`** file is read directly;
/// - anything else (a missing path, an unrecognized file) becomes a [`Warning`].
///
/// The host owns all the I/O; the returned [`ClassFile`]s are self-contained, so the caller can feed
/// them straight into `jals-hir` and drop everything else.
pub fn load_classpath(entries: &[PathBuf]) -> ClasspathLoad {
    let mut load = ClasspathLoad::default();
    for entry in entries {
        load_entry(entry, &mut load);
    }
    load
}

fn load_entry(entry: &Path, load: &mut ClasspathLoad) {
    if entry.is_dir() {
        load_dir(entry, load);
    } else if entry.is_file() {
        if has_ext(entry, "class") {
            load_class_file(entry, load);
        } else if has_ext(entry, "jar") || has_ext(entry, "zip") {
            load_jar(entry, load);
        } else {
            load.warn(
                entry,
                "unrecognized classpath entry (expected a directory, a `.jar`, or a `.class` file)",
            );
        }
    } else {
        load.warn(entry, "classpath entry does not exist");
    }
}

/// Walk a directory of `.class` files (an exploded classpath, e.g. a `javac -d` output dir).
fn load_dir(dir: &Path, load: &mut ClasspathLoad) {
    for entry in WalkDir::new(dir).sort_by_file_name().into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                load.warn(dir, &format!("failed to walk directory: {err}"));
                continue;
            }
        };
        let path = entry.path();
        if entry.file_type().is_file() && has_ext(path, "class") {
            load_class_file(path, load);
        }
    }
}

/// Read every `*.class` member of a jar (a zip archive).
fn load_jar(path: &Path, load: &mut ClasspathLoad) {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(err) => return load.warn(path, &format!("failed to open jar: {err}")),
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(err) => return load.warn(path, &format!("failed to read jar: {err}")),
    };
    for i in 0..archive.len() {
        let mut member = match archive.by_index(i) {
            Ok(m) => m,
            Err(err) => {
                load.warn(path, &format!("failed to read jar entry {i}: {err}"));
                continue;
            }
        };
        if member.is_dir() || !member.name().ends_with(".class") {
            continue;
        }
        // `joined` names the failing member within the jar, e.g. `dep.jar!java/util/List.class`.
        let joined = path.join(member.name());
        let mut bytes = Vec::with_capacity(member.size() as usize);
        if let Err(err) = member.read_to_end(&mut bytes) {
            load.warn(&joined, &format!("failed to read class from jar: {err}"));
            continue;
        }
        parse_into(&joined, &bytes, load);
    }
}

/// Read and parse a single `.class` file on disk.
fn load_class_file(path: &Path, load: &mut ClasspathLoad) {
    match std::fs::read(path) {
        Ok(bytes) => parse_into(path, &bytes, load),
        Err(err) => load.warn(path, &format!("failed to read class file: {err}")),
    }
}

/// Parse `bytes` as a class file, pushing the result or a warning attributed to `path`.
fn parse_into(path: &Path, bytes: &[u8], load: &mut ClasspathLoad) {
    match jals_classfile::read(bytes) {
        Ok(cf) => load.classes.push(cf),
        Err(err) => load.warn(path, &format!("failed to parse class file: {err}")),
    }
}

/// Whether `path` has the given (case-insensitive) extension.
fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

impl ClasspathLoad {
    fn warn(&mut self, path: &Path, message: &str) {
        self.warnings.push(Warning::new(path, message));
    }
}

/// The outcome of extracting dependency **sources** jars: the `.java` files written to disk, plus any
/// non-fatal [`Warning`]s for jars/members that could not be read.
#[derive(Debug, Default)]
pub struct SourcesExtraction {
    /// The extracted `.java` file paths on disk, in jar/archive order. The host registers these as
    /// (read-only) navigation files so go-to-definition can land in a library's real source.
    pub java_files: Vec<PathBuf>,
    /// One per jar or member that could not be read/extracted. Extraction continues regardless.
    pub warnings: Vec<Warning>,
}

/// Extract every `*.java` member of each sources jar in `jars` into `dest_dir`, returning the paths of
/// the `.java` files written to disk.
///
/// Each jar's members are placed under `dest_dir/<jar-stem>-<hash>/<entry-path>` (e.g.
/// `.../sources/foo-sources-0badc0de/java/util/List.java`). The `<hash>` of the jar's own path keeps
/// two jars that share a filename from colliding. Member paths are sanitized — an entry that would
/// escape `dest_dir` (an absolute path or a `..` component, a zip-slip attempt) is skipped.
///
/// Idempotent and cheap to re-run: a member already present on disk (non-empty) is left untouched (a
/// fixed dependency's sources jar does not change), so re-opening a project does not re-inflate. Like
/// [`load_classpath`], it is **error-resilient**: an unreadable jar/member becomes a [`Warning`] and is
/// skipped, never aborting.
pub fn extract_sources(jars: &[PathBuf], dest_dir: &Path) -> SourcesExtraction {
    let mut out = SourcesExtraction::default();
    for jar in jars {
        let into = dest_dir.join(jar_subdir(jar));
        extract_members(
            jar,
            &into,
            ".java",
            "sources jar",
            &mut out.warnings,
            &mut out.java_files,
        );
    }
    out
}

/// Extract every member of `jar` whose name ends with `ext` into `into` (its dedicated subdir of the
/// extraction root), appending each written-or-reused path to `extracted` and each non-fatal problem to
/// `warnings` (worded with `noun`, e.g. `"sources jar"` / `"jar"`). The shared member-walk spine of
/// [`extract_sources`] (`.java`) and [`extract_nested_jars`] (bundled `.jar`s): open the zip, skip
/// directories and non-`ext` members, sanitize each member path against zip-slip, and write it via
/// [`write_atomic`] — unless an identical non-empty file is already on disk (skip-if-exists). A
/// jar/member that cannot be read becomes a [`Warning`]; extraction never aborts.
fn extract_members(
    jar: &Path,
    into: &Path,
    ext: &str,
    noun: &str,
    warnings: &mut Vec<Warning>,
    extracted: &mut Vec<PathBuf>,
) {
    let file = match std::fs::File::open(jar) {
        Ok(f) => f,
        Err(err) => {
            warnings.push(Warning::new(jar, &format!("failed to open {noun}: {err}")));
            return;
        }
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(err) => {
            warnings.push(Warning::new(jar, &format!("failed to read {noun}: {err}")));
            return;
        }
    };
    for i in 0..archive.len() {
        let mut member = match archive.by_index(i) {
            Ok(m) => m,
            Err(err) => {
                warnings.push(Warning::new(
                    jar,
                    &format!("failed to read {noun} entry {i}: {err}"),
                ));
                continue;
            }
        };
        if member.is_dir() || !member.name().ends_with(ext) {
            continue;
        }
        let Some(rel) = safe_relative(member.name()) else {
            // `jar.join(member.name())` names the failing member, e.g. `dep.jar!java/util/List.java`.
            let joined = jar.join(member.name());
            warnings.push(Warning::new(
                &joined,
                &format!("skipped {noun} member with an unsafe path"),
            ));
            continue;
        };
        let dest = into.join(rel);
        // Already extracted (immutable for a fixed jar): reuse without re-reading the member.
        if is_nonempty_file(&dest) {
            extracted.push(dest);
            continue;
        }
        let mut bytes = Vec::with_capacity(member.size() as usize);
        if let Err(err) = member.read_to_end(&mut bytes) {
            let joined = jar.join(member.name());
            warnings.push(Warning::new(
                &joined,
                &format!("failed to read {noun} member: {err}"),
            ));
            continue;
        }
        match write_atomic(&dest, &bytes) {
            Ok(()) => extracted.push(dest),
            Err(message) => {
                let joined = jar.join(member.name());
                warnings.push(Warning::new(&joined, &message));
            }
        }
    }
}

/// The outcome of recursively unpacking a jar's **bundled jars**: the nested `*.jar` files written to
/// disk (at every depth), plus any non-fatal [`Warning`]s for jars/members that could not be read.
#[derive(Debug, Default)]
pub struct NestedJarsExtraction {
    /// The extracted nested jar paths on disk. The host appends these to the classpath so the bundled
    /// libraries' `.class` files load — `recursive = true` on a `[dependencies]` jar opts into this.
    pub jars: Vec<PathBuf>,
    /// One per jar or member that could not be read/extracted. Extraction continues regardless.
    pub warnings: Vec<Warning>,
}

/// The recursion-depth cap for [`extract_nested_jars`]. A jar cannot contain itself, so genuine nesting
/// is shallow (a fat jar's `BOOT-INF/lib/*.jar` is one level); this only guards against a pathological
/// or adversarial archive, well above any real layering.
const MAX_NESTED_JAR_DEPTH: usize = 64;

/// Recursively extract every **bundled jar** (`*.jar` member, at any depth) of `jar` into `dest_dir`,
/// returning the paths of the nested jars written to disk for the host to add to the classpath.
///
/// This is what `recursive = true` on a `[dependencies]` jar opts into: a fat jar (e.g. a Spring-Boot
/// layout with `BOOT-INF/lib/*.jar`) bundles its dependencies as nested jars that [`load_classpath`]
/// would otherwise skip (it reads a jar's own `.class` members only). Each nested jar is written under
/// `dest_dir/<jar-stem>-<hash>/<entry-path>` (the `<hash>` of the parent jar's path keeps two jars that
/// share a filename from colliding), then itself scanned for further nested jars, so a jar-in-jar-in-jar
/// resolves too. Member paths are sanitized — an entry that would escape `dest_dir` (a zip-slip) is
/// skipped.
///
/// Idempotent and cheap to re-run (skip-if-exists, like [`extract_sources`]) and **error-resilient**, the
/// same as [`load_classpath`]: an unreadable jar/member becomes a [`Warning`] and is skipped, never
/// aborting.
pub fn extract_nested_jars(jar: &Path, dest_dir: &Path) -> NestedJarsExtraction {
    let mut out = NestedJarsExtraction::default();
    extract_nested_into(jar, dest_dir, 0, &mut out);
    out
}

/// One level of [`extract_nested_jars`]: write `jar`'s `*.jar` members into their dedicated subdir of
/// `dest_dir`, then recurse into each extracted jar (`depth`-bounded by [`MAX_NESTED_JAR_DEPTH`]).
fn extract_nested_into(jar: &Path, dest_dir: &Path, depth: usize, out: &mut NestedJarsExtraction) {
    if depth >= MAX_NESTED_JAR_DEPTH {
        return out.warn(jar, "nested jar recursion too deep; not unpacking further");
    }
    // Write out this level's nested jars first, then recurse into them — descending while the parent
    // archive is still open would tangle the borrow; the extracted jars are independent files.
    let into = dest_dir.join(jar_subdir(jar));
    let mut extracted = Vec::new();
    extract_members(jar, &into, ".jar", "jar", &mut out.warnings, &mut extracted);
    for nested in extracted {
        extract_nested_into(&nested, dest_dir, depth + 1, out);
        out.jars.push(nested);
    }
}

/// Whether `path` is an existing, non-empty file — the skip-if-exists cache-hit test shared by the jar
/// downloader (`resolve`) and the member extractor ([`extract_members`]): a fixed dependency's
/// already-written file is reused untouched, so re-opening a project does no redundant download/inflate.
pub(crate) fn is_nonempty_file(path: &Path) -> bool {
    path.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

/// A 16-hex-digit [`DefaultHasher`] digest of `value`, used to disambiguate cache filenames / subdirs
/// (e.g. two URLs or jar paths that share a name). [`DefaultHasher`] is fixed-keyed, so the digest is
/// stable across runs — only disambiguation matters here, not collision resistance.
pub(crate) fn hash_hex(value: impl Hash) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The per-jar extraction subdir name: `<file-stem>-<hash of the jar's path>`. Hashing the jar path
/// disambiguates two jars that share a filename.
fn jar_subdir(jar: &Path) -> String {
    let stem = jar
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "jar".to_string());
    format!("{stem}-{}", hash_hex(jar.to_string_lossy()))
}

/// Sanitize a zip member name into a relative path that stays inside the extraction dir: keep only
/// `Normal` components, drop `.`, and reject anything else (an absolute path or a `..`, which could
/// escape — a zip-slip). `None` for an empty or unsafe path.
fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for comp in Path::new(name).components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Write `bytes` to `dest`, creating parents, via a `.part` sibling renamed into place so an
/// interrupted write never leaves a truncated file a later run would mistake for a complete extraction.
/// The temp file keeps `dest`'s own extension (e.g. `List.java` → `List.java.part`, `inner.jar` →
/// `inner.jar.part`) so two extractions targeting the same dir never collide on the temp name.
fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating dir {}: {e}", parent.display()))?;
    }
    let mut tmp = dest.as_os_str().to_os_string();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dest).map_err(|e| format!("finalizing {}: {e}", dest.display()))?;
    Ok(())
}

impl NestedJarsExtraction {
    fn warn(&mut self, path: &Path, message: &str) {
        self.warnings.push(Warning::new(path, message));
    }
}
