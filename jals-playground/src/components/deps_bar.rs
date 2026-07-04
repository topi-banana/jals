//! The external-dependencies bar under the settings bar.

use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;

/// Props for [`DepsBar`].
#[derive(Properties, PartialEq)]
pub struct DepsBarProps {
    /// A status line under the controls: the last resolve's outcome (resolved N classes / an error /
    /// warnings), or `None` before the first resolve.
    pub status: Option<String>,
    /// Emitted with `(dependencies_toml, proxy)` when *Resolve* is clicked.
    pub on_resolve: Callback<(String, String)>,
}

/// A bar for external dependency resolution: a `[dependencies]` TOML box, an optional CORS-proxy
/// URL, and a *Resolve* button. Downloading a jar in the browser is subject to CORS, so the
/// placeholder points at a CORS-permissive host and the proxy field covers the rest. The bar is
/// presentational — the root [`App`](crate::app::App) owns the async resolve flow and feeds the
/// [`status`](DepsBarProps::status) back down; the two inputs are read from the DOM on click via
/// [`NodeRef`]s (mirroring [`SettingsBar`](crate::components::SettingsBar)'s stateless controls).
pub struct DepsBar {
    deps: NodeRef,
    proxy: NodeRef,
}

impl Component for DepsBar {
    type Message = ();
    type Properties = DepsBarProps;

    fn create(_ctx: &Context<Self>) -> Self {
        DepsBar {
            deps: NodeRef::default(),
            proxy: NodeRef::default(),
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        let deps_ref = self.deps.clone();
        let proxy_ref = self.proxy.clone();
        let on_resolve = props.on_resolve.clone();
        let onclick = Callback::from(move |_: MouseEvent| {
            let deps = deps_ref
                .cast::<HtmlTextAreaElement>()
                .map(|el| el.value())
                .unwrap_or_default();
            let proxy = proxy_ref
                .cast::<HtmlInputElement>()
                .map(|el| el.value())
                .unwrap_or_default();
            on_resolve.emit((deps, proxy));
        });

        let lbl = "font-mono text-xs text-mute";
        html! {
            <div class="flex flex-col gap-2 border-b border-hairline bg-canvas-soft px-6 py-2">
                <div class="flex items-center gap-2">
                    <span class={lbl}>{ "Dependencies" }</span>
                    <input
                        ref={self.proxy.clone()}
                        class="h-7 flex-1 rounded-md border border-hairline bg-canvas px-2 text-xs text-ink outline-none"
                        type="text"
                        placeholder="CORS proxy (optional, e.g. https://corsproxy.io/?)"
                    />
                    <button
                        class="h-7 rounded-md border border-hairline bg-canvas px-3 text-xs text-ink"
                        {onclick}
                    >{ "Resolve" }</button>
                </div>
                <textarea
                    ref={self.deps.clone()}
                    class="h-20 w-full resize-y rounded-md border border-hairline bg-canvas p-2 font-mono text-xs text-ink outline-none"
                    placeholder={PLACEHOLDER}
                />
                if let Some(status) = &props.status {
                    <span class={lbl}>{ status }</span>
                }
            </div>
        }
    }
}

/// The `[dependencies]` placeholder — a CORS-permissive jar so the demo works without a proxy.
const PLACEHOLDER: &str = "[dependencies]\n\
     # a CORS-permissive jar works directly; Maven Central needs the proxy above\n\
     # mylib = { jar = \"https://cdn.jsdelivr.net/.../mylib.jar\" }";
