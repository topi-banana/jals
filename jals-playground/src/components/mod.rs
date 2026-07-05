//! The playground's UI, split by responsibility into struct-based Yew components.
//!
//! Every component here is a `struct` plus an `impl yew::Component` block (never a
//! `#[function_component]`): [`Header`] (the top action bar, with the CORS-proxy input and the
//! dependency-resolution status), [`FileTree`] (the config-files + workspace-files sidebar),
//! [`EditorPane`] (the Monaco editor mount + lifecycle), and [`SyntaxPane`] (the CST dump). The
//! `jals.toml` / `jalsfmt.toml` configuration is edited as TOML files in the editor itself (see
//! [`crate::app::App`]), so there are no dedicated config/dependency bars. The root
//! [`crate::app::App`] owns all state and wires the components together with props and callbacks.

mod editor_pane;
mod file_tree;
mod header;
mod syntax_pane;

pub use editor_pane::EditorPane;
pub use file_tree::{FileTree, TreeEntry};
pub use header::Header;
pub use syntax_pane::SyntaxPane;

/// Shared class list for a pane's small uppercase-mono header label (Files / active file /
/// Syntax tree). Kept in one place so every pane's chrome stays visually identical.
pub(crate) const PANE_LABEL: &str = "border-b border-hairline bg-canvas px-4 py-2 font-mono text-xs font-medium uppercase tracking-wider text-mute";
