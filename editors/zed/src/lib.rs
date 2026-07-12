//! Zed extension that runs the `jals` language server for Java files.
//!
//! The extension provides no grammar of its own: it attaches the `jals` language server to
//! Zed's `Java` language (install the "Java" extension for that grammar) and starts the
//! `jals` binary's stdio server — `jals lsp`, which is how [`jals-lsp`] speaks LSP. The binary
//! is resolved in order: the user-configured `lsp.jals.binary.path`, a `jals` on the worktree's
//! `$PATH`, and finally an **auto-downloaded** build — the newest per-platform `jals-<target>`
//! artifact CI uploads on every push to `main`. GitHub's own artifact-download endpoint
//! requires authentication even on a public repository, so the zip is fetched through
//! <https://nightly.link>, which serves public artifacts anonymously; downloads are cached in
//! the extension's working directory keyed by artifact id, and a cached copy is used when the
//! lookup fails (offline, rate-limited). Every part of the command (path, arguments, and
//! environment) can be overridden through the `lsp.jals.binary` setting in Zed.
//!
//! The resolution machinery is split by responsibility: [`platform`] maps the host OS/arch to
//! artifact/binary names, [`github`] lists artifacts via the GitHub API, [`cache`] owns the
//! on-disk download cache, and [`binary`] ties them together into a [`binary::BinaryResolver`].

mod binary;
mod cache;
mod github;
mod platform;

use zed_extension_api::{
    self as zed, serde_json, settings::LspSettings, Command, LanguageServerId, Result, Worktree,
};

use crate::binary::BinaryResolver;

/// The `jals` executable (built from the `jals-cli` crate).
pub(crate) const JALS_BINARY: &str = "jals";

/// The GitHub repository whose CI uploads the per-platform `jals` binary artifacts.
pub(crate) const GITHUB_REPO: &str = "topi-banana/jals";

struct JalsExtension {
    /// Resolves the `jals` binary and remembers the auto-downloaded build for the session.
    resolver: BinaryResolver,
}

impl zed::Extension for JalsExtension {
    fn new() -> Self {
        Self {
            resolver: BinaryResolver::new(),
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let settings =
            LspSettings::for_worktree(language_server_id.as_ref(), worktree).unwrap_or_default();
        let (path, arguments, env) = settings
            .binary
            .map(|binary| (binary.path, binary.arguments, binary.env))
            .unwrap_or_default();

        let command = self.resolver.resolve(path, language_server_id, worktree)?;

        // A configured `binary.arguments` replaces the default `["lsp"]` (the subcommand that
        // starts the stdio server) wholesale, so an override is free to pass extra flags (or a
        // different subcommand).
        let args = arguments.unwrap_or_else(|| vec!["lsp".to_owned()]);

        // A configured `binary.env` replaces the inherited shell environment; otherwise the
        // language server inherits the worktree's shell env (so `jals` can find the JDK, etc.).
        let env = env.map_or_else(|| worktree.shell_env(), |env| env.into_iter().collect());

        Ok(Command { command, args, env })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        // Forward the user's `lsp.jals.initialization_options` verbatim, if any.
        Ok(
            LspSettings::for_worktree(language_server_id.as_ref(), worktree)
                .unwrap_or_default()
                .initialization_options,
        )
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        // Forward the user's `lsp.jals.settings` verbatim, if any.
        Ok(
            LspSettings::for_worktree(language_server_id.as_ref(), worktree)
                .unwrap_or_default()
                .settings,
        )
    }
}

zed::register_extension!(JalsExtension);
