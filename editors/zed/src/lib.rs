//! Zed extension that runs the `jals` language server for Java files.
//!
//! The extension provides no grammar of its own: it attaches the `jals` language server to
//! Zed's `Java` language (install the "Java" extension for that grammar) and starts the
//! `jals` binary's stdio server — `jals lsp`, which is how [`jals-lsp`] speaks LSP. The binary
//! is located on the worktree's `$PATH`; every part of the command (path, arguments, and
//! environment) can be overridden through the `lsp.jals.binary` setting in Zed.

use zed_extension_api::{
    self as zed, serde_json, settings::LspSettings, Command, LanguageServerId, Result, Worktree,
};

/// The `jals` executable (built from the `jals-cli` crate).
const JALS_BINARY: &str = "jals";

struct JalsExtension;

impl JalsExtension {
    /// Resolves the `jals` executable: the user-configured `lsp.jals.binary.path` when set,
    /// otherwise a `jals` found on the worktree's `$PATH`.
    fn resolve_binary_path(configured: Option<String>, worktree: &Worktree) -> Result<String> {
        if let Some(path) = configured {
            return Ok(path);
        }

        worktree.which(JALS_BINARY).ok_or_else(|| {
            format!(
                "`{JALS_BINARY}` was not found on your $PATH. Install it (e.g. \
                 `cargo install --path jals-cli` or `cargo binstall jals-cli`), or set \
                 `lsp.jals.binary.path` in your Zed settings."
            )
        })
    }
}

impl zed::Extension for JalsExtension {
    fn new() -> Self {
        Self
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

        let command = Self::resolve_binary_path(path, worktree)?;

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
