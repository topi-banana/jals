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

use crate::Warning;
use crate::resolve::DepsCache;

/// The recursion-depth cap for [`JarExtraction::extract_nested_jars_in`]. A jar cannot contain itself,
/// so genuine nesting is shallow (a fat jar's `BOOT-INF/lib/*.jar` is one level); this only guards
/// against a pathological or adversarial archive, well above any real layering.
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
    /// Load every `.class` file reachable from `entries` (a project's resolved classpath, as
    /// `/`-separated virtual paths), reading through `fs`.
    ///
    /// Each entry is classified by what it is on the tree:
    /// - a **directory** is walked recursively for `*.class` files;
    /// - a **jar** (`.jar`/`.zip`) has its `*.class` members read;
    /// - a bare **`.class`** file is read directly;
    /// - anything else (a missing path, an unrecognized file) becomes a [`Warning`].
    pub fn load_classpath_in(fs: &dyn FileTree, entries: &[String]) -> Self {
        let mut load = Self::default();
        for entry in entries {
            load.load_entry(fs, entry);
        }
        load
    }

    fn warn(&mut self, path: &str, message: &str) {
        self.warnings.push(Warning::new(path, message));
    }

    /// Classify one classpath entry by what it is on the tree and load it.
    fn load_entry(&mut self, fs: &dyn FileTree, entry: &str) {
        if fs.is_dir(entry) {
            self.load_dir(fs, entry);
        } else if fs.is_file(entry) {
            match path::VPath::extension(entry) {
                Some(e) if e.eq_ignore_ascii_case("class") => self.load_class_file(fs, entry),
                Some(e) if e.eq_ignore_ascii_case("jar") || e.eq_ignore_ascii_case("zip") => {
                    self.load_jar(fs, entry);
                }
                _ => self.warn(
                    entry,
                    "unrecognized classpath entry (expected a directory, a `.jar`, or a `.class` file)",
                ),
            }
        } else {
            self.warn(entry, "classpath entry does not exist");
        }
    }

    /// Walk a directory of `.class` files (an exploded classpath, e.g. a `javac -d` output dir).
    fn load_dir(&mut self, fs: &dyn FileTree, dir: &str) {
        match fs.walk_ext(dir, "class") {
            Ok(paths) => {
                for path in paths {
                    self.load_class_file(fs, &path);
                }
            }
            Err(err) => self.warn(dir, &format!("failed to walk directory: {err}")),
        }
    }

    /// Read every `*.class` member of a jar (a zip archive), unzipped from an in-memory [`Cursor`].
    fn load_jar(&mut self, fs: &dyn FileTree, jar: &str) {
        let bytes = match fs.read(jar) {
            Ok(b) => b,
            Err(err) => return self.warn(jar, &format!("failed to open jar: {err}")),
        };
        let mut archive = match zip::ZipArchive::new(Cursor::new(bytes)) {
            Ok(a) => a,
            Err(err) => return self.warn(jar, &format!("failed to read jar: {err}")),
        };
        for i in 0..archive.len() {
            let mut member = match archive.by_index(i) {
                Ok(m) => m,
                Err(err) => {
                    self.warn(jar, &format!("failed to read jar entry {i}: {err}"));
                    continue;
                }
            };
            if member.is_dir()
                || !path::VPath::extension(member.name())
                    .is_some_and(|e| e.eq_ignore_ascii_case("class"))
            {
                continue;
            }
            // `joined` names the failing member within the jar, e.g. `dep.jar/java/util/List.class`.
            let joined = path::VPath::join(jar, member.name());
            let mut buf = Vec::with_capacity(usize::try_from(member.size()).unwrap_or(0));
            if let Err(err) = member.read_to_end(&mut buf) {
                self.warn(&joined, &format!("failed to read class from jar: {err}"));
                continue;
            }
            self.parse_into(&joined, &buf);
        }
    }

    /// Read and parse a single `.class` file.
    fn load_class_file(&mut self, fs: &dyn FileTree, path: &str) {
        match fs.read(path) {
            Ok(bytes) => self.parse_into(path, &bytes),
            Err(err) => self.warn(path, &format!("failed to read class file: {err}")),
        }
    }

    /// Parse `bytes` as a class file, pushing the result or a warning attributed to `path`.
    fn parse_into(&mut self, path: &str, bytes: &[u8]) {
        match jals_classfile::ClassFile::read(bytes) {
            Ok(cf) => self.classes.push(cf),
            Err(err) => self.warn(path, &format!("failed to parse class file: {err}")),
        }
    }
}

