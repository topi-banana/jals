//! Decoding the JDK archives `jals toolchain` downloads.
//!
//! A provider hands back an `https://` location and nothing about its shape is trusted twice: the
//! archive kind is sniffed from the bytes, so a redirect that lands on a different extension (the
//! Adoptium binary endpoint has none at all) cannot lie to the decoder, and the JDK home is found
//! by looking for `bin/java` rather than by assuming a directory layout — Temurin ships a plain
//! `jdk-21…/bin/java` on Linux and a `jdk-21….jdk/Contents/Home/bin/java` bundle on macOS, and both
//! come out of one rule.
//!
//! Nothing here writes outside the caller's scratch directory. Tar entries are validated by the
//! `tar` crate's `unpack_in`, zip entries through `ZipFile::enclosed_name`, and the one rename that
//! puts a home in place is a rename inside the project's own `target/jdk`.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

/// A JDK home's defining file, per platform.
pub(crate) struct JdkHome;

impl JdkHome {
    /// The executable a directory must hold under `bin/` to be a JDK home.
    pub(crate) const fn java() -> &'static str {
        if cfg!(windows) { "java.exe" } else { "java" }
    }

    /// The compiler a full JDK additionally holds.
    pub(crate) const fn javac() -> &'static str {
        if cfg!(windows) { "javac.exe" } else { "javac" }
    }

    /// Whether `path` is shaped like a JDK home (`bin/<java>` exists).
    pub(crate) fn is_home(path: &Path) -> bool {
        path.join("bin").join(Self::java()).is_file()
    }

    /// Whether `path` is a usable JDK home — a home that also holds `javac`.
    ///
    /// [`is_home`](Self::is_home) is the archive question (a provider ships a JDK, and `java` is
    /// the file every JDK has); this is the stronger one `link` asks, because registering a
    /// directory as a toolchain promises the manifest's `compiler` selection something to run.
    pub(crate) fn is_jdk(path: &Path) -> bool {
        Self::is_home(path) && path.join("bin").join(Self::javac()).is_file()
    }
}

/// How an archive's bytes are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Archive {
    /// A gzip-compressed tar — every Unix JDK build.
    TarGz,
    /// A zip — every Windows JDK build, and some vendors' Unix builds.
    Zip,
}

