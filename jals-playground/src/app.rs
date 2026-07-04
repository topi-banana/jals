//! The root [`App`] component: it owns all playground state and orchestrates the UI.
//!
//! `App` holds the in-memory [`Workspace`], the shared formatter [`Config`], and the current
//! syntax-tree dump, and wires the responsibility-split child components ([`Header`],
//! [`SettingsBar`], [`FileTree`], [`EditorPane`], [`SyntaxPane`]) together with props and
//! callbacks. Editor *content* operations (switching files, applying a format, repainting
//! diagnostics) are driven imperatively against the single Monaco instance through the
//! [`crate::monaco`] service; the child components stay presentational.

use std::cell::RefCell;
use std::rc::Rc;

use jals_fmt::Config;
use jals_fs::InMemoryFileTree;
use jals_hir::{LoweredClasspath, ProjectIndex};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::components::{
    DepsBar, EditorPane, FileTree, Header, SettingsBar, SyntaxPane, TreeEntry,
};
use crate::fetcher::BrowserFetcher;
use crate::workspace::Workspace;
use crate::{monaco, providers};

/// A message driving an [`App`] state transition.
pub enum Msg {
    /// The editor buffer changed (debounced; edits the active file).
    EditorChanged(String),
    /// Switch the active file (clicked in the file tree).
    SelectFile(String),
    /// Format the active file in place.
    Format,
    /// Dump the active file's syntax tree into the right pane.
    Syntax,
    /// The editor exists: register the language-feature providers and paint the initial markers.
    EditorReady,
    /// A cross-file navigation switched the editor's model to `path`; track it as the active file.
    ModelOpened(String),
    /// Replace the formatter config (sent by the settings bar).
    SetConfig(Config),
    /// Resolve external `[dependencies]` (the TOML) with an optional CORS proxy — spawns the async
    /// download/parse and reports back via [`Msg::ClasspathResolved`].
    ResolveDependencies(String, String),
    /// The async dependency resolution finished: the lowered classpath + a status line, or an error.
    ClasspathResolved(Result<(LoweredClasspath, String), String>),
}

/// The playground's root component. Owns every piece of state; the children are presentational.
pub struct App {
    /// The in-memory multi-file workspace; the active file backs the editor. Shared behind an
    /// `Rc<RefCell<…>>` so the once-registered Monaco language-feature providers (registered in
    /// [`Msg::EditorReady`]) can analyse it without a second synced copy.
    workspace: Rc<RefCell<Workspace>>,
    /// The formatter configuration — the settings bar's source of truth. Shared behind an
    /// `Rc<RefCell<…>>` so the once-registered Monaco *Format Document* provider (created in
    /// [`EditorPane`]) reads the latest settings without a second synced copy.
    config: Rc<RefCell<Config>>,
    /// The most recent syntax-tree dump shown in the right pane, if any.
    syntax_dump: Option<String>,
    /// The in-memory dependency cache — the browser's equivalent of `target/jals/deps`. Downloaded
    /// jars are written here and reused across resolves (skip-if-exists), shared behind an
    /// `Rc<RefCell<…>>` so the async resolve task can snapshot and repopulate it without holding a
    /// borrow across an `.await`.
    deps_cache: Rc<RefCell<InMemoryFileTree>>,
    /// The last dependency-resolution status line shown in the [`DepsBar`], if any.
    deps_status: Option<String>,
}

impl App {
    /// Recompute the active file's diagnostics and push them to Monaco as inline markers. The
    /// workspace already maps each range to Monaco's UTF-16 coordinates, so this only marshals
    /// through [`monaco::set_diagnostics`].
    fn refresh_markers(&self) {
        let diags = self
            .workspace
            .borrow()
            .analyze_active(&jals_lint::Config::default());
        monaco::set_diagnostics(diags.iter().map(|d| monaco::Marker {
            start_line: d.range.start_line,
            start_col: d.range.start_col,
            end_line: d.range.end_line,
            end_col: d.range.end_col,
            message: &d.message,
            severity: d.severity,
        }));
    }

