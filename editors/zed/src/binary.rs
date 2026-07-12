//! Resolving the `jals` executable for the language-server command. Ties the [`Platform`],
//! [`Github`], and [`ArtifactCache`] pieces together: a configured path or a `$PATH` hit wins
//! outright, and only otherwise is the latest CI build looked up and downloaded (once per
//! session, then remembered on this resolver).

use zed_extension_api::{
    self as zed, LanguageServerId, LanguageServerInstallationStatus, Result, Worktree,
};

use crate::{cache::ArtifactCache, github::Github, platform::Platform, JALS_BINARY};

/// Resolves the `jals` binary path, remembering the auto-downloaded build for the session.
pub(crate) struct BinaryResolver {
    /// The auto-downloaded binary resolved earlier in this session, so the GitHub API is
    /// queried at most once per Zed session (a configured path or a `$PATH` hit never gets
    /// this far).
    cached_binary_path: Option<String>,
}

impl BinaryResolver {
    pub(crate) fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    /// Resolves the `jals` executable: the user-configured `lsp.jals.binary.path` when set,
    /// otherwise a `jals` found on the worktree's `$PATH`, otherwise the latest CI-built
    /// binary, downloaded (and cached) from the repository's GitHub Actions artifacts.
    pub(crate) fn resolve(
        &mut self,
        configured: Option<String>,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<String> {
        if let Some(path) = configured {
            return Ok(path);
        }
        if let Some(path) = worktree.which(JALS_BINARY) {
            return Ok(path);
        }
        if let Some(path) = &self.cached_binary_path {
            if ArtifactCache::is_file(path) {
                return Ok(path.clone());
            }
        }

        let path = Self::download_latest_artifact(language_server_id)?;
        self.cached_binary_path = Some(path.clone());
        Ok(path)
    }

    /// Downloads (or reuses) the newest `jals` binary CI uploaded for this platform, returning
    /// its path relative to the extension's working directory. When the artifact lookup or the
    /// download fails, falls back to the most recently downloaded binary already on disk.
    fn download_latest_artifact(language_server_id: &LanguageServerId) -> Result<String> {
        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let artifact_name = Platform::artifact_name()?;
        let downloaded = Github::latest_artifact_id(&artifact_name)
            .and_then(|artifact_id| ArtifactCache::fetch_artifact(language_server_id, artifact_id));
        match downloaded {
            Ok(path) => Ok(path),
            Err(err) => ArtifactCache::newest_cached_binary().ok_or_else(|| {
                format!(
                    "failed to fetch the latest `{artifact_name}` CI artifact ({err}), and no \
                     previously downloaded binary is cached. Install `jals` yourself (e.g. \
                     `cargo install --path jals-cli`) or set `lsp.jals.binary.path` in your \
                     Zed settings."
                )
            }),
        }
    }
}
