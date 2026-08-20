//! Detecting a native Java-formatter config on the host and migrating it into a `jalsfmt.toml`.
//!
//! `jals-fmt` owns the two halves of the work that are pure: `import` lowers a native config's
//! *text* onto jals's option surface, and `generate` renders that surface back out as TOML.
//! Neither can look at a filesystem — `jals-fmt` is a portable `no_std` crate. Finding out *which
//! file is there* is host I/O, so it lives here (`jals-fmt/DESIGN.md` §19). Bytes are still read
//! through a `jals-storage` `ProjectView` rather than `std::fs`; only the directory listing that
//! decides which paths to snapshot uses the host filesystem directly.
//!
//! # The ladder
//!
//! Detection follows `DESIGN.md` appendix A.1, judging by **content** rather than by file name
//! (an exported Eclipse profile and an exported IntelliJ scheme can both be called anything):
//!
//! 1. `jalsfmt.toml` — an authored config already governs the tree; migrate nothing.
//! 2. `.editorconfig`
//! 3. `.idea/codeStyles/*.xml` carrying a `<code_scheme>`
//! 4. a top-level `*.xml` carrying a `<code_scheme>` (an exported IDE scheme)
//! 5. a top-level `*.xml` carrying an Eclipse formatter profile
//! 6. `.settings/org.eclipse.jdt.core.prefs`
//!
//! A.1's row 7 (a Spotless block in `build.gradle` / `pom.xml`) is deliberately absent. Spotless
//! configuration is a build DSL — code, not data — so its values cannot be read reliably, and it
//! selects a *delegate* engine. Guessing one would silently produce a config nobody wrote
//! (`DESIGN.md` P-gen-4).

// The ladder and the signatures name native products and files (IntelliJ, EditorConfig, …) in
// prose, not as Rust items.
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jals_config::fmt::Config;
use jals_exec::Exec;
use jals_fmt::generate::{MigrationWarning, Provenance};
use jals_fmt::import::{
    ConfigImporter, EclipsePrefs, EclipseXmlProfile, IntellijEditorConfig, IntellijXmlScheme,
};
use jals_storage::{FileKey, NativeScope, NativeStorage};

/// The config jals discovers on its own. Row 1 of the ladder.
const JALSFMT: &str = "jalsfmt.toml";
/// Row 2.
const EDITORCONFIG: &str = ".editorconfig";
/// Row 6.
const ECLIPSE_PREFS: &str = ".settings/org.eclipse.jdt.core.prefs";
/// Where IntelliJ keeps a project-committed code style (row 3).
const IDEA_CODE_STYLES: &str = ".idea/codeStyles";

/// Files that mark a directory as a project root, and so end the ancestor walk.
const PROJECT_MARKERS: [&str; 2] = ["jals.toml", ".git"];

/// How far up the ancestor walk may go before giving up. A backstop against a pathological path;
/// a real project root is a handful of levels away at most.
const MAX_ANCESTORS: usize = 64;

/// The Eclipse formatter setting prefix, which is what makes a `.prefs` file a *formatter* config
/// rather than a compiler or code-completion one.
const ECLIPSE_FORMATTER_PREFIX: &str = "org.eclipse.jdt.core.formatter.";

/// One resolved migration: a native config found, imported, and ready to write.
pub(crate) struct Migration {
    /// The directory the signature was found in — where `jalsfmt.toml` is written. It holds a
    /// native formatter config, so it is a project root by evidence.
    pub(crate) root: PathBuf,
    /// The imported config, already projected onto jals's surface.
    pub(crate) config: Config,
    /// Where it came from, for the generated file's header.
    pub(crate) provenance: Provenance,
    /// Notes worth showing the user and recording in the header.
    pub(crate) warnings: Vec<MigrationWarning>,
}

/// How far [`detect`] looks for a native config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Walk {
    /// Up through the ancestors, stopping at the project root.
    Ancestors,
    /// Only the directory itself — what `jals init` uses, so a new project does not silently
    /// inherit an unrelated parent repository's formatter config.
    DirectoryOnly,
}