/// The non-fatal [`Warning`]s accumulated while extracting jar members onto the tree.
///
/// The shared warning sink behind [`extract_sources_in`](JarExtraction::extract_sources_in)
/// (`.java` members) and [`extract_nested_jars_in`](JarExtraction::extract_nested_jars_in)
/// (bundled `.jar`s); the extracted member paths flow back through each method's return value.
#[derive(Default)]
pub struct JarExtraction {
    /// One per jar or member that could not be read/extracted.
    warnings: Vec<Warning>,
}

impl JarExtraction {
    /// Extract every `*.java` member of each sources jar in `jars` into `dest_dir`, returning the paths
    /// of the `.java` files written to the tree, plus any non-fatal [`Warning`]s.
    ///
    /// Each jar's members are placed under `dest_dir/<jar-stem>-<hash>/<entry-path>`. Idempotent
    /// (skip-if-exists) and error-resilient — an unreadable jar/member becomes a [`Warning`] and is
    /// skipped.
    pub fn extract_sources_in(
        fs: &mut dyn FileTree,
        jars: &[String],
        dest_dir: &str,
    ) -> (Vec<String>, Vec<Warning>) {
        let mut extraction = Self::default();
        let mut extracted = Vec::new();
        for jar in jars {
            let into = path::VPath::join(dest_dir, &Self::subdir(jar));
            extracted.extend(extraction.extract_members(fs, jar, &into, ".java", "sources jar"));
        }
        (extracted, extraction.warnings)
    }

    /// Recursively extract every **bundled jar** (`*.jar` member, at any depth) of `jar` into
    /// `dest_dir`.
    ///
    /// Returns the nested jar paths written to the tree (for the host to add to the classpath), plus
    /// any non-fatal [`Warning`]s.
    ///
    /// This is what `recursive = true` on a `[dependencies]` jar opts into. Idempotent (skip-if-exists),
    /// zip-slip sanitized, depth-bounded, and error-resilient.
    pub fn extract_nested_jars_in(
        fs: &mut dyn FileTree,
        jar: &str,
        dest_dir: &str,
    ) -> (Vec<String>, Vec<Warning>) {
        let mut extraction = Self::default();
        let extracted = extraction.extract_nested_into(fs, jar, dest_dir, 0);
        (extracted, extraction.warnings)
    }

    /// One level of [`extract_nested_jars_in`](Self::extract_nested_jars_in): write `jar`'s `*.jar`
    /// members into their dedicated subdir of `dest_dir`, then recurse into each extracted jar
    /// (`depth`-bounded). Returns the extracted jar paths, each nested jar preceded by its own
    /// (deeper) nested jars — a deepest-first order.
    fn extract_nested_into(
        &mut self,
        fs: &mut dyn FileTree,
        jar: &str,
        dest_dir: &str,
        depth: usize,
    ) -> Vec<String> {
        if depth >= MAX_NESTED_JAR_DEPTH {
            self.warnings.push(Warning::new(
                jar,
                "nested jar recursion too deep; not unpacking further",
            ));
            return Vec::new();
        }
        // Write out this level's nested jars first, then recurse into them — the extracted jars are
        // independent files on the tree, so the parent archive need not stay open while descending.
        let into = path::VPath::join(dest_dir, &Self::subdir(jar));
        let level = self.extract_members(fs, jar, &into, ".jar", "jar");
        let mut extracted = Vec::new();
        for nested in level {
            extracted.extend(self.extract_nested_into(fs, &nested, dest_dir, depth + 1));
            extracted.push(nested);
        }
        extracted
    }

