//! The JDK's own type signatures: where `java.lang.Object` comes from when a project is analysed
//! or compiled by something other than a spawned `javac`.
//!
//! `jals-hir` ships a handful of signature-only `java.lang`/`java.util`/`java.io` stubs so an editor
//! buffer can be typed with no JDK anywhere. They are deliberately partial — a stub `System` does
//! not declare `lineSeparator` — and a rule that reports a *cannot resolve* against them reports the
//! absence of a stub rather than a defect in the project. The real signatures are
//! `$JAVA_HOME/lib/ct.sym`: the archive `javac --release` reads, an ordinary zip of ordinary class
//! files with their method bodies stripped. Decoding it gives every analysis the same platform
//! `javac` itself uses.
//!
//! This module is the one place that archive is read. `jals lint`, the language server, and the
//! in-process `jals` backend each take what comes out of it, which is what makes "the three hosts
//! see one platform class set" a structural property: there is one reader and one signature set,
//! so the only way for them to disagree is for one of them to stop asking.
//!
//! # Which JDK
//!
//! The rule this states is that **the platform classes come from the JDK the project compiles
//! with**. `[toolchain] compiler` selects a `javac`, and that `javac` lives in the home whose
//! `ct.sym` is read — so an analysis of a project pinned to JDK 21 resolves `String` against JDK 21
//! even on a host whose `PATH` `javac` is 25. Two cases have no `javac` to follow:
//! `[toolchain] compiler = "builtin"` selects the in-process compiler, and an explicit `$JAVAC` can
//! point anywhere. There the home is the project's default toolchain (`target/jdk/default`), then
//! `$JAVA_HOME`, then the first installed JDK discovery finds. A host with no JDK at all gets
//! [`PlatformSelection::Absent`] and analysis falls back to the embedded stubs — the same answer it
//! gave before there were platform classes at all.
//!
//! # Which release
//!
//! `[build] release` when the project names one, otherwise the release the selected JDK is. A
//! `ct.sym` describes what a JDK can *target* and not always what it *is* — a JDK 21's archive
//! carries 8 through 20 — so a release the archive does not describe is read as the newest release
//! it does. The gap that leaves is the API the JDK's own release added, and
//! [`read_ct_sym`](PlatformClasses::read_ct_sym) says why it is taken rather than closed.
//!
//! # Native I/O
//!
//! Reading `ct.sym` needs a host path and a zip decoder, so this module is `native`-gated like the
//! rest of the toolchain host half, and every step that touches the filesystem runs on
//! [`on_blocking_pool`](jals_exec::tokio_rt::on_blocking_pool). Everything it produces —
//! [`jals_classfile::ClassFile`] values — is portable and crosses no boundary afterwards.

use std::path::{Path, PathBuf};

use jals_classfile::ClassFile;
use jals_config::Manifest;
use jals_exec::tokio_rt::on_blocking_pool;

use crate::native::SubprocessToolchain;
use crate::toolchain::{JdkInstall, Tool};

/// Why a JDK's platform signatures could not be read.
///
/// A message rather than an enum of cases because the only caller does one thing with it — report
/// it and fall back to the stubs — and each case already knows what to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformError(String);

impl PlatformError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl core::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlatformError {}

/// One JDK's platform signatures, decoded from its `lib/ct.sym`.
///
/// Held as parsed [`ClassFile`]s rather than as bytes: every consumer folds them into a
/// [`ProjectIndex`](jals_hir::ProjectIndex) through
/// [`ProjectIndex::lower_classpath`](jals_hir::ProjectIndex::lower_classpath), and decoding a class
/// file is the only part of that which is not free to repeat.
#[derive(Debug, Default)]
pub struct PlatformClasses {
    classes: Vec<ClassFile>,
}

/// What [`PlatformClasses::for_project`] found on this host.
///
/// Three answers and not two, for the same reason [`BackendSelection`](crate::BackendSelection) has
/// an `Absent` arm: "there is no JDK here" is a normal state an analysis copes with, while "there is
/// a JDK here and its `ct.sym` is unreadable" is a fault somebody would want to know about. Folding
/// them into one would report a missing JDK as a broken one, or hide a broken one behind the stubs'
/// silence.
#[derive(Debug)]
pub enum PlatformSelection {
    /// A JDK was identified and its signatures were read.
    Available(PlatformClasses),
    /// No JDK this project resolves to. Analysis falls back to the embedded stubs.
    Absent,
    /// A JDK was identified but its platform signatures could not be read.
    Unreadable(PlatformError),
}