impl Archive {
    /// Identify an archive from its first bytes.
    ///
    /// Sniffing rather than trusting the URL's extension is deliberate: the Adoptium binary
    /// endpoint redirects to a filename only the origin ever sees, and a provider that changed its
    /// packaging would otherwise hand the wrong decoder an archive it cannot read.
    pub(crate) fn sniff(bytes: &[u8]) -> Result<Self> {
        if bytes.starts_with(&[0x1f, 0x8b]) {
            return Ok(Self::TarGz);
        }
        if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
            return Ok(Self::Zip);
        }
        bail!(
            "the downloaded archive is neither gzip nor zip: {}",
            Self::sample(bytes)
        )
    }

    /// A short hex preview of a response, for the error above.
    fn sample(bytes: &[u8]) -> String {
        use std::fmt::Write as _;

        let mut rendered = String::new();
        for byte in bytes.iter().take(4) {
            if !rendered.is_empty() {
                rendered.push(' ');
            }
            let _ = write!(rendered, "{byte:02x}");
        }
        if rendered.is_empty() {
            rendered.push_str("<empty>");
        }
        rendered
    }

    /// Extract `bytes` into `destination`, returning the JDK home inside it.
    ///
    /// `destination` is the caller's scratch directory; the returned path is a descendant of it.
    pub(crate) fn unpack(self, bytes: &[u8], destination: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(destination)
            .with_context(|| format!("creating {}", destination.display()))?;
        match self {
            Self::TarGz => Self::unpack_tar_gz(bytes, destination)?,
            Self::Zip => Self::unpack_zip(bytes, destination)?,
        }
        Self::locate_home(destination)
    }

    /// Unpack a `.tar.gz` into `destination`.
    fn unpack_tar_gz(bytes: &[u8], destination: &Path) -> Result<()> {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        // Executable bits are load-bearing: `bin/java` that cannot be spawned is not a JDK.
        // Ownership and timestamps are not — the install belongs to whoever ran `jals`.
        archive.set_preserve_permissions(true);
        archive.set_preserve_ownerships(false);
        archive.set_preserve_mtime(false);
        archive
            .unpack(destination)
            .with_context(|| format!("extracting into {}", destination.display()))?;
        Ok(())
    }

    /// Unpack a `.zip` into `destination`.
    fn unpack_zip(bytes: &[u8], destination: &Path) -> Result<()> {
        let mut archive = zip::ZipArchive::new(io::Cursor::new(bytes))
            .context("reading the downloaded zip archive")?;
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .with_context(|| format!("reading zip member {index}"))?;
            let Some(relative) = entry.enclosed_name() else {
                bail!(
                    "zip member `{}` escapes the extraction directory",
                    entry.name()
                );
            };
            // `enclosed_name` normalizes; `mangled_name` would too, but a member that needed it is
            // exactly the one not worth accepting from a vendor archive.
            let out = destination.join(&relative);
            if entry.is_dir() {
                std::fs::create_dir_all(&out)
                    .with_context(|| format!("creating {}", out.display()))?;
                continue;
            }
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let mut file = std::fs::File::create(&out)
                .with_context(|| format!("creating {}", out.display()))?;
            io::copy(&mut entry, &mut file)
                .with_context(|| format!("extracting {}", out.display()))?;
            drop(file);
            Self::apply_unix_mode(&out, entry.unix_mode());
        }
        Ok(())
    }

    /// Give a zip member the executable bit its Unix mode asks for, where that is a thing.
    ///
    /// A mode-less member keeps the umask's answer, which is what the tar path gives a member with
    /// no permission bits.
    #[cfg(unix)]
    fn apply_unix_mode(path: &Path, mode: Option<u32>) {
        use std::os::unix::fs::PermissionsExt as _;

        if let Some(mode) = mode {
            let mode = mode & 0o7777;
            if mode != 0 {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
            }
        }
    }

    /// Windows has no executable bit to restore: a member keeps whatever the filesystem assigns.
    ///
    /// Split from the Unix body rather than one function with a `cfg`'d empty arm, because the
    /// Windows arm then *is* a `const fn` and clippy says so — and an `#[allow]` for that would be
    /// a suppression Linux's `cargo unused-allow` correctly reports as unused.
    #[cfg(not(unix))]
    const fn apply_unix_mode(_path: &Path, _mode: Option<u32>) {}

    /// The directory inside `extracted` holding `bin/java`, nearest the root.
    ///
    /// Breadth-first over at most [`Self::MAX_HOME_DEPTH`] levels, children visited in name order,
    /// so an archive carrying more than one candidate resolves to the shallowest and — among
    /// equals — the first by name rather than by `read_dir` order. The depth bound is what keeps a
    /// pathological archive from turning discovery into a full walk.
    fn locate_home(extracted: &Path) -> Result<PathBuf> {
        let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
        queue.push_back((extracted.to_path_buf(), 0));
        while let Some((directory, depth)) = queue.pop_front() {
            if JdkHome::is_home(&directory) {
                return Ok(directory);
            }
            if depth == Self::MAX_HOME_DEPTH {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            let mut children: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect();
            children.sort();
            for child in children {
                queue.push_back((child, depth + 1));
            }
        }
        bail!(
            "the archive holds no JDK home: no `bin/{}` was found under {}",
            JdkHome::java(),
            extracted.display()
        )
    }

    /// How deep below an archive root a JDK home may sit.
    ///
    /// Four covers every layout seen in the wild with room to spare: `<jdk>/bin/java` (depth 1),
    /// the macOS bundle's `<jdk>.jdk/Contents/Home/bin/java` (depth 3), and one wrapping directory
    /// a vendor might add above either.
    const MAX_HOME_DEPTH: usize = 4;
}
