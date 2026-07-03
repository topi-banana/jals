//! The center editor section: the Monaco editor mount and its lifecycle.

use std::cell::RefCell;
use std::rc::Rc;

use jals_fmt::Config;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use yew::prelude::*;

use super::PANE_LABEL;
use crate::monaco;

/// Props for [`EditorPane`].
#[derive(Properties, PartialEq)]
pub struct EditorPaneProps {
    /// Path of the active file — the pane's label, and the model created on first mount.
    pub path: String,
    /// The active file's source, used to seed the editor's model on first mount.
    pub source: String,
    /// Emitted (debounced) with the full buffer text on every edit.
    pub on_change: Callback<String>,
    /// Emitted once, after the editor exists and is ready for the initial diagnostics.
    pub on_ready: Callback<()>,
    /// Shared formatter config the once-registered *Format Document* provider reads.
    pub config: Rc<RefCell<Config>>,
}

/// The center editor section: a label naming the active file, and the Monaco editor mounted into a
/// container `<div>`.
///
/// This component owns the editor's *creation and wiring*: on first render it boots Monaco, creates
/// the single editor over the active file, wires the (debounced) change callback, registers the
/// *Format Document* provider, and signals readiness. The root [`App`] drives *content* operations
/// — switching files, applying a format, repainting markers — through the [`crate::monaco`]
/// service. The component re-renders only to refresh its label; the editor `<div>` is preserved
/// across every diff, so the live Monaco instance is never torn down.
///
/// [`App`]: crate::app::App
pub struct EditorPane {
    /// The DOM node Monaco is mounted into.
    node_ref: NodeRef,
}

impl Component for EditorPane {
    type Message = ();
    type Properties = EditorPaneProps;

    fn create(_ctx: &Context<Self>) -> Self {
        EditorPane {
            node_ref: NodeRef::default(),
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, old_props: &Self::Properties) -> bool {
        // Re-render only when the active file changes, to refresh the file-name label. The Monaco
        // container `<div>` keeps its place in the diff, so the live editor is untouched.
        ctx.props().path != old_props.path
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if !first_render {
            return;
        }
        let Some(el) = self.node_ref.cast::<web_sys::Element>() else {
            return;
        };
        let props = ctx.props();
        let path = props.path.clone();
        let source = props.source.clone();

        // Monaco notifies us (debounced, from JS) whenever the buffer changes.
        let on_change = props.on_change.clone();
        let change_closure = Closure::<dyn FnMut(String)>::new(move |value: String| {
            on_change.emit(value);
        });

        // "Format Document" (Ctrl+Shift+I) formats with the latest shared config.
        let config = props.config.clone();
        let formatter = Closure::<dyn FnMut(String) -> String>::new(move |text: String| {
            let cfg = config.borrow();
            jals_fmt::format_source(&text, &cfg).formatted
        });

        let on_ready = props.on_ready.clone();
        spawn_local(async move {
            JsFuture::from(monaco::init_monaco()).await.ok();
            monaco::create_editor(&el, &path, &source, change_closure.as_ref().unchecked_ref());
            monaco::register_formatter(formatter.as_ref().unchecked_ref());
            // Keep the closures alive for the app's lifetime (a single editor instance).
            change_closure.forget();
            formatter.forget();
            // Now that the editor exists, let the app paint the initial diagnostics.
            on_ready.emit(());
        });
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        html! {
            <section class="flex min-h-0 flex-col border-b border-hairline md:border-b-0 md:border-r">
                <div class={PANE_LABEL}>{ ctx.props().path.clone() }</div>
                <div ref={self.node_ref.clone()} class="min-h-0 flex-1" />
            </section>
        }
    }
}