impl PlatformClasses {
    /// The letter `ct.sym` files a feature release under: `8` and `9` are themselves, then `A` is
    /// 10, `B` is 11, and so on.
    ///
    /// A directory name inside the archive is a *set* of these letters (`MNOP` covers 22 through
    /// 25), so an entry belongs to a release when its first path segment contains that release's
    /// letter.
    const fn release_letter(release: u32) -> Option<char> {
        match release {
            8 | 9 => char::from_digit(release, 10),
            10.. => char::from_u32('A' as u32 + (release - 10)),
            _ => None,
        }
    }

    /// Whether `entry` is a `ct.sym` archive member holding one signature for `release`.
    ///
    /// Entries are named `<releases>/<module>/<package>/<Class>.sig`, where `<releases>` is the set
    /// of release letters the signature is valid for. There is **no module filter**: `javac`
    /// compiles against the whole platform of the release it was asked for, so an analysis that
    /// dropped `jdk.unsupported` would report `sun.misc.Unsafe` as unresolvable on a project `javac`
    /// builds without a word. What a release's platform *is* is one set, and this is where it is
    /// read.
    #[expect(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "a `ct.sym` member name is an archive entry and not a host path, and the archive \
                  spells the suffix lower-case — a case-insensitive match would admit a member no \
                  JDK writes"
    )]
    fn is_signature_entry(entry: &str, release: char) -> bool {
        let Some((releases, rest)) = entry.split_once('/') else {
            return false;
        };
        let Some((_module, class_path)) = rest.split_once('/') else {
            return false;
        };
        releases.contains(release) && class_path.ends_with(".sig")
    }

    /// Read every signature the JDK at `home` ships for `release`.
    ///
    /// Two archives, because a JDK describes two different things and they are not the same file.
    /// `lib/ct.sym` is what `javac --release` reads: the API of every release the JDK can *target*.
    /// The release the JDK *is* is often not among them — a JDK 21's `ct.sym` carries 8 through 20
    /// and only a `system-modules` marker for 21 — so that release's platform is the modules the
    /// JDK actually ships, read out of `jmods/`. Those are the same class set the `jrt:` filesystem
    /// exposes, and a zip each, so no second archive decoder is involved.
    ///
    /// # Errors
    /// If `lib/ct.sym` is missing or unreadable, or if neither source holds a single class. A JRE
    /// or a stripped image cannot supply a platform at all, and falling back to the embedded stubs
    /// silently would report stub coverage wearing the name of a real analysis.
    pub async fn read(home: &Path, release: u32) -> Result<Self, PlatformError> {
        let home = home.to_path_buf();
        on_blocking_pool(move || Self::read_blocking(&home, release)).await
    }

    /// Read the signatures of the JDK at `home`, for the feature release that home states.
    ///
    /// # Errors
    /// If `home` states no feature release, or [`read`](Self::read) cannot read its `ct.sym`.
    async fn read_home(home: &Path) -> Result<Self, PlatformError> {
        let home = home.to_path_buf();
        on_blocking_pool(move || {
            let release = Self::feature_release(&home).ok_or_else(|| {
                PlatformError::new(format!(
                    "{} states no `JAVA_VERSION` — is this a JDK home?",
                    home.display()
                ))
            })?;
            Self::read_blocking(&home, release)
        })
        .await
    }

    /// The platform classes of the JDK a project resolves to, or why there are none.
    ///
    /// The JDK homes are asked in the order [`candidate_homes`](Self::candidate_homes) states; the
    /// first one holding a `lib/ct.sym` is the answer, and everything after it is never read.
    pub async fn for_project(manifest: &Manifest, project_root: &Path) -> PlatformSelection {
        let homes = Self::candidate_homes(manifest, project_root).await;
        let wanted = manifest.build.release;
        let home = on_blocking_pool(move || {
            homes
                .into_iter()
                .find(|home| home.join("lib").join("ct.sym").is_file())
        })
        .await;
        let Some(home) = home else {
            return PlatformSelection::Absent;
        };
        // `[build] release` is the API the project compiles against; a project that names none
        // compiles to whatever its JDK is. Both are `read`'s question, and answering them from the
        // same home is the whole of the consistency rule above.
        let read = match wanted {
            Some(release) => Self::read(&home, release).await,
            None => Self::read_home(&home).await,
        };
        match read {
            Ok(classes) => PlatformSelection::Available(classes),
            Err(error) => PlatformSelection::Unreadable(error),
        }
    }

    /// [`for_project`](Self::for_project) for a host with no project at all — a standalone file
    /// outside any `jals.toml`. There is no `[toolchain] compiler` to follow, so the home is
    /// `$JAVA_HOME`, then the project's (absent here), then discovery.
    pub async fn for_host(project_root: &Path) -> PlatformSelection {
        Self::for_project(&Manifest::default(), project_root).await
    }

    /// The JDK homes a project's platform signatures may come from, most-preferred first.
    ///
    /// Order is the consistency rule: first the home that owns the `javac` `[toolchain] compiler`
    /// selects, then the project's own default toolchain and `$JAVA_HOME`, then whatever discovery
    /// finds. Duplicates are dropped on the way out so a host that set `$JAVA_HOME` to its only JDK
    /// does not read `ct.sym` twice.
    async fn candidate_homes(manifest: &Manifest, project_root: &Path) -> Vec<PathBuf> {
        let mut homes: Vec<PathBuf> = Vec::new();
        // The `javac` the project compiles with, when it selects one at all. `builtin` names the
        // in-process compiler and has no home to follow.
        if manifest.toolchain.compiler.spec().is_some() {
            let toolchain = SubprocessToolchain::from_manifest(manifest, project_root).await;
            let tool = toolchain.resolve_program(Tool::Javac, project_root).await;
            // The tool is `<home>/bin/<tool>`, so the home is two levels up. A bare `javac` from
            // `PATH` resolves to itself and canonicalizing it finds the real JDK behind the
            // distribution's symlink (`/usr/bin/javac` → `/usr/lib/jvm/…/bin/javac`).
            if let Some(home) = tool
                .canonicalize()
                .ok()
                .and_then(|tool| tool.parent().and_then(Path::parent).map(Path::to_path_buf))
            {
                homes.push(home);
            }
        }
        // The project's own default toolchain takes the `$JAVA_HOME` slot for the same reason the
        // run step gives it: a project's choice beats the host's.
        if let Some(default) = SubprocessToolchain::project_default(project_root) {
            homes.push(default);
        }
        if let Some(java_home) = std::env::var_os("JAVA_HOME") {
            homes.push(PathBuf::from(java_home));
        }
        let root = project_root.to_path_buf();
        for install in on_blocking_pool(move || JdkInstall::discover(Some(&root))).await {
            homes.push(install.home().to_path_buf());
        }

        let mut seen: Vec<String> = Vec::new();
        homes.retain(|home| {
            let key = home.to_string_lossy().into_owned();
            if seen.contains(&key) {
                return false;
            }
            seen.push(key);
            true
        });
        homes
    }

    /// The JDK the `java` on `PATH` reports as the running one — `(home, feature release)`.
    ///
    /// `$JAVA_HOME` is routinely unset even where a JDK is installed, so it is not consulted;
    /// asking the JVM for `java.home` works whenever `java` is on `PATH` at all. Used by the corpus
    /// harness, which scores against whichever JDK its pinned `javac` is — that is a different
    /// question from which JDK a *project* resolves to, which is
    /// [`for_project`](Self::for_project)'s.
    pub async fn detect_running() -> Option<(PathBuf, u32)> {
        on_blocking_pool(|| {
            let output = std::process::Command::new("java")
                .args(["-XshowSettings:properties", "-version"])
                .output()
                .ok()?;
            // The settings dump goes to stderr, one `  name = value` per line.
            let text = String::from_utf8_lossy(&output.stderr).into_owned();
            let property = |name: &str| {
                text.lines()
                    .filter_map(|line| line.split_once('='))
                    .find(|(key, _)| key.trim() == name)
                    .map(|(_, value)| value.trim().to_owned())
            };
            let home = PathBuf::from(property("java.home")?);
            let release = property("java.specification.version")?.parse().ok()?;
            Some((home, release))
        })
        .await
    }

    /// The decoded signatures, in archive order.
    #[cfg_attr(not(test), allow(dead_code))]
    fn classes(&self) -> &[ClassFile] {
        &self.classes
    }

    /// The decoded signatures, consuming the reader.
    pub fn into_classes(self) -> Vec<ClassFile> {
        self.classes
    }

    /// How many signatures were read.
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(&self) -> usize {
        self.classes.len()
    }

    /// Whether this JDK supplied no signatures at all (never true of a successful read).
    #[cfg_attr(not(test), allow(dead_code))]
    const fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// The feature release the JDK at `home` states in its `release` file, if it states one.
    ///
    /// `JAVA_VERSION="21.0.12.1"` — the dotted form, so the major is the first component. The
    /// install directory's name is deliberately not consulted here: a home is often reached through
    /// a symlink or a package-manager path that carries no version in its name at all.
    fn feature_release(home: &Path) -> Option<u32> {
        let text = std::fs::read_to_string(home.join("release")).ok()?;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() != "JAVA_VERSION" {
                continue;
            }
            let value = value.trim().trim_matches('"');
            let mut parts = value.split('.');
            let major = parts.next()?;
            // Legacy `1.8` spelling: the real major is the second component.
            if major == "1" {
                return parts.next()?.parse().ok();
            }
            return major.parse().ok();
        }
        None
    }

    /// The blocking half of [`read`](Self::read): the archive walk and every class-file decode.
    fn read_blocking(home: &Path, release: u32) -> Result<Self, PlatformError> {
        let classes = Self::read_ct_sym(home, release)?;
        if classes.is_empty() {
            return Err(PlatformError::new(format!(
                "{} holds no platform classes for release {release}",
                home.display()
            )));
        }
        Ok(Self { classes })
    }

    /// The signatures of `release` out of `lib/ct.sym`, in archive order.
    ///
    /// A JDK's `ct.sym` describes the releases it can *target*, and the release it *is* is often
    /// not among them: a JDK 21's archive carries 8 through 20 and only a `system-modules` marker
    /// for 21. When the requested release is one of those, the signatures of the **newest release
    /// the archive does describe** are what this reads — the platform of the release before the
    /// JDK's own, which is every API but the ones the newest release added.
    ///
    /// That gap is deliberate and it is named: closing it means reading the modules the JDK itself
    /// ships (`jmods/`, the class set `jrt:` exposes), and those carry method bodies — a full
    /// decode of some 28 000 classes on every run, against the 4 700 a `ct.sym` release holds. That
    /// is a caching problem rather than a source problem, and it is the follow-up.
    fn read_ct_sym(home: &Path, release: u32) -> Result<Vec<ClassFile>, PlatformError> {
        let path = home.join("lib").join("ct.sym");
        let bytes = std::fs::read(&path)
            .map_err(|error| PlatformError::new(format!("open {}: {error}", path.display())))?;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|error| {
            PlatformError::new(format!("{} is not a zip: {error}", path.display()))
        })?;

        // The letter to read for, decided before anything is decoded: the requested release's
        // when the archive carries it, else the newest letter any entry names.
        let target = Self::release_letter(release).ok_or_else(|| {
            PlatformError::new(format!("JDK {release} has no ct.sym release letter"))
        })?;
        let mut letters: Vec<char> = Vec::new();
        for index in 0..archive.len() {
            let Ok(entry) = archive.by_index(index) else {
                continue;
            };
            let Some(name) = entry.enclosed_name() else {
                continue;
            };
            let name = name.to_string_lossy();
            let Some((releases, rest)) = name.split_once('/') else {
                continue;
            };
            // Only a signature names a release: `L/system-modules` is the archive's module list for
            // a release it otherwise says nothing about, and reading it as "this release is
            // covered" is what would leave a JDK's own release with no classes at all.
            #[expect(
                clippy::case_sensitive_file_extension_comparisons,
                reason = "a member name here is an archive entry and not a host path, and ct.sym                           spells the suffix lower-case"
            )]
            if !rest.contains('/') || !rest.ends_with(".sig") {
                continue;
            }
            for letter in releases.chars() {
                if Self::release_of_letter(letter).is_some() && !letters.contains(&letter) {
                    letters.push(letter);
                }
            }
        }
        let wanted = if letters.contains(&target) {
            target
        } else {
            letters
                .into_iter()
                .max_by_key(|letter| Self::release_of_letter(*letter))
                .unwrap_or(target)
        };

        Ok(Self::collect(&mut archive, |entry| {
            Self::is_signature_entry(entry, wanted)
        }))
    }

    /// The release a `ct.sym` release letter stands for, the inverse of
    /// [`release_letter`](Self::release_letter).
    const fn release_of_letter(letter: char) -> Option<u32> {
        match letter {
            '8' | '9' => letter.to_digit(10),
            'A'..='Z' => Some((letter as u32 - 'A' as u32) + 10),
            _ => None,
        }
    }

    /// Every member `take` wants, decoded into a class file.
    ///
    /// A member this workspace's own reader cannot parse is a `jals-classfile` gap, and it is not
    /// this module's subject: the type simply does not appear, and whatever needed it says so at
    /// the point of use. A `module-info` is dropped for a different reason — it is a module
    /// descriptor and not a type, and the index has no module layer to file it under.
    fn collect(
        archive: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
        take: impl Fn(&str) -> bool,
    ) -> Vec<ClassFile> {
        let mut classes = Vec::new();
        for index in 0..archive.len() {
            let Ok(mut entry) = archive.by_index(index) else {
                continue;
            };
            let Some(name) = entry.enclosed_name() else {
                continue;
            };
            let name = name.to_string_lossy().replace('\\', "/");
            if !take(&name)
                || name.ends_with("module-info.sig")
                || name.ends_with("module-info.class")
            {
                continue;
            }
            let mut bytes = Vec::new();
            if std::io::Read::read_to_end(&mut entry, &mut bytes).is_err() {
                continue;
            }
            if let Ok(class) = jals_exec::block_on_inline(ClassFile::read(bytes.as_slice())) {
                classes.push(class);
            }
        }
        classes
    }
}

