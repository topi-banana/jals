//! The right pane: the active file's lossless CST dump.

use yew::prelude::*;

use super::PANE_LABEL;

/// Props for [`SyntaxPane`].
#[derive(Properties, PartialEq)]
pub struct SyntaxPaneProps {
    /// The most recent syntax-tree dump, or `None` before the first *Syntax tree* press.
    pub dump: Option<String>,
}

/// The right pane: a header plus either the active file's lossless CST dump or a placeholder
/// prompting the user to request one. Purely presentational — the root [`App`] recomputes the
/// dump and feeds it down as [`SyntaxPaneProps::dump`].
///
/// [`App`]: crate::app::App
pub struct SyntaxPane;

impl Component for SyntaxPane {
    type Message = ();
    type Properties = SyntaxPaneProps;

    fn create(_ctx: &Context<Self>) -> Self {
        SyntaxPane
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let mono_pane = "min-h-0 flex-1 overflow-auto whitespace-pre bg-canvas p-4 font-mono text-[13px] leading-5 text-ink outline-none";
        html! {
            <section class="flex min-h-0 flex-col">
                <div class={PANE_LABEL}>{ "Syntax tree" }</div>
                if let Some(dump) = &ctx.props().dump {
                    <pre class={mono_pane}>{ dump }</pre>
                } else {
                    <div class="min-h-0 flex-1 overflow-auto bg-canvas p-4 font-mono text-xs text-mute">
                        { "Press “Syntax tree” to dump the lossless CST of the active file." }
                    </div>
                }
            </section>
        }
    }
}
