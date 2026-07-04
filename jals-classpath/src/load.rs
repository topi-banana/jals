//! Classpath loading and jar extraction, routed through a [`jals_fs::FileTree`].
//!
//! This is the pure, `wasm32`-compatible half of `jals-classpath`: it turns the classpath *entries*
//! a project lists (jars and directories of `.class` files) into parsed [`ClassFile`]s, and inflates
//! a jar's `.java` / bundled-`.jar` members onto the file tree — all through the [`FileTree`]
//! abstraction, so it runs identically against an [`OsFileTree`](jals_fs::OsFileTree) on a host and
//! an [`InMemoryFileTree`](jals_fs::InMemoryFileTree) in the browser. jars are read into memory and
//! unzipped from a [`Cursor`] (no `std::fs::File`), directories are walked via
//! [`FileTree::walk_ext`], and writes go through [`FileTree::write`] (atomic, so "the file exists"
//! implies "complete" — the skip-if-exists cache test is a plain [`FileTree::is_file`]).
//!
//! Loading is **error-resilient**: an unreadable jar, a corrupt `.class`, or a missing entry is
//! recorded as a [`Warning`] and skipped, never aborting.

use std::io::{Cursor, Read};

use jals_classfile::ClassFile;
use jals_fs::{FileTree, path};

use crate::resolve::deps_cache_dir;
use crate::skeleton::skeleton_groups;
use crate::{Warning, hash_hex};

/// The recursion-depth cap for [`extract_nested_jars_in`]. A jar cannot contain itself, so genuine
/// nesting is shallow (a fat jar's `BOOT-INF/lib/*.jar` is one level); this only guards against a
/// pathological or adversarial archive, well above any real layering.
const MAX_NESTED_JAR_DEPTH: usize = 64;

/// The outcome of loading a classpath: every `.class` file that parsed, plus any non-fatal
/// [`Warning`]s for entries that could not be read.
#[derive(Debug, Default)]
pub struct ClasspathLoad {
    /// The parsed class files, ready to hand to `ProjectIndexBuilder::with_classpath`. The order
    /// follows the classpath entries (and, within a directory/jar, the tree/archive order).
    pub classes: Vec<ClassFile>,
    /// One per entry or member file that could not be read or parsed. Loading continues regardless.
    pub warnings: Vec<Warning>,
}

impl ClasspathLoad {
    fn warn(&mut self, path: &str, message: &str) {
        self.warnings.push(Warning::new(path, message));
    }
}

/// Load every `.class` file reachable from `entries` (a project's resolved classpath, as `/`-separated
/// virtual paths), reading through `fs`.
///
/// Each entry is classified by what it is on the tree:
/// - a **directory** is walked recursively for `*.class` files;
/// - a **jar** (`.jar`/`.zip`) has its `*.class` members read;
/// - a bare **`.class`** file is read directly;
/// - anything else (a missing path, an unrecognized file) becomes a [`Warning`].
pub fn load_classpath_in(fs: &dyn FileTree, entries: &[String]) -> ClasspathLoad {
    let mut load = ClasspathLoad::default();
    for entry in entries {
        load_entry(fs, entry, &mut load);
    }
    load
}

fn load_entry(fs: &dyn FileTree, entry: &str, load: &mut ClasspathLoad) {
    if fs.is_dir(entry) {
        load_dir(fs, entry, load);
    } else if fs.is_file(entry) {
        match path::extension(entry) {
            Some(e) if e.eq_ignore_ascii_case("class") => load_class_file(fs, entry, load),
            Some(e) if e.eq_ignore_ascii_case("jar") || e.eq_ignore_ascii_case("zip") => {
                load_jar(fs, entry, load)
            }
            _ => load.warn(
                entry,
                "unrecognized classpath entry (expected a directory, a `.jar`, or a `.class` file)",
            ),
        }
    } else {
        load.warn(entry, "classpath entry does not exist");
    }
}

/// Walk a directory of `.class` files (an exploded classpath, e.g. a `javac -d` output dir).
fn load_dir(fs: &dyn FileTree, dir: &str, load: &mut ClasspathLoad) {
    match fs.walk_ext(dir, "class") {
        Ok(paths) => {
            for path in paths {
                load_class_file(fs, &path, load);
            }
        }
        Err(err) => load.warn(dir, &format!("failed to walk directory: {err}")),
    }
}

