//! The right pane: the active file's lossless CST dump, or the last compile's report.

use web_sys::HtmlInputElement;
use yew::prelude::*;

use super::PANE_LABEL;

/// Which view the right pane shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PaneTab {
    /// The active file's lossless CST dump.
    Syntax,
    /// What the last *Build* produced, or why it produced nothing.
    Output,
}

impl PaneTab {
    /// Both tabs in display order, with their labels — the single source for the tab strip.
    const ALL: [(PaneTab, &'static str); 2] = [
        (PaneTab::Syntax, "Syntax tree"),
        (PaneTab::Output, "Build output"),
    ];
}

/// Props for [`ResultPane`].
#[derive(Properties, PartialEq)]
pub struct ResultPaneProps {
    /// Which tab is showing.
    pub tab: PaneTab,
    /// Emitted with the tab the user clicked.
    pub on_tab: Callback<PaneTab>,
    /// The most recent syntax-tree dump, or `None` before the first *Syntax tree* press.
    pub dump: Option<String>,
    /// The most recent compile's report, or `None` before the first *Build* press.
    pub output: Option<String>,
    /// The downloadable artifact's file name, when the last compile produced one. Only the name
    /// crosses into props — the bytes stay in [`App`], so a render never clones them.
    ///
    /// [`App`]: crate::app::App
    pub artifact: Option<String>,
    /// Invoked when the *Download* button is pressed.
    pub on_download: Callback<()>,
    /// Whether the artifact is one this host can execute, which is what puts the run controls
    /// under the report. A WebAssembly module is; a jar is not, because running one needs a JVM.
    pub runnable: bool,
    /// Emitted with the run box's text on every keystroke. The box is uncontrolled — the DOM holds
    /// what was typed — so this is a push and never a round trip.
    pub on_run_command: Callback<String>,
    /// Invoked when the *Run* button is pressed.
    pub on_run: Callback<()>,
    /// What the last run said, or `None` before one.
    pub run_output: Option<String>,
}

/// The right pane: a tab strip over either the active file's lossless CST dump or the last
/// compile's report plus its download. Purely presentational — the root [`App`] recomputes both
/// and feeds them down as [`ResultPaneProps`].
///
/// [`App`]: crate::app::App
pub struct ResultPane;

/// Shared class list for the pane body, whichever tab fills it. Compiler reports wrap (a message
/// is a sentence, not a tree), which the CST dump tolerates because it is already indented.
const MONO_BODY: &str = "min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-words bg-canvas p-4 font-mono text-[13px] leading-5 text-ink outline-none";

/// Shared class list for the placeholder shown before a tab has anything to say.
const PLACEHOLDER: &str = "min-h-0 flex-1 overflow-auto bg-canvas p-4 font-mono text-xs text-mute";

impl Component for ResultPane {
    type Message = ();
    type Properties = ResultPaneProps;

    fn create(_ctx: &Context<Self>) -> Self {
        ResultPane
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        let tabs = PaneTab::ALL.into_iter().map(|(tab, label)| {
            let on_click = props.on_tab.reform(move |_| tab);
            let classes = classes!(
                "cursor-pointer",
                "border-b-2",
                "pb-1",
                "transition-colors",
                if tab == props.tab {
                    "border-ink text-ink"
                } else {
                    "border-transparent hover:text-ink"
                }
            );
            html! { <button onclick={on_click} class={classes}>{ label }</button> }
        });

        html! {
            <section class="flex min-h-0 flex-col">
                <div class={classes!(PANE_LABEL, "flex", "items-center", "gap-4")}>
                    { for tabs }
                </div>
                if props.tab == PaneTab::Syntax {
                    { Self::syntax_body(props) }
                } else {
                    { Self::output_body(props) }
                }
            </section>
        }
    }
}

impl ResultPane {
    fn syntax_body(props: &ResultPaneProps) -> Html {
        props.dump.as_ref().map_or_else(
            || {
                html! {
                    <div class={PLACEHOLDER}>
                        { "Press “Syntax tree” to dump the lossless CST of the active file." }
                    </div>
                }
            },
            |dump| html! { <pre class={MONO_BODY}>{ dump }</pre> },
        )
    }

    fn output_body(props: &ResultPaneProps) -> Html {
        let body = props.output.as_ref().map_or_else(
            || {
                html! {
                    <div class={PLACEHOLDER}>
                        { "Press “Build” to compile the workspace with the backend jals.toml selects." }
                    </div>
                }
            },
            |output| html! { <pre class={MONO_BODY}>{ output }</pre> },
        );
        let download = props.artifact.as_ref().map(|name| {
            let on_click = props.on_download.reform(|_| ());
            html! {
                <div class="shrink-0 border-t border-hairline bg-canvas px-4 py-3">
                    <button
                        onclick={on_click}
                        class="inline-flex h-9 cursor-pointer items-center rounded-md bg-ink px-3 text-sm font-medium text-canvas transition-colors hover:opacity-90"
                    >
                        { format!("Download {name}") }
                    </button>
                </div>
            }
        });
        // Two things a compile report cannot explain about itself: why a resolved dependency is
        // not on the compiler's classpath, and why a path nobody authored appears in the errors.
        html! {
            { body }
            { Self::run_controls(props) }
            { download }
            <div class="shrink-0 border-t border-hairline bg-canvas px-4 py-2 font-mono text-[11px] leading-4 text-mute">
                { "The in-process compiler reads library signatures from the embedded JDK stubs; \
                   resolved [dependencies] jars are not on its classpath. Paths under target/ are \
                   build-script output." }
            </div>
        }
    }

    /// The run box, for the one artifact this host can execute.
    ///
    /// A method name and its arguments rather than a *Run* with no target: wasm has no entry-point
    /// convention, and Java's `main` cannot be lowered here — its `String[]` needs a `java.base`
    /// the module has no room for. An empty box still runs something: instantiating executes the
    /// module's start function, which is where the project's `static` initialisers went.
    fn run_controls(props: &ResultPaneProps) -> Option<Html> {
        if !props.runnable {
            return None;
        }
        let on_input = {
            let command = props.on_run_command.clone();
            Callback::from(move |event: InputEvent| {
                let element: HtmlInputElement = event.target_unchecked_into();
                command.emit(element.value());
            })
        };
        let on_click = props.on_run.reform(|_| ());
        Some(html! {
            <div class="shrink-0 border-t border-hairline bg-canvas px-4 py-3">
                <div class="flex items-center gap-2">
                    <input
                        class="h-9 flex-1 rounded-md border border-hairline bg-canvas px-2 font-mono text-sm text-ink outline-none"
                        type="text"
                        placeholder="exported static method and arguments, e.g. twice 21 (empty: instantiate only)"
                        oninput={on_input}
                    />
                    <button
                        onclick={on_click}
                        class="inline-flex h-9 cursor-pointer items-center rounded-md bg-ink px-3 text-sm font-medium text-canvas transition-colors hover:opacity-90"
                    >
                        { "Run" }
                    </button>
                </div>
                if let Some(output) = &props.run_output {
                    <pre class="mt-2 whitespace-pre-wrap break-words font-mono text-[13px] leading-5 text-ink">
                        { output }
                    </pre>
                }
            </div>
        })
    }
}
