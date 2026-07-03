//! `jals-playground`: a browser playground for the `jals` formatter and workspace, built on the
//! Monaco Editor.
//!
//! A file tree on the left holds several Java files (an in-memory workspace); pick one to edit it
//! in the center pane — a Monaco editor with Java syntax highlighting. Diagnostics (syntax errors,
//! lint findings, cross-file type mismatches and unresolved types) are recomputed as you type and
//! shown inline as Monaco markers. The settings bar under the header configures the `jals-fmt`
//! [`Config`]; the top-right *Format* button (and Monaco's *Format Document* action) rewrites the
//! buffer in place, and *Syntax tree* dumps the lossless CST into the right pane. Everything runs
//! in the browser via `wasm32`; there is no server round-trip.

mod line_index;
mod workspace;

use std::cell::RefCell;
use std::rc::Rc;

use jals_fmt::{Config, IndentStyle, LineEnding};
use jals_lint::Severity;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

use line_index::LineIndex;
use workspace::Workspace;

// The Monaco glue (see `js/monaco_glue.js`), pulled in as a wasm-bindgen snippet.
#[wasm_bindgen(module = "/js/monaco_glue.js")]
extern "C" {
    /// Resolve once Monaco's editor API has loaded.
    #[wasm_bindgen(js_name = initMonaco)]
    fn init_monaco() -> js_sys::Promise;

    /// Create the editor in `el`, showing `path`'s model, calling `on_change` (debounced) on edits.
    #[wasm_bindgen(js_name = createEditor)]
    fn create_editor(el: &web_sys::Element, path: &str, value: &str, on_change: &js_sys::Function);

    /// Switch the editor to `path`'s model (created from `value` if new).
    #[wasm_bindgen(js_name = switchModel)]
    fn switch_model(path: &str, value: &str);

    /// Replace the current model's text (as an undoable edit).
    #[wasm_bindgen(js_name = updateModel)]
    fn update_model(value: &str);

    /// The live text currently in the editor.
    #[wasm_bindgen(js_name = currentValue)]
    fn current_value() -> String;

    /// Build one marker object for [`set_markers`].
    #[wasm_bindgen(js_name = marker)]
    fn marker(
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
        message: &str,
        severity: u32,
    ) -> JsValue;

    /// Replace the diagnostic markers on the current model.
    #[wasm_bindgen(js_name = setMarkers)]
    fn set_markers(markers: &js_sys::Array);

    /// Register the Java document-formatting provider, calling `format` for the text.
    #[wasm_bindgen(js_name = registerFormatter)]
    fn register_formatter(format: &js_sys::Function);
}

/// Map a lint [`Severity`] to a Monaco `MarkerSeverity` (Error = 8, Warning = 4, Hint = 1).
fn marker_severity(severity: Severity) -> u32 {
    match severity {
        Severity::Error => 8,
        Severity::Warn => 4,
        Severity::Allow => 1,
    }
}

enum Msg {
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

/// Shared base classes for the toolbar buttons.
const BTN_BASE: &str = "inline-flex h-9 cursor-pointer items-center rounded-md px-3 text-sm font-medium transition-colors";

struct App {
    /// The in-memory multi-file workspace; the active file backs the editor.
    workspace: Workspace,
    /// The formatter configuration — the settings bar's source of truth. Shared behind an
    /// `Rc<RefCell<…>>` so the once-registered Monaco *Format Document* provider (created in
    /// [`Component::rendered`]) reads the latest settings without a second synced copy.
    config: Rc<RefCell<Config>>,
    /// Container the Monaco editor is mounted into.
    editor_ref: NodeRef,
    /// The most recent syntax-tree dump shown in the right pane, if any.
    syntax_dump: Option<String>,
}

impl App {
    /// Recompute the active file's diagnostics and push them to Monaco as inline markers.
    fn refresh_markers(&self) {
        let (source, diags) = self.workspace.analyze_active(&jals_lint::Config::default());
        let index = LineIndex::new(&source);
        let markers = js_sys::Array::new();
        for d in &diags {
            let (sl, sc, el, ec) = index.to_monaco(&source, &d.range);
            markers.push(&marker(
                sl,
                sc,
                el,
                ec,
                &d.message,
                marker_severity(d.severity),
            ));
        }
        set_markers(&markers);
    }