#[cfg(test)]
mod tests {
    use super::{PlatformClasses, PlatformSelection};

    #[test]
    fn a_release_letter_is_its_own_for_8_and_9() {
        assert_eq!(PlatformClasses::release_letter(8), Some('8'));
        assert_eq!(PlatformClasses::release_letter(9), Some('9'));
    }

    #[test]
    fn a_release_letter_counts_from_a_for_10() {
        assert_eq!(PlatformClasses::release_letter(10), Some('A'));
        assert_eq!(PlatformClasses::release_letter(11), Some('B'));
        assert_eq!(PlatformClasses::release_letter(20), Some('K'));
        assert_eq!(PlatformClasses::release_letter(21), Some('L'));
        assert_eq!(PlatformClasses::release_letter(25), Some('P'));
    }

    #[test]
    fn a_release_below_8_has_no_letter() {
        assert_eq!(PlatformClasses::release_letter(7), None);
    }

    #[test]
    fn a_signature_entry_names_releases_module_and_class() {
        let release = PlatformClasses::release_letter(20).unwrap();
        // A directory name is a *set* of release letters, so membership is what selects an entry.
        assert!(PlatformClasses::is_signature_entry(
            "BCDEFGHIJK/java.base/java/lang/Object.sig",
            release
        ));
        assert!(PlatformClasses::is_signature_entry(
            "K/jdk.unsupported/sun/misc/Unsafe.sig",
            release
        ));
    }

