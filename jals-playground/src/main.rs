//! `jals-playground`: a browser playground for the `jals` formatter, syntax tree, and workspace.
//!
//! A file tree on the left holds several Java files (an in-memory workspace); pick one to edit it
//! in the center pane, then run the formatter, dump the lossless syntax tree, or lint it across the
//! whole workspace with the buttons in the top-right. The settings bar under the header configures
//! the `jals-fmt` [`Config`]; while formatted output is showing, changing a setting re-formats
//! live. Everything runs in the browser via `wasm32`; there is no server round-trip.

mod workspace;

use jals_fmt::{Config, IndentStyle, LineEnding};
use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

use workspace::Workspace;

/// Which tool produced the current output.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// `jals-fmt` pretty-printer output.
    Format,
    /// `jals-syntax` lossless CST dump.
    Syntax,
    /// `jals-lint` + `jals-hir` cross-file analysis over the whole workspace.
    Lint,
}

impl Mode {
    /// Label shown above the output pane.
    fn output_title(self) -> &'static str {
        match self {
            Mode::Format => "Formatted",
            Mode::Syntax => "Syntax tree",
            Mode::Lint => "Diagnostics",
        }
    }
}

enum Msg {
    /// The editor textarea changed (edits the active file).
    Input(String),
    /// Switch the active file (clicked in the file tree).
    SelectFile(String),
    /// Run a tool over the active file.
    Run(Mode),
    /// Replace the formatter config (sent by the settings bar).
    SetConfig(Config),
}

/// Shared base classes for the toolbar buttons.
const BTN_BASE: &str = "inline-flex h-9 cursor-pointer items-center rounded-md px-3 text-sm font-medium transition-colors";

struct App {
    /// The in-memory multi-file workspace; the active file backs the editor.
    workspace: Workspace,
    output: String,
    /// Human-readable diagnostics: formatter warnings or parse errors.
    diagnostics: Vec<String>,
    /// The tool that produced `output`, once one has run.
    mode: Option<Mode>,
    /// Current formatter configuration.
    config: Config,
}

