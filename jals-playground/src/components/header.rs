//! The top application bar.

use web_sys::HtmlInputElement;
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
    /// Emitted with the CORS-proxy URL as it changes — used to download the `jals.toml`
    /// `[dependencies]` jars (Maven Central needs it; CORS-permissive hosts do not).
    pub on_proxy_change: Callback<String>,
    /// The latest build-script/classpath status, shown beside the proxy input.
    pub deps_status: Option<String>,
}

/// The top application bar: the `jals playground` wordmark on the left, and — on the right — the
/// build/classpath status, the CORS-proxy input, and the *Syntax tree* / *Format* actions.
/// Purely presentational — it forwards clicks and the proxy value to the root [`App`] via its
/// [`HeaderProps`] callbacks.
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
        // The proxy input is uncontrolled — each keystroke pushes its value up so the next resolve
        // reads the latest.
        let on_proxy = {
            let cb = props.on_proxy_change.clone();
            Callback::from(move |e: InputEvent| {
                let el: HtmlInputElement = e.target_unchecked_into();
                cb.emit(el.value());
            })
        };

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
                    if let Some(status) = &props.deps_status {
                        <span class="font-mono text-xs text-mute">{ status }</span>
                    }
                    <input
                        class="h-8 w-64 rounded-md border border-hairline bg-canvas px-2 text-xs text-ink outline-none"
                        type="text"
                        placeholder="CORS proxy (optional, e.g. https://corsproxy.io/?)"
                        oninput={on_proxy}
                    />
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