    /// Extract every member of `jar` whose name ends with `ext` into `into`, returning each written-
    /// or-reused path and pushing each non-fatal problem to [`warnings`](Self::warnings) (worded with
    /// `noun`). The shared member-walk spine of
    /// [`extract_sources_in`](Self::extract_sources_in) (`.java`) and
    /// [`extract_nested_jars_in`](Self::extract_nested_jars_in) (bundled `.jar`s): read the jar into
    /// memory, unzip from a [`Cursor`], skip directories and non-`ext` members, sanitize each member
    /// path against zip-slip, and [`write`](FileTree::write) it — unless it is already on the tree
    /// (skip-if-exists). A jar/member that cannot be read becomes a [`Warning`]; extraction never
    /// aborts.
    fn extract_members(
        &mut self,
        fs: &mut dyn FileTree,
        jar: &str,
        into: &str,
        ext: &str,
        noun: &str,
    ) -> Vec<String> {
        let mut extracted = Vec::new();
        let bytes = match fs.read(jar) {
            Ok(b) => b,
            Err(err) => {
                self.warnings
                    .push(Warning::new(jar, &format!("failed to open {noun}: {err}")));
                return extracted;
            }
        };
        let mut archive = match zip::ZipArchive::new(Cursor::new(bytes)) {
            Ok(a) => a,
            Err(err) => {
                self.warnings
                    .push(Warning::new(jar, &format!("failed to read {noun}: {err}")));
                return extracted;
            }
        };
        for i in 0..archive.len() {
            let mut member = match archive.by_index(i) {
                Ok(m) => m,
                Err(err) => {
                    self.warnings.push(Warning::new(
                        jar,
                        &format!("failed to read {noun} entry {i}: {err}"),
                    ));
                    continue;
                }
            };
            if member.is_dir() || !member.name().ends_with(ext) {
                continue;
            }
            let Some(rel) = Self::safe_relative(member.name()) else {
                let joined = path::VPath::join(jar, member.name());
                self.warnings.push(Warning::new(
                    &joined,
                    &format!("skipped {noun} member with an unsafe path"),
                ));
                continue;
            };
            let dest = path::VPath::join(into, &rel);
            // Already extracted (immutable for a fixed jar): reuse without re-reading the member.
            if fs.is_file(&dest) {
                extracted.push(dest);
                continue;
            }
            let mut buf = Vec::with_capacity(usize::try_from(member.size()).unwrap_or(0));
            if let Err(err) = member.read_to_end(&mut buf) {
                let joined = path::VPath::join(jar, member.name());
                self.warnings.push(Warning::new(
                    &joined,
                    &format!("failed to read {noun} member: {err}"),
                ));
                continue;
            }
            match fs.write(&dest, &buf) {
                Ok(()) => extracted.push(dest),
                Err(err) => {
                    let joined = path::VPath::join(jar, member.name());
                    self.warnings
                        .push(Warning::new(&joined, &format!("writing {dest}: {err}")));
                }
            }
        }
        extracted
    }

    /// The per-jar extraction subdir name: `<file-stem>-<hash of the jar's virtual path>`. Hashing the
    /// jar path disambiguates two jars that share a filename.
    fn subdir(jar: &str) -> String {
        let name = path::VPath::file_name(jar).unwrap_or("jar");
        let stem = match name.rfind('.') {
            Some(0) | None => name, // no extension (or a dotfile)
            Some(idx) => &name[..idx],
        };
        format!("{stem}-{}", DepsCache::hash_hex(jar))
    }

    /// Sanitize a zip member name into a safe `/`-relative virtual path that stays inside the
    /// extraction dir: split on `/`, drop empty / `.` components, and reject `..` or an absolute path
    /// (a zip-slip attempt). `None` for an empty or unsafe name.
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
}
