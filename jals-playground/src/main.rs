//! `jals-playground`: a browser playground for the `jals` formatter and workspace, built on the
//! Monaco Editor.
//!
//! A sidebar on the left holds editable `jals.toml`, `jalsfmt.toml`, and `build.rhai` files over an
//! in-memory workspace of Java files; pick one to edit it in the center pane — a Monaco editor with
//! Java syntax highlighting. Diagnostics (syntax errors, lint findings, cross-file type mismatches
//! and unresolved types) are recomputed as you type and shown inline as Monaco markers. Editing
//! `jalsfmt.toml` updates the `jals-fmt` [`Config`] used by the top-right *Format* button (and
//! Monaco's *Format Document* action, which rewrite the buffer in place), and editing `jals.toml`
//! runs its Rhai build script and re-resolves its `[dependencies]` (downloaded through the header's
//! CORS proxy). Generated Java is indexed immediately. The right pane has two tabs: *Syntax tree*
//! dumps the lossless CST, and *Build output* reports what the top-right *Build* button produced —
//! the whole workspace compiled in-process by `jals-javac` into a downloadable `.jar` or a
//! WebAssembly module, whichever `[build] backend` selects. Everything runs in the browser via
//! `wasm32`; there is no server round-trip, and no compiler subprocess either.
//!
//! The UI is split by responsibility into struct-based Yew components (see [`components`]), all
//! wired together by the stateful root [`app::App`]:
//! - [`app`] — the root component; owns the workspace, config buffers, and build results.
//! - [`components`] — the presentational pieces (header, file tree, editor, result pane).
//! - [`compile`] — the frontend/backend pipeline behind *Build*; host-testable, DOM-free.
//! - [`download`] — the browser-only shim that saves a compiled artifact.
//! - [`monaco`] — the typed Rust bridge to the single imperative Monaco editor instance.
//! - [`workspace`] — the thin Monaco adapter over the shared `jals-editor` core.
//! - [`providers`] — wires each Monaco language-feature provider to the workspace analysis.
//! - [`host`] — the Monaco `EditorHost`: coordinate mapping and the provider payload shapes.
//!
//! [`Config`]: jals_config::fmt::Config

mod app;
mod compile;
mod components;
mod download;
mod fetcher;
mod host;
mod monaco;
mod providers;
mod workspace;

use app::App;

fn main() {
    yew::Renderer::<App>::new().render();
}