/// Read every `*.class` member of a jar (a zip archive), unzipped from an in-memory [`Cursor`].
fn load_jar(fs: &dyn FileTree, jar: &str, load: &mut ClasspathLoad) {
    let bytes = match fs.read(jar) {
        Ok(b) => b,
        Err(err) => return load.warn(jar, &format!("failed to open jar: {err}")),
    };
    let mut archive = match zip::ZipArchive::new(Cursor::new(bytes)) {
        Ok(a) => a,
        Err(err) => return load.warn(jar, &format!("failed to read jar: {err}")),
    };
    for i in 0..archive.len() {
        let mut member = match archive.by_index(i) {
            Ok(m) => m,
            Err(err) => {
                load.warn(jar, &format!("failed to read jar entry {i}: {err}"));
                continue;
            }
        };
        if member.is_dir() || !member.name().ends_with(".class") {
            continue;
        }
        // `joined` names the failing member within the jar, e.g. `dep.jar/java/util/List.class`.
        let joined = path::join(jar, member.name());
        let mut buf = Vec::with_capacity(member.size() as usize);
        if let Err(err) = member.read_to_end(&mut buf) {
            load.warn(&joined, &format!("failed to read class from jar: {err}"));
            continue;
        }
        parse_into(&joined, &buf, load);
    }
}

/// Read and parse a single `.class` file.
fn load_class_file(fs: &dyn FileTree, path: &str, load: &mut ClasspathLoad) {
    match fs.read(path) {
        Ok(bytes) => parse_into(path, &bytes, load),
        Err(err) => load.warn(path, &format!("failed to read class file: {err}")),
    }
}

/// Parse `bytes` as a class file, pushing the result or a warning attributed to `path`.
fn parse_into(path: &str, bytes: &[u8], load: &mut ClasspathLoad) {
    match jals_classfile::read(bytes) {
        Ok(cf) => load.classes.push(cf),
        Err(err) => load.warn(path, &format!("failed to parse class file: {err}")),
    }
}

/// Extract every `*.java` member of each sources jar in `jars` into `dest_dir`, returning the paths of
/// the `.java` files written to the tree, plus any non-fatal [`Warning`]s.
///
/// Each jar's members are placed under `dest_dir/<jar-stem>-<hash>/<entry-path>`. Idempotent
/// (skip-if-exists) and error-resilient — an unreadable jar/member becomes a [`Warning`] and is
/// skipped.
pub fn extract_sources_in(
    fs: &mut dyn FileTree,
    jars: &[String],
    dest_dir: &str,
) -> (Vec<String>, Vec<Warning>) {
    let mut java_files = Vec::new();
    let mut warnings = Vec::new();
    for jar in jars {
        let into = path::join(dest_dir, &jar_subdir(jar));
        extract_members(
            fs,
            jar,
            &into,
            ".java",
            "sources jar",
            &mut warnings,
            &mut java_files,
        );
    }
    (java_files, warnings)
}

/// Recursively extract every **bundled jar** (`*.jar` member, at any depth) of `jar` into `dest_dir`,
/// returning the nested jar paths written to the tree (for the host to add to the classpath), plus any
/// non-fatal [`Warning`]s.
///
/// This is what `recursive = true` on a `[dependencies]` jar opts into. Idempotent (skip-if-exists),
/// zip-slip sanitized, depth-bounded, and error-resilient.
pub fn extract_nested_jars_in(
    fs: &mut dyn FileTree,
    jar: &str,
    dest_dir: &str,
) -> (Vec<String>, Vec<Warning>) {
    let mut jars = Vec::new();
    let mut warnings = Vec::new();
    extract_nested_into(fs, jar, dest_dir, 0, &mut jars, &mut warnings);
    (jars, warnings)
}

/// One level of [`extract_nested_jars_in`]: write `jar`'s `*.jar` members into their dedicated subdir
/// of `dest_dir`, then recurse into each extracted jar (`depth`-bounded).
fn extract_nested_into(
    fs: &mut dyn FileTree,
    jar: &str,
    dest_dir: &str,
    depth: usize,
    jars: &mut Vec<String>,
    warnings: &mut Vec<Warning>,
) {
    if depth >= MAX_NESTED_JAR_DEPTH {
        warnings.push(Warning::new(
            jar,
            "nested jar recursion too deep; not unpacking further",
        ));
        return;
    }
    // Write out this level's nested jars first, then recurse into them — the extracted jars are
    // independent files on the tree, so the parent archive need not stay open while descending.
    let into = path::join(dest_dir, &jar_subdir(jar));
    let mut extracted = Vec::new();
    extract_members(fs, jar, &into, ".jar", "jar", warnings, &mut extracted);
    for nested in extracted {
        extract_nested_into(fs, &nested, dest_dir, depth + 1, jars, warnings);
        jars.push(nested);
    }
}