    #[test]
    fn a_signature_entry_of_another_release_is_not_ours() {
        let release = PlatformClasses::release_letter(21).unwrap();
        assert!(!PlatformClasses::is_signature_entry(
            "BCDEFGHIJK/java.base/java/lang/Object.sig",
            release
        ));
    }

    #[test]
    fn a_member_that_is_not_a_signature_is_not_one() {
        let release = PlatformClasses::release_letter(20).unwrap();
        // Not `.sig`, and not the three-segment shape a signature has.
        assert!(!PlatformClasses::is_signature_entry(
            "8/META-INF/MANIFEST.MF",
            release
        ));
        assert!(!PlatformClasses::is_signature_entry("README", release));
    }

    #[test]
    fn a_real_jdk_supplies_the_platform_classes() {
        // The host's own JDK, found the way every host finds one. A corpus-shaped test stands
        // itself down rather than failing where the host cannot supply what it needs.
        let PlatformSelection::Available(classes) =
            jals_exec::block_on_inline(PlatformClasses::for_project(
                &jals_config::Manifest::default(),
                std::path::Path::new("."),
            ))
        else {
            eprintln!("no JDK on this host; standing down");
            return;
        };
        assert!(!classes.is_empty());
        // The one type every analysis needs, and the one a partial stub would otherwise answer for.
        let names = classes.classes().iter().map(|class| {
            class
                .constant_pool
                .class_name(class.this_class)
                .unwrap_or_default()
                .into_owned()
        });
        assert!(
            names.into_iter().any(|name| name == "java/lang/Object"),
            "the platform holds no java.lang.Object"
        );
    }
}