impl Migration {
    /// Detect a native formatter config for `start` and lower it to a jals [`Config`].
    ///
    /// `Ok(None)` is A.1 row 8 — nothing to migrate. The caller keeps `Config::default()` and writes
    /// nothing.
    pub(crate) async fn detect(start: &Path, walk: Walk, exec: &Exec) -> Result<Option<Self>> {
        let start = crate::canonical_path(start);

        // Row 1. Checked against *every* ancestor, not just the ones the walk below visits: an
        // authored `jalsfmt.toml` anywhere above is the config that will actually be used
        // (`HostConfigs::for_dir` walks unbounded), so generating a second one under it would shadow
        // a file the user wrote.
        if start
            .ancestors()
            .take(MAX_ANCESTORS)
            .any(|dir| dir.join(JALSFMT).is_file())
        {
            return Ok(None);
        }

        for dir in Self::candidates(&start, walk) {
            if let Some(migration) = Self::detect_in(&dir, exec).await? {
                return Ok(Some(migration));
            }
        }
        Ok(None)
    }

    /// Write `migration`'s rendered config at its root, unless one is already there.
    ///
    /// Returns the path when a file was created. The root is an *ancestor* of the directory group
    /// `jals fmt` snapshots, so this opens its own storage and commits its own transaction rather
    /// than riding the group's.
    pub(crate) async fn write(&self, exec: &Exec) -> Result<Option<PathBuf>> {
        let key = FileKey::parse(JALSFMT).expect("static key is valid");
        let mut storage = NativeStorage::for_project_scoped(
            &self.root,
            [NativeScope::all(key.path().clone())],
            exec.clone(),
        )
        .await
        .with_context(|| format!("opening {}", self.root.display()))?;

        // Never overwrite. `detect` already stops at an existing `jalsfmt.toml`; this is the guard
        // that holds even if the file appeared in between.
        if storage.view().tree().lookup_file(&key).is_some() {
            return Ok(None);
        }

        let text = self.provenance.jalsfmt_toml(&self.config, &self.warnings);
        let mut transaction = storage.transaction(storage.revision())?;
        transaction.create_file(key, text.into_bytes())?;
        transaction
            .commit()
            .await
            .with_context(|| format!("writing {}", self.root.join(JALSFMT).display()))?;
        Ok(Some(self.root.join(JALSFMT)))
    }

    /// The directories to probe, nearest first.
    ///
    /// `Walk::Ancestors` stops after the first directory carrying a [project marker](PROJECT_MARKERS),
    /// and yields **nothing at all** when there is none. Detection writes a file, so it must not be
    /// able to reach out of the user's project — a tree with neither a manifest nor a repository is
    /// not a project, and inheriting (or generating) a config from `/tmp` or `/` there would be
    /// worse than doing nothing. Bounding by the working directory instead was rejected: it would
    /// make the answer depend on where `jals` was invoked from within one project.
    fn candidates(start: &Path, walk: Walk) -> Vec<PathBuf> {
        if walk == Walk::DirectoryOnly {
            return vec![start.to_path_buf()];
        }
        let mut out = Vec::new();
        for dir in start.ancestors().take(MAX_ANCESTORS) {
            out.push(dir.to_path_buf());
            if PROJECT_MARKERS
                .iter()
                .any(|marker| dir.join(marker).exists())
            {
                return out;
            }
        }
        Vec::new()
    }