/// Extract every member of `jar` whose name ends with `ext` into `into`, appending each written-or-
/// reused path to `extracted` and each non-fatal problem to `warnings` (worded with `noun`). The
/// shared member-walk spine of [`extract_sources_in`] (`.java`) and [`extract_nested_jars_in`]
/// (bundled `.jar`s): read the jar into memory, unzip from a [`Cursor`], skip directories and
/// non-`ext` members, sanitize each member path against zip-slip, and [`write`](FileTree::write) it —
/// unless it is already on the tree (skip-if-exists). A jar/member that cannot be read becomes a
/// [`Warning`]; extraction never aborts.
fn extract_members(
    fs: &mut dyn FileTree,
    jar: &str,
    into: &str,
    ext: &str,
    noun: &str,
    warnings: &mut Vec<Warning>,
    extracted: &mut Vec<String>,
) {
    let bytes = match fs.read(jar) {
        Ok(b) => b,
        Err(err) => {
            warnings.push(Warning::new(jar, &format!("failed to open {noun}: {err}")));
            return;
        }
    };
    let mut archive = match zip::ZipArchive::new(Cursor::new(bytes)) {
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
            let joined = path::join(jar, member.name());
            warnings.push(Warning::new(
                &joined,
                &format!("skipped {noun} member with an unsafe path"),
            ));
            continue;
        };
        let dest = path::join(into, &rel);
        // Already extracted (immutable for a fixed jar): reuse without re-reading the member.
        if fs.is_file(&dest) {
            extracted.push(dest);
            continue;
        }
        let mut buf = Vec::with_capacity(member.size() as usize);
        if let Err(err) = member.read_to_end(&mut buf) {
            let joined = path::join(jar, member.name());
            warnings.push(Warning::new(
                &joined,
                &format!("failed to read {noun} member: {err}"),
            ));
            continue;
        }
        match fs.write(&dest, &buf) {
            Ok(()) => extracted.push(dest),
            Err(err) => {
                let joined = path::join(jar, member.name());
                warnings.push(Warning::new(&joined, &format!("writing {dest}: {err}")));
            }
        }
    }
}

/// Synthesize signature-only `.java` **skeletons** for `classes` (already-parsed classpath class
/// files) and write them under `<root>/target/jals/deps/decompiled`, returning the written `.java`
/// paths for the host to register as go-to-definition targets.
///
/// Takes the project `root` and derives the cache subdir itself, mirroring the sibling
/// `resolve_project_*_in` orchestrators (so the cache layout lives in one place — the core, next to
/// [`deps_cache_dir`]).
///
/// One `.java` per top-level type (nested types inlined), carrying every member's signature. Idempotent
/// (skip-if-exists) and error-resilient — a class that fails to write is reported through `warn` and
/// skipped. Pure rendering (via `skeleton.rs`) plus tree writes; no network.
pub fn synthesize_classpath_sources_in(
    fs: &mut dyn FileTree,
    classes: &[ClassFile],
    root: &str,
    mut warn: impl FnMut(String),
) -> Vec<String> {
    let dest_dir = path::join(&deps_cache_dir(root), "decompiled");
    let mut out = Vec::new();
    for group in skeleton_groups(classes) {
        let rel = group.rel_path();
        let dest = path::join(&dest_dir, &rel);
        // Already synthesized (a class file does not change under us): reuse the cached file.
        if fs.is_file(&dest) {
            out.push(dest);
            continue;
        }
        match fs.write(&dest, group.render().as_bytes()) {
            Ok(()) => out.push(dest),
            Err(err) => warn(format!("{dest}: {err}")),
        }
    }
    out
}

/// The per-jar extraction subdir name: `<file-stem>-<hash of the jar's virtual path>`. Hashing the jar
/// path disambiguates two jars that share a filename.
fn jar_subdir(jar: &str) -> String {
    let name = path::file_name(jar).unwrap_or("jar");
    let stem = match name.rfind('.') {
        Some(0) | None => name, // no extension (or a dotfile)
        Some(idx) => &name[..idx],
    };
    format!("{stem}-{}", hash_hex(jar))
}

/// Sanitize a zip member name into a safe `/`-relative virtual path that stays inside the extraction
/// dir: split on `/`, drop empty / `.` components, and reject `..` or an absolute path (a zip-slip
/// attempt). `None` for an empty or unsafe name.
fn safe_relative(name: &str) -> Option<String> {
    if name.starts_with('/') {
        return None; // absolute — would escape the extraction dir
    }
    let mut parts: Vec<&str> = Vec::new();
    for comp in name.split('/') {
        match comp {
            "" | "." => {}
            ".." => return None,
            normal => parts.push(normal),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}