    /// Flush Monaco's live buffer into the active file's `fs` mirror. Monaco owns the live text
    /// (the `fs` copy lags by the edit debounce), so any handler about to read `active_source()`
    /// must flush first.
    fn flush_editor(&mut self) {
        self.workspace
            .borrow_mut()
            .edit_active(&monaco::current_value());
    }

    /// Refresh the right pane's syntax-tree dump from the active file.
    fn dump_syntax(&mut self) {
        let parse = self.workspace.borrow().syntax_active();
        self.syntax_dump = Some(format!("{:#?}", parse.syntax()));
    }

    /// If the syntax pane is currently shown, re-dump it from the active file. Returns whether it
    /// was refreshed (i.e. whether a re-render is needed to show the new dump).
    fn refresh_syntax_if_shown(&mut self) -> bool {
        if self.syntax_dump.is_some() {
            self.dump_syntax();
            true
        } else {
            false
        }
    }

    /// Flatten the workspace's files into a pre-order [`TreeEntry`] list for the [`FileTree`].
    fn tree_entries(&self) -> Vec<TreeEntry> {
        let mut out = Vec::new();
        self.collect_entries("", 0, &mut out);
        out
    }

    /// Append the children of directory `dir` (each at indentation `depth`) to `out`, recursing
    /// into subdirectories so the whole tree is flattened in pre-order.
    fn collect_entries(&self, dir: &str, depth: usize, out: &mut Vec<TreeEntry>) {
        for path in self.workspace.borrow().read_dir(dir) {
            let name = jals_fs::path::file_name(&path)
                .map(str::to_string)
                .unwrap_or_else(|| path.clone());
            let is_dir = self.workspace.borrow().is_dir(&path);
            out.push(TreeEntry {
                path: path.clone(),
                name,
                depth,
                is_dir,
            });
            if is_dir {
                self.collect_entries(&path, depth + 1, out);
            }
        }
    }
}

impl Component for App {
    type Message = Msg;
    type Properties = ();