    /// Apply rows 2–6 to one directory.
    async fn detect_in(dir: &Path, exec: &Exec) -> Result<Option<Self>> {
        let top_xml = Self::xml_files(dir, "");
        let style_xml = Self::xml_files(dir, IDEA_CODE_STYLES);
        let editorconfig = FileKey::parse(EDITORCONFIG).expect("static key is valid");
        let prefs = FileKey::parse(ECLIPSE_PREFS).expect("static key is valid");

        let scopes: Vec<NativeScope> = [&editorconfig, &prefs]
            .into_iter()
            .chain(&top_xml)
            .chain(&style_xml)
            .map(|key| NativeScope::all(key.path().clone()))
            .collect();
        let storage = NativeStorage::for_project_scoped(dir, scopes, exec.clone())
            .await
            .with_context(|| format!("reading {}", dir.display()))?;
        let view = storage.view();
        // A file whose bytes are not UTF-8 is not a config we can read; treat it as absent.
        let text = |key: &FileKey| view.file_text(key).ok();

        // Row 2 — `.editorconfig`. Unlike the other rows this one is judged by *yield* rather than by
        // a marker: the file name is unambiguous, and A.1's stricter `ij_*`-key signature would skip
        // a plain `.editorconfig`, whose universal properties (`indent_size`, `max_line_length`, …)
        // the IntelliJ model does carry. Nothing recognized ⇒ fall through rather than write an
        // empty config.
        if let Some(source) = text(&editorconfig)
            && let Some(config) = Self::import(IntellijEditorConfig::import(source), EDITORCONFIG)
            && config != Config::default()
        {
            return Ok(Some(Self::assemble(
                dir,
                config,
                EDITORCONFIG,
                "intellij",
                None,
                Vec::new(),
            )));
        }

        // Rows 3 and 4 — an IntelliJ code-style scheme, first the project-committed location, then a
        // top-level export.
        for (key, extra) in Self::claimant(&style_xml, &view, Self::is_intellij_scheme)
            .into_iter()
            .chain(Self::claimant(&top_xml, &view, Self::is_intellij_scheme))
        {
            let Some(source) = text(&key) else { continue };
            let name = key.to_string();
            if let Some(config) = Self::import(IntellijXmlScheme::import(source), &name) {
                let version = Self::attribute(source, "<code_scheme", "version");
                return Ok(Some(Self::assemble(
                    dir, config, &name, "intellij", version, extra,
                )));
            }
        }

        // Row 5 — an exported Eclipse formatter profile.
        if let Some((key, mut extra)) = Self::claimant(&top_xml, &view, Self::is_eclipse_profile)
            && let Some(source) = text(&key)
        {
            let name = key.to_string();
            // `EclipseProfileReader` merges every `<setting>` in the document into one map without
            // looking at the enclosing `<profile>`, and an "Export All" file holds several. The
            // result is a last-wins hybrid of all of them, so say so rather than pass it off as one
            // profile's settings.
            let profiles = source.matches("<profile ").count();
            if profiles > 1 {
                extra.push(MigrationWarning::ambiguous(
                    name.clone(),
                    format!("the file declares {profiles} profiles and their settings were merged"),
                ));
            }
            if let Some(config) = Self::import(EclipseXmlProfile::import(source), &name) {
                let version = Self::attribute(source, "<profile ", "version");
                return Ok(Some(Self::assemble(
                    dir, config, &name, "eclipse", version, extra,
                )));
            }
        }

        // Row 6 — the Eclipse preference store.
        if let Some(source) = text(&prefs)
            && source.contains(ECLIPSE_FORMATTER_PREFIX)
            && let Some(config) = Self::import(EclipsePrefs::import(source), ECLIPSE_PREFS)
        {
            return Ok(Some(Self::assemble(
                dir,
                config,
                ECLIPSE_PREFS,
                "eclipse",
                None,
                Vec::new(),
            )));
        }

        Ok(None)
    }

    /// Assemble a [`Migration`], folding the §17 rounding notes in with whatever the detector found.
    fn assemble(
        dir: &Path,
        config: Config,
        source: &str,
        tool: &'static str,
        version: Option<String>,
        mut warnings: Vec<MigrationWarning>,
    ) -> Self {
        warnings.extend(MigrationWarning::rounding(&config));
        Self {
            root: dir.to_path_buf(),
            config,
            provenance: Provenance {
                source: source.to_owned(),
                tool,
                version,
            },
            warnings,
        }
    }