impl App {
    /// Run `mode` over the active file, refreshing `output` and `diagnostics`.
    fn run(&mut self, mode: Mode) {
        match mode {
            Mode::Format => {
                let out = self.workspace.format_active(&self.config);
                self.output = out.formatted;
                self.diagnostics = out
                    .warnings
                    .iter()
                    .map(|w| format!("{}..{}  {}", w.range.start, w.range.end, w.message))
                    .collect();
            }
            Mode::Syntax => {
                let parse = self.workspace.syntax_active();
                self.output = format!("{:#?}", parse.syntax());
                self.diagnostics = parse
                    .errors()
                    .iter()
                    .map(|e| format!("{:?}  {}", e.range(), e.message()))
                    .collect();
            }
            Mode::Lint => {
                self.output = self.workspace.lint_active(&jals_lint::Config::default());
                self.diagnostics = Vec::new();
            }
        }
        self.mode = Some(mode);
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

    /// The diagnostics strip below the output, or nothing when there are no diagnostics.
    fn view_diagnostics(&self) -> Html {
        if self.diagnostics.is_empty() {
            return html! {};
        }
        let count = self.diagnostics.len();
        html! {
            <div class="max-h-40 shrink-0 overflow-auto border-t border-hairline bg-canvas px-4 py-2 font-mono text-xs text-error">
                <div class="pb-1 font-medium">
                    { format!("{count} issue{}", if count == 1 { "" } else { "s" }) }
                </div>
                { for self.diagnostics.iter().map(|d| html! { <div class="py-0.5">{ d }</div> }) }
            </div>
        }
    }

    /// The formatter settings bar. Each control emits a fresh [`Config`] via
    /// [`Msg::SetConfig`]; `onchange` (not `oninput`) keeps the numeric fields stable while
    /// typing. Parse failures leave the field unchanged.
    fn view_settings(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let cfg = &self.config;

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

        let on_indent_style = {
            let config = cfg.clone();
            link.callback(move |e: Event| {
                let el: HtmlSelectElement = e.target_unchecked_into();
                let mut c = config.clone();
                c.indent_style = if el.value() == "tab" {
                    IndentStyle::Tab
                } else {
                    IndentStyle::Space
                };
                Msg::SetConfig(c)
            })
        };
        let on_indent_width = usize_cb(|c, v| c.indent_width = v.max(1));
        let on_max_width = usize_cb(|c, v| c.max_width = v.max(1));
        let on_comment_width = usize_cb(|c, v| c.comment_width = v.max(1));
        let on_max_blank_lines = usize_cb(|c, v| c.max_blank_lines = v);
        let on_line_ending = {
            let config = cfg.clone();
            link.callback(move |e: Event| {
                let el: HtmlSelectElement = e.target_unchecked_into();
                let mut c = config.clone();
                c.line_ending = if el.value() == "crlf" {
                    LineEnding::Crlf
                } else {
                    LineEnding::Lf
                };
                Msg::SetConfig(c)
            })
        };
        let on_final_newline = {
            let config = cfg.clone();
            link.callback(move |e: Event| {
                let el: HtmlInputElement = e.target_unchecked_into();
                let mut c = config.clone();
                c.insert_final_newline = el.checked();
                Msg::SetConfig(c)
            })
        };
        let on_wrap_comments = {
            let config = cfg.clone();
            link.callback(move |e: Event| {
                let el: HtmlInputElement = e.target_unchecked_into();
                let mut c = config.clone();
                c.wrap_comments = el.checked();
                Msg::SetConfig(c)
            })
        };

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
        let mut app = App {
            workspace: Workspace::new(),
            output: String::new(),
            diagnostics: Vec::new(),
            mode: None,
            config: Config::default(),
        };
        app.run(Mode::Format);
        app
    }

    fn update(&mut self, _ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            // Write the edit straight into the workspace without re-rendering: the textarea DOM
            // already holds the text, so re-rendering here would be wasted work. The next `Run`
            // reads it back through the workspace.
            Msg::Input(value) => {
                self.workspace.edit_active(&value);
                false
            }
            Msg::SelectFile(path) => {
                self.workspace.set_active(&path);
                // Refresh the output for the newly-active file (and let the editor re-render with
                // its contents).
                if let Some(mode) = self.mode {
                    self.run(mode);
                }
                true
            }
            Msg::Run(mode) => {
                self.run(mode);
                true
            }
            Msg::SetConfig(config) => {
                self.config = config;
                // Apply the change live while formatted output is on screen.
                if self.mode == Some(Mode::Format) {
                    self.run(Mode::Format);
                }
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let oninput = link.callback(|e: InputEvent| {
            let textarea: HtmlTextAreaElement = e.target_unchecked_into();
            Msg::Input(textarea.value())
        });
        let on_format = link.callback(|_| Msg::Run(Mode::Format));
        let on_syntax = link.callback(|_| Msg::Run(Mode::Syntax));
        let on_lint = link.callback(|_| Msg::Run(Mode::Lint));

        let output_title = self.mode.map_or("Output", Mode::output_title);
        let label = "border-b border-hairline bg-canvas px-4 py-2 font-mono text-xs font-medium uppercase tracking-wider text-mute";
        let editor = "min-h-0 flex-1 overflow-auto whitespace-pre bg-canvas p-4 font-mono text-[13px] leading-5 text-ink outline-none";
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
                        <button onclick={on_syntax} class={secondary.clone()}>
                            { "Syntax tree" }
                        </button>
                        <button onclick={on_lint} class={secondary}>
                            { "Lint" }
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
                            <textarea
                                class={editor}
                                spellcheck="false"
                                placeholder="Type Java / JALS source here…"
                                value={self.workspace.active_source()}
                                oninput={oninput}
                            />
                        </section>
                        <section class="flex min-h-0 flex-col">
                            <div class={label}>{ output_title }</div>
                            <pre class={editor}>{ &self.output }</pre>
                            { self.view_diagnostics() }
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