    fn create(_ctx: &Context<Self>) -> Self {
        App {
            workspace: Rc::new(RefCell::new(Workspace::new())),
            config: Rc::new(RefCell::new(Config::default())),
            syntax_dump: None,
            deps_cache: Rc::new(RefCell::new(InMemoryFileTree::new())),
            deps_status: None,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            // Sync the workspace and repaint markers imperatively — no Yew re-render needed, since
            // the editor holds its own text and the markers go straight to Monaco.
            Msg::EditorChanged(value) => {
                self.workspace.borrow_mut().edit_active(&value);
                self.refresh_markers();
                false
            }
            Msg::SelectFile(path) => {
                // Flush the live editor text into the (still-active) file before switching.
                self.flush_editor();
                self.workspace.borrow_mut().set_active(&path);
                let src = self.workspace.borrow().active_source();
                monaco::switch_model(&path, &src);
                self.refresh_syntax_if_shown();
                self.refresh_markers();
                true
            }
            Msg::Format => {
                self.flush_editor();
                let formatted = self
                    .workspace
                    .borrow()
                    .format_active(&self.config.borrow())
                    .formatted;
                monaco::update_model(&formatted);
                self.workspace.borrow_mut().edit_active(&formatted);
                let rerender = self.refresh_syntax_if_shown();
                self.refresh_markers();
                rerender
            }
            Msg::Syntax => {
                self.flush_editor();
                self.dump_syntax();
                true
            }
            Msg::EditorReady => {
                // Eagerly create a URI-backed model for every file, so cross-file navigation and
                // peek-references can reach files never opened in the editor.
                let files = js_sys::Array::new();
                for (path, text) in self.workspace.borrow().file_texts() {
                    files.push(&js_sys::Array::of2(
                        &JsValue::from(path),
                        &JsValue::from(text),
                    ));
                }
                monaco::create_models(&files);
                // Register the language-feature providers, backed by the shared workspace.
                providers::install(self.workspace.clone());
                self.refresh_markers();
                false
            }
            Msg::ModelOpened(path) => {
                // Monaco already switched the model (and flushed the outgoing file via `on_change`);
                // only track the new active file and repaint. Must not flush or `switch_model` again.
                self.workspace.borrow_mut().set_active(&path);
                self.refresh_syntax_if_shown();
                self.refresh_markers();
                true
            }
            Msg::SetConfig(config) => {
                *self.config.borrow_mut() = config;
                true
            }
            Msg::ResolveDependencies(toml, proxy) => {
                self.deps_status = Some("resolving…".to_string());
                let cache = self.deps_cache.clone();
                let link = ctx.link().clone();
                spawn_local(async move {
                    let result = resolve_classpath(toml, proxy, cache).await;
                    link.send_message(Msg::ClasspathResolved(result));
                });
                true
            }
            Msg::ClasspathResolved(result) => {
                match result {
                    Ok((classpath, status)) => {
                        self.workspace.borrow_mut().set_classpath(Some(classpath));
                        self.deps_status = Some(status);
                        // Re-analyse the active file with the external types now in the index.
                        self.refresh_markers();
                    }
                    Err(err) => self.deps_status = Some(format!("error: {err}")),
                }
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let active = self.workspace.borrow().active().to_string();
        let source = self.workspace.borrow().active_source();
        html! {
            <div class="flex h-screen flex-col bg-canvas-soft text-ink">
                <Header
                    on_format={link.callback(|_| Msg::Format)}
                    on_syntax={link.callback(|_| Msg::Syntax)}
                />
                <SettingsBar
                    config={self.config.borrow().clone()}
                    on_change={link.callback(Msg::SetConfig)}
                />
                <DepsBar
                    status={self.deps_status.clone()}
                    on_resolve={link.callback(|(toml, proxy)| Msg::ResolveDependencies(toml, proxy))}
                />
                <div class="flex min-h-0 flex-1">
                    <FileTree
                        entries={self.tree_entries()}
                        active={active.clone()}
                        on_select={link.callback(Msg::SelectFile)}
                    />
                    <main class="grid min-h-0 flex-1 grid-cols-1 md:grid-cols-2">
                        <EditorPane
                            path={active}
                            source={source}
                            on_change={link.callback(Msg::EditorChanged)}
                            on_ready={link.callback(|_| Msg::EditorReady)}
                            on_open={link.callback(Msg::ModelOpened)}
                            config={self.config.clone()}
                        />
                        <SyntaxPane dump={self.syntax_dump.clone()} />
                    </main>
                </div>
            </div>
        }
    }
}

/// Resolve a `[dependencies]` TOML into a lowered classpath, in the browser: parse the manifest,
/// download each remote jar with a [`BrowserFetcher`] into the in-memory `cache`, load the `.class`
/// files, and lower them for the project index. Returns the classpath and a human-readable status
/// line (class/jar counts plus any warnings), or an error message.
///
/// The whole resolution runs against a *snapshot clone* of `cache` so no `RefCell` borrow is held
/// across an `.await`; the populated snapshot is written back afterwards, so a re-resolve reuses the
/// already-downloaded jars (skip-if-exists) — the browser's `target/jals/deps`.
async fn resolve_classpath(
    toml: String,
    proxy: String,
    cache: Rc<RefCell<InMemoryFileTree>>,
) -> Result<(LoweredClasspath, String), String> {
    let manifest = toml
        .parse::<jals_build::Manifest>()
        .map_err(|e| e.to_string())?;
    let fetcher = BrowserFetcher::new(proxy);
    let mut snapshot = cache.borrow().clone();
    let mut warnings = Vec::new();
    // A synthetic root: remote (`https://`) jars ignore it; local `file://`/`path` jars are not
    // reachable in the browser anyway.
    let jars = jals_classpath::resolve_project_dependencies_in(
        &fetcher,
        &mut snapshot,
        &manifest,
        "/",
        |message| warnings.push(message),
    )
    .await;
    let load = jals_classpath::load_classpath_in(&snapshot, &jars);
    *cache.borrow_mut() = snapshot;
    for warning in &load.warnings {
        warnings.push(format!("{}: {}", warning.path, warning.message));
    }
    let classpath = ProjectIndex::lower_classpath(&load.classes);
    let mut status = format!(
        "resolved {} class(es) from {} jar(s)",
        load.classes.len(),
        jars.len()
    );
    if !warnings.is_empty() {
        status.push_str(&format!(
            " — {} warning(s): {}",
            warnings.len(),
            warnings.join("; ")
        ));
    }
    Ok((classpath, status))
}
