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
use yew::prelude::*;

use crate::components::{EditorPane, FileTree, Header, SettingsBar, SyntaxPane, TreeEntry};
use crate::line_index::LineIndex;
use crate::monaco;
use crate::workspace::Workspace;

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
    /// Recompute and apply the inline diagnostics (e.g. once the editor is ready).
    Refresh,
    /// Replace the formatter config (sent by the settings bar).
    SetConfig(Config),
}

/// The playground's root component. Owns every piece of state; the children are presentational.
pub struct App {
    /// The in-memory multi-file workspace; the active file backs the editor.
    workspace: Workspace,
    /// The formatter configuration — the settings bar's source of truth. Shared behind an
    /// `Rc<RefCell<…>>` so the once-registered Monaco *Format Document* provider (created in
    /// [`EditorPane`]) reads the latest settings without a second synced copy.
    config: Rc<RefCell<Config>>,
    /// The most recent syntax-tree dump shown in the right pane, if any.
    syntax_dump: Option<String>,
}

impl App {
    /// Recompute the active file's diagnostics and push them to Monaco as inline markers, mapping
    /// each byte range to Monaco's UTF-16 coordinates. The `js_sys` marshalling lives behind
    /// [`monaco::set_diagnostics`].
    fn refresh_markers(&self) {
        let (source, diags) = self.workspace.analyze_active(&jals_lint::Config::default());
        let index = LineIndex::new(&source);
        monaco::set_diagnostics(diags.iter().map(|d| {
            let (sl, sc, el, ec) = index.to_monaco(&source, &d.range);
            monaco::Marker {
                start_line: sl,
                start_col: sc,
                end_line: el,
                end_col: ec,
                message: &d.message,
                severity: d.severity,
            }
        }));
    }

    /// Flush Monaco's live buffer into the active file's `fs` mirror. Monaco owns the live text
    /// (the `fs` copy lags by the edit debounce), so any handler about to read `active_source()`
    /// must flush first.
    fn flush_editor(&mut self) {
        self.workspace.edit_active(&monaco::current_value());
    }

    /// Refresh the right pane's syntax-tree dump from the active file.
    fn dump_syntax(&mut self) {
        let parse = self.workspace.syntax_active();
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
        for path in self.workspace.read_dir(dir) {
            let name = jals_fs::path::file_name(&path)
                .map(str::to_string)
                .unwrap_or_else(|| path.clone());
            let is_dir = self.workspace.is_dir(&path);
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
            workspace: Workspace::new(),
            config: Rc::new(RefCell::new(Config::default())),
            syntax_dump: None,
        }
    }

    fn update(&mut self, _ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            // Sync the workspace and repaint markers imperatively — no Yew re-render needed, since
            // the editor holds its own text and the markers go straight to Monaco.
            Msg::EditorChanged(value) => {
                self.workspace.edit_active(&value);
                self.refresh_markers();
                false
            }
            Msg::SelectFile(path) => {
                // Flush the live editor text into the (still-active) file before switching.
                self.flush_editor();
                self.workspace.set_active(&path);
                let src = self.workspace.active_source();
                monaco::switch_model(&path, &src);
                self.refresh_syntax_if_shown();
                self.refresh_markers();
                true
            }
            Msg::Format => {
                self.flush_editor();
                let formatted = self
                    .workspace
                    .format_active(&self.config.borrow())
                    .formatted;
                monaco::update_model(&formatted);
                self.workspace.edit_active(&formatted);
                let rerender = self.refresh_syntax_if_shown();
                self.refresh_markers();
                rerender
            }
            Msg::Syntax => {
                self.flush_editor();
                self.dump_syntax();
                true
            }
            Msg::Refresh => {
                self.refresh_markers();
                false
            }
            Msg::SetConfig(config) => {
                *self.config.borrow_mut() = config;
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let active = self.workspace.active().to_string();
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
                <div class="flex min-h-0 flex-1">
                    <FileTree
                        entries={self.tree_entries()}
                        active={active.clone()}
                        on_select={link.callback(Msg::SelectFile)}
                    />
                    <main class="grid min-h-0 flex-1 grid-cols-1 md:grid-cols-2">
                        <EditorPane
                            path={active}
                            source={self.workspace.active_source()}
                            on_change={link.callback(Msg::EditorChanged)}
                            on_ready={link.callback(|_| Msg::Refresh)}
                            config={self.config.clone()}
                        />
                        <SyntaxPane dump={self.syntax_dump.clone()} />
                    </main>
                </div>
            </div>
        }
    }
}