    /// The one candidate in `keys` that answers a row: the lowest-sorted file whose text `claims`
    /// recognizes, paired with the warnings its selection implies.
    ///
    /// `keys` is sorted, so the choice is deterministic. When several files answer the same row the
    /// others are named in a warning, so the user can tell which config actually won.
    fn claimant(
        keys: &[FileKey],
        view: &jals_storage::ProjectView,
        claims: fn(&str) -> bool,
    ) -> Option<(FileKey, Vec<MigrationWarning>)> {
        let matched: Vec<&FileKey> = keys
            .iter()
            .filter(|key| view.file_text(key).is_ok_and(claims))
            .collect();
        let (&chosen, rest) = matched.split_first()?;
        let mut warnings = Vec::new();
        if !rest.is_empty() {
            let others: Vec<String> = rest.iter().map(ToString::to_string).collect();
            warnings.push(MigrationWarning::ambiguous(
                chosen.to_string(),
                format!("it was chosen over {}", others.join(", ")),
            ));
        }
        Some((chosen.clone(), warnings))
    }

    /// Unwrap an import, reporting a malformed native config instead of failing the run.
    ///
    /// A team's Eclipse profile being broken is not a reason `jals fmt` cannot format Java. The row
    /// is skipped and the ladder continues.
    fn import(
        result: Result<Config, jals_fmt::import::ImportError>,
        source: &str,
    ) -> Option<Config> {
        match result {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!("warning: ignoring {source}: {error}");
                None
            }
        }
    }

    /// Whether `src` is an IntelliJ code-style scheme (rows 3 and 4 both key off the `<code_scheme>`
    /// element; only its nesting differs, and both read through the same importer).
    fn is_intellij_scheme(src: &str) -> bool {
        src.contains("<code_scheme")
    }

    /// Whether `src` is an exported Eclipse formatter profile.
    fn is_eclipse_profile(src: &str) -> bool {
        src.contains("kind=\"CodeFormatterProfile\"") || src.contains(ECLIPSE_FORMATTER_PREFIX)
    }

    /// The value of `name="…"` in the first `element` start tag of `src`, when it has one. Used only
    /// for the generated header's version note, so a quoting form this misses just omits it.
    fn attribute(src: &str, element: &str, name: &str) -> Option<String> {
        let tag = src.split_once(element)?.1.split_once('>')?.0;
        let value = tag.split_once(&format!("{name}=\""))?.1.split_once('"')?.0;
        Some(value.to_owned())
    }

    /// The `*.xml` files directly inside `dir/relative` (`relative` empty ⇒ `dir` itself), as keys
    /// relative to `dir`.
    ///
    /// Listing one directory is what keeps content-based XML detection affordable: scoping a snapshot
    /// by extension from the project root would walk and ingest the whole tree, `target/` included.
    /// `read_dir` yields in filesystem order, so the sort is what makes detection deterministic.
    /// A directory that cannot be listed simply contributes no candidates — it is not evidence of a
    /// config we then fail to read.
    fn xml_files(dir: &Path, relative: &str) -> Vec<FileKey> {
        let target = if relative.is_empty() {
            dir.to_path_buf()
        } else {
            dir.join(relative)
        };
        let Ok(entries) = std::fs::read_dir(&target) else {
            return Vec::new();
        };
        let mut out: Vec<FileKey> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                if !name.to_ascii_lowercase().ends_with(".xml") {
                    return None;
                }
                let path = if relative.is_empty() {
                    name
                } else {
                    format!("{relative}/{name}")
                };
                FileKey::parse(&path).ok()
            })
            .collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_editorconfig_naming_another_language_is_not_java() {
        // A `[*.javascript]` section must not be read as the Java one (DESIGN.md A.2 / A.8).
        //
        // Compared against an IntelliJ import that read *nothing*, not against `Config::default()`:
        // importing an IDEA config carries IDEA's own fixed Javadoc readings whatever the file
        // declares, so the claim here is that this section moved nothing — which is a statement
        // about the section, not about the vendor.
        let config =
            IntellijEditorConfig::import("[*.javascript]\nindent_size = 7\nmax_line_length = 77\n")
                .expect("the fixture should import");
        let untouched =
            IntellijEditorConfig::import("[*.java]\n").expect("the empty section should import");
        assert_eq!(config, untouched);
    }

    #[test]
    fn a_prefs_file_without_formatter_settings_does_not_match() {
        let compiler_only = "\
eclipse.preferences.version=1
org.eclipse.jdt.core.compiler.source=21
";
        assert!(!compiler_only.contains(ECLIPSE_FORMATTER_PREFIX));
    }

    #[test]
    fn the_xml_signatures_separate_the_two_vendors() {
        let scheme = r#"<component name="ProjectCodeStyleConfiguration">
  <code_scheme name="Project" version="173"/>
</component>"#;
        let profile = r#"<profiles version="23">
  <profile kind="CodeFormatterProfile" name="Team" version="23"/>
</profiles>"#;

        assert!(Migration::is_intellij_scheme(scheme) && !Migration::is_eclipse_profile(scheme));
        assert!(Migration::is_eclipse_profile(profile) && !Migration::is_intellij_scheme(profile));
        // A Maven POM sitting next to them claims neither row.
        let pom = "<project><artifactId>app</artifactId></project>";
        assert!(!Migration::is_intellij_scheme(pom) && !Migration::is_eclipse_profile(pom));
    }

    #[test]
    fn the_version_attribute_is_read_from_the_right_element() {
        let profile = r#"<profiles version="1">
  <profile kind="CodeFormatterProfile" name="Team" version="23"/>
</profiles>"#;
        assert_eq!(
            Migration::attribute(profile, "<profile ", "version").as_deref(),
            Some("23")
        );

        let scheme = r#"<code_scheme name="Project" version="173"/>"#;
        assert_eq!(
            Migration::attribute(scheme, "<code_scheme", "version").as_deref(),
            Some("173")
        );
        assert_eq!(
            Migration::attribute(scheme, "<code_scheme", "missing"),
            None
        );
    }

    #[test]
    fn the_walk_stops_at_a_project_marker() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = crate::canonical_path(dir.path());
        std::fs::write(root.join("jals.toml"), "[package]\nname = \"x\"\n").expect("write");
        let nested = root.join("src/main/java");
        std::fs::create_dir_all(&nested).expect("mkdir");

        let walked = Migration::candidates(&nested, Walk::Ancestors);

        assert_eq!(
            walked,
            vec![
                root.join("src/main/java"),
                root.join("src/main"),
                root.join("src"),
                root.clone(),
            ]
        );
        // And a directory-only walk never leaves where it started.
        assert_eq!(
            Migration::candidates(&nested, Walk::DirectoryOnly),
            vec![root.join("src/main/java")]
        );
    }

    #[test]
    fn a_tree_with_no_project_marker_is_not_walked_at_all() {
        // Without this, `jals fmt /tmp/scratch/A.java` could pick up `/tmp/.editorconfig` — and
        // then write a `jalsfmt.toml` outside anything the user considers their project.
        let dir = tempfile::tempdir().expect("a temp dir");
        let nested = crate::canonical_path(dir.path()).join("scratch");
        std::fs::create_dir_all(&nested).expect("mkdir");

        assert_eq!(
            Migration::candidates(&nested, Walk::Ancestors),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn xml_candidates_are_sorted_and_extension_matched() {
        let dir = tempfile::tempdir().expect("a temp dir");
        for name in ["zeta.xml", "alpha.XML", "notes.txt", "pom.xml"] {
            std::fs::write(dir.path().join(name), "<x/>").expect("write");
        }
        std::fs::create_dir(dir.path().join("nested.xml")).expect("mkdir");

        let found: Vec<String> = Migration::xml_files(dir.path(), "")
            .iter()
            .map(ToString::to_string)
            .collect();

        assert_eq!(found, ["alpha.XML", "pom.xml", "zeta.xml"]);
    }
}
