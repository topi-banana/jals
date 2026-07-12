//! The on-disk cache of downloaded `jals` binary artifacts, under the extension's working
//! directory. Each artifact is unpacked into its own `jals-artifact-<id>` version directory;
//! this module downloads into them, prunes the stale ones, and locates the newest cached binary
//! for the offline fallback.

use std::fs;

use zed_extension_api::{
    self as zed, DownloadedFileType, LanguageServerId, LanguageServerInstallationStatus, Result,
};

use crate::{platform::Platform, GITHUB_REPO};

/// Prefix of the directories a downloaded artifact is unpacked into, inside the extension's
/// working directory: `jals-artifact-<artifact id>`. Artifact ids increase monotonically, so
/// the largest suffix is the newest download.
const VERSION_DIR_PREFIX: &str = "jals-artifact-";

/// The on-disk cache of downloaded artifacts.
pub(crate) struct ArtifactCache;

impl ArtifactCache {
    /// Downloads the given artifact into its version directory (skipped when already present)
    /// and returns the binary's path.
    pub(crate) fn fetch_artifact(
        language_server_id: &LanguageServerId,
        artifact_id: u64,
    ) -> Result<String> {
        let version_dir = format!("{VERSION_DIR_PREFIX}{artifact_id}");
        let binary_path = format!("{version_dir}/{}", Platform::binary_file_name());

        if !Self::is_file(&binary_path) {
            zed::set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::Downloading,
            );

            // GitHub's artifact-download endpoint requires authentication even on a public
            // repository, so the zip is fetched through nightly.link, a proxy that serves
            // public artifacts anonymously (no repo-side setup needed for public repos).
            let url =
                format!("https://nightly.link/{GITHUB_REPO}/actions/artifacts/{artifact_id}.zip");
            zed::download_file(&url, &version_dir, DownloadedFileType::Zip)
                .map_err(|err| format!("downloading `{url}` failed: {err}"))?;

            // The artifact zip does not preserve the executable bit.
            if Platform::needs_executable_bit() {
                zed::make_file_executable(&binary_path)?;
            }

            Self::remove_stale_version_dirs(&version_dir);
        }

        Ok(binary_path)
    }

    /// The newest already-downloaded binary on disk, if any — the offline fallback.
    pub(crate) fn newest_cached_binary() -> Option<String> {
        let file_name = Platform::binary_file_name();
        Self::version_dirs()
            .map(|(dir_name, artifact_id)| (artifact_id, format!("{dir_name}/{file_name}")))
            .filter(|(_, path)| Self::is_file(path))
            .max_by_key(|(artifact_id, _)| *artifact_id)
            .map(|(_, path)| path)
    }

    /// Whether `path` names an existing regular file.
    pub(crate) fn is_file(path: &str) -> bool {
        fs::metadata(path).is_ok_and(|meta| meta.is_file())
    }

    /// Iterates the `jals-artifact-<id>` version directories in the extension's working
    /// directory, yielding each as `(dir_name, artifact_id)`; entries without the prefix or
    /// with a non-numeric suffix are skipped.
    fn version_dirs() -> impl Iterator<Item = (String, u64)> {
        fs::read_dir(".")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let dir_name = entry.file_name().to_str()?.to_owned();
                let artifact_id = dir_name
                    .strip_prefix(VERSION_DIR_PREFIX)?
                    .parse::<u64>()
                    .ok()?;
                Some((dir_name, artifact_id))
            })
    }

    /// Removes every downloaded version directory except the current one.
    fn remove_stale_version_dirs(current_dir: &str) {
        for (dir_name, _) in Self::version_dirs() {
            if dir_name != current_dir {
                fs::remove_dir_all(&dir_name).ok();
            }
        }
    }
}