    /// Flush Monaco's live buffer into the active file's `fs` mirror. Monaco owns the live text
    /// (the `fs` copy lags by the edit debounce), so any handler about to read `active_source()`
    /// must flush first.
    fn flush_editor(&mut self) {
        self.workspace.edit_active(&current_value());
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

    /// The file-tree sidebar: the workspace's files rendered as a fully-expanded tree, each file
    /// row selecting the active file. The active row carries the app-shell left-edge indicator.
    fn view_sidebar(&self, ctx: &Context<Self>) -> Html {
        html! { <div class="py-1">{ self.view_tree(ctx, "", 0) }</div> }
    }

    /// The children of directory `dir`, each rendered at indentation `depth`.
    fn view_tree(&self, ctx: &Context<Self>, dir: &str, depth: usize) -> Html {
        html! {
            { for self.workspace.read_dir(dir).into_iter().map(|path| self.view_entry(ctx, path, depth)) }
        }
    }

    /// A single tree row: a directory label (recursing into its children) or a clickable file.
    fn view_entry(&self, ctx: &Context<Self>, path: String, depth: usize) -> Html {
        let name = jals_fs::path::file_name(&path)
            .map(str::to_string)
            .unwrap_or_else(|| path.clone());
        let pad = format!("padding-left: {}px", 12 + depth * 14);
        if self.workspace.is_dir(&path) {
            html! {
                <div>
                    <div class="flex items-center gap-1 py-1 font-mono text-xs text-mute" style={pad}>
                        <span>{ "▸" }</span>
                        <span>{ name }</span>
                    </div>
                    { self.view_tree(ctx, &path, depth + 1) }
                </div>
            }
        } else {
            let is_active = path == self.workspace.active();
            let onclick = {
                let path = path.clone();
                ctx.link().callback(move |_| Msg::SelectFile(path.clone()))
            };
            let base = "flex cursor-pointer items-center gap-1 py-1 font-mono text-xs";
            let state = if is_active {
                "border-l-2 border-ink bg-canvas-soft text-ink"
            } else {
                "border-l-2 border-transparent text-body hover:bg-canvas-soft"
            };
            html! {
                <div class={classes!(base, state)} style={pad} onclick={onclick}>
                    <span>{ name }</span>
                </div>
            }
        }
    }

    /// The formatter settings bar. Each control emits a fresh [`Config`] via
    /// [`Msg::SetConfig`]; `onchange` (not `oninput`) keeps the numeric fields stable while
    /// typing. Parse failures leave the field unchanged.
    fn view_settings(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let cfg = self.config.borrow().clone();

        // A change callback for one `usize` field, identified by a setter closure.
        let usize_cb = |set: fn(&mut Config, usize)| {
            let config = cfg.clone();
            link.callback(move |e: Event| {
                let el: HtmlInputElement = e.target_unchecked_into();
                let mut c = config.clone();
                if let Ok(value) = el.value().parse::<usize>() {
                    set(&mut c, value);
                }
                Msg::SetConfig(c)
            })
        };
        // A change callback for one `bool` field (a checkbox).
        let bool_cb = |set: fn(&mut Config, bool)| {
            let config = cfg.clone();
            link.callback(move |e: Event| {
                let el: HtmlInputElement = e.target_unchecked_into();
                let mut c = config.clone();
                set(&mut c, el.checked());
                Msg::SetConfig(c)
            })
        };
        // A change callback for one `<select>`, mapping its selected value to a field.
        let select_cb = |set: fn(&mut Config, String)| {
            let config = cfg.clone();
            link.callback(move |e: Event| {
                let el: HtmlSelectElement = e.target_unchecked_into();
                let mut c = config.clone();
                set(&mut c, el.value());
                Msg::SetConfig(c)
            })
        };

        let on_indent_style = select_cb(|c, v| {
            c.indent_style = if v == "tab" {
                IndentStyle::Tab
            } else {
                IndentStyle::Space
            };
        });
        let on_indent_width = usize_cb(|c, v| c.indent_width = v.max(1));
        let on_max_width = usize_cb(|c, v| c.max_width = v.max(1));
        let on_comment_width = usize_cb(|c, v| c.comment_width = v.max(1));
        let on_max_blank_lines = usize_cb(|c, v| c.max_blank_lines = v);
        let on_line_ending = select_cb(|c, v| {
            c.line_ending = if v == "crlf" {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            };
        });
        let on_final_newline = bool_cb(|c, v| c.insert_final_newline = v);
        let on_wrap_comments = bool_cb(|c, v| c.wrap_comments = v);

        let field = "flex items-center gap-1.5";
        let lbl = "font-mono text-xs text-mute";
        let num = "h-7 w-14 rounded-md border border-hairline bg-canvas px-2 text-xs text-ink outline-none";
        let sel =
            "h-7 rounded-md border border-hairline bg-canvas px-1 text-xs text-ink outline-none";

        html! {
            <div class="flex flex-wrap items-center gap-x-5 gap-y-2 border-b border-hairline bg-canvas-soft px-6 py-2">
                <label class={field}>
                    <span class={lbl}>{ "Indent" }</span>
                    <select class={sel} onchange={on_indent_style}>
                        <option value="space" selected={cfg.indent_style == IndentStyle::Space}>{ "Spaces" }</option>
                        <option value="tab" selected={cfg.indent_style == IndentStyle::Tab}>{ "Tabs" }</option>
                    </select>
                    <input class={num} type="number" min="1" value={cfg.indent_width.to_string()} onchange={on_indent_width} />
                </label>
                <label class={field}>
                    <span class={lbl}>{ "Max width" }</span>
                    <input class={num} type="number" min="1" value={cfg.max_width.to_string()} onchange={on_max_width} />
                </label>
                <label class="flex cursor-pointer items-center gap-1.5">
                    <input type="checkbox" class="h-3.5 w-3.5 accent-ink" checked={cfg.wrap_comments} onchange={on_wrap_comments} />
                    <span class={lbl}>{ "Wrap comments" }</span>
                </label>
                <label class={field}>
                    <span class={lbl}>{ "Comment width" }</span>
                    <input class={num} type="number" min="1" value={cfg.comment_width.to_string()} onchange={on_comment_width} />
                </label>
                <label class={field}>
                    <span class={lbl}>{ "Blank lines" }</span>
                    <input class={num} type="number" min="0" value={cfg.max_blank_lines.to_string()} onchange={on_max_blank_lines} />
                </label>
                <label class={field}>
                    <span class={lbl}>{ "Line ending" }</span>
                    <select class={sel} onchange={on_line_ending}>
                        <option value="lf" selected={cfg.line_ending == LineEnding::Lf}>{ "LF" }</option>
                        <option value="crlf" selected={cfg.line_ending == LineEnding::Crlf}>{ "CRLF" }</option>
                    </select>
                </label>
                <label class="flex cursor-pointer items-center gap-1.5">
                    <input type="checkbox" class="h-3.5 w-3.5 accent-ink" checked={cfg.insert_final_newline} onchange={on_final_newline} />
                    <span class={lbl}>{ "Final newline" }</span>
                </label>
            </div>
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
            editor_ref: NodeRef::default(),
            syntax_dump: None,
        }
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if !first_render {
            return;
        }
        let Some(el) = self.editor_ref.cast::<web_sys::Element>() else {
            return;
        };
        let path = self.workspace.active().to_string();
        let source = self.workspace.active_source();

        // Monaco notifies us (debounced, from JS) whenever the buffer changes.
        let link = ctx.link().clone();
        let on_change = Closure::<dyn FnMut(String)>::new(move |value: String| {
            link.send_message(Msg::EditorChanged(value));
        });

        // "Format Document" (Ctrl+Shift+I) formats with the current config.
        let config = self.config.clone();
        let formatter = Closure::<dyn FnMut(String) -> String>::new(move |text: String| {
            let cfg = config.borrow();
            jals_fmt::format_source(&text, &cfg).formatted
        });

        let refresh_link = ctx.link().clone();
        spawn_local(async move {
            JsFuture::from(init_monaco()).await.ok();
            create_editor(&el, &path, &source, on_change.as_ref().unchecked_ref());
            register_formatter(formatter.as_ref().unchecked_ref());
            // Keep the closures alive for the app's lifetime (a single editor instance).
            on_change.forget();
            formatter.forget();
            // Now that the editor exists, paint the initial diagnostics.
            refresh_link.send_message(Msg::Refresh);
        });
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
                switch_model(&path, &src);
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
                update_model(&formatted);
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
        let on_format = link.callback(|_| Msg::Format);
        let on_syntax = link.callback(|_| Msg::Syntax);

        let label = "border-b border-hairline bg-canvas px-4 py-2 font-mono text-xs font-medium uppercase tracking-wider text-mute";
        let mono_pane = "min-h-0 flex-1 overflow-auto whitespace-pre bg-canvas p-4 font-mono text-[13px] leading-5 text-ink outline-none";
        let secondary = classes!(
            BTN_BASE,
            "border",
            "border-hairline",
            "bg-canvas",
            "text-ink",
            "hover:bg-canvas-soft"
        );

        html! {
            <div class="flex h-screen flex-col bg-canvas-soft text-ink">
                <header class="flex h-16 shrink-0 items-center justify-between border-b border-hairline bg-canvas px-6">
                    <div class="flex items-baseline gap-2">
                        <span class="text-base font-semibold tracking-tight">{ "jals" }</span>
                        <span class="text-sm text-mute">{ "playground" }</span>
                    </div>
                    <div class="flex items-center gap-2">
                        <button onclick={on_syntax} class={secondary}>
                            { "Syntax tree" }
                        </button>
                        <button
                            onclick={on_format}
                            class={classes!(BTN_BASE, "bg-ink", "text-canvas", "hover:opacity-90")}
                        >
                            { "Format" }
                        </button>
                    </div>
                </header>
                { self.view_settings(ctx) }
                <div class="flex min-h-0 flex-1">
                    <aside class="flex w-60 shrink-0 flex-col overflow-auto border-r border-hairline bg-canvas">
                        <div class={label}>{ "Files" }</div>
                        { self.view_sidebar(ctx) }
                    </aside>
                    <main class="grid min-h-0 flex-1 grid-cols-1 md:grid-cols-2">
                        <section class="flex min-h-0 flex-col border-b border-hairline md:border-b-0 md:border-r">
                            <div class={label}>{ self.workspace.active() }</div>
                            <div ref={self.editor_ref.clone()} class="min-h-0 flex-1" />
                        </section>
                        <section class="flex min-h-0 flex-col">
                            <div class={label}>{ "Syntax tree" }</div>
                            if let Some(dump) = &self.syntax_dump {
                                <pre class={mono_pane}>{ dump }</pre>
                            } else {
                                <div class="min-h-0 flex-1 overflow-auto bg-canvas p-4 font-mono text-xs text-mute">
                                    { "Press “Syntax tree” to dump the lossless CST of the active file." }
                                </div>
                            }
                        </section>
                    </main>
                </div>
            </div>
        }
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
