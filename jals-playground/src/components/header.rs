//! The top application bar.

use yew::prelude::*;

/// Shared base classes for the toolbar buttons.
const BTN_BASE: &str = "inline-flex h-9 cursor-pointer items-center rounded-md px-3 text-sm font-medium transition-colors";

/// Props for [`Header`].
#[derive(Properties, PartialEq)]
pub struct HeaderProps {
    /// Invoked when the *Format* button is pressed.
    pub on_format: Callback<()>,
    /// Invoked when the *Syntax tree* button is pressed.
    pub on_syntax: Callback<()>,
}

/// The top application bar: the `jals playground` wordmark on the left, the *Syntax tree* and
/// *Format* actions on the right. Purely presentational — it forwards clicks to the root [`App`]
/// via its [`HeaderProps`] callbacks.
///
/// [`App`]: crate::app::App
pub struct Header;

impl Component for Header {
    type Message = ();
    type Properties = HeaderProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Header
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        // The buttons hand Monaco's `MouseEvent` back as `()` to the parent's action callbacks.
        let on_format = props.on_format.reform(|_| ());
        let on_syntax = props.on_syntax.reform(|_| ());

        let secondary = classes!(
            BTN_BASE,
            "border",
            "border-hairline",
            "bg-canvas",
            "text-ink",
            "hover:bg-canvas-soft"
        );

        html! {
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
        }
    }
}
