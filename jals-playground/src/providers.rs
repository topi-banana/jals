//! Wires each Monaco language-feature provider to the in-browser [`Workspace`] analysis.
//!
//! Every provider is a Rust closure returning a `js_sys::Promise` (Monaco accepts Thenables for
//! all provider results): it receives the model's live text and cursor position from JS, locks
//! the shared workspace, reflects the text into the overlay ([`Workspace::sync_active`] — a
//! no-op when unchanged, so a query storm never re-analyzes), runs the corresponding async
//! [`Workspace`] query, and marshals the Monaco payload into a plain `JsValue` via the
//! [`crate::monaco`] factories.
//!
//! The closures capture a shared `Rc<futures::lock::Mutex<Workspace>>` and **hold the lock across
//! their awaits** — deliberate single-flight serialization: concurrent Monaco requests (and the
//! app's own async handlers) queue on the FIFO-fair lock instead of interleaving analysis, while
//! the runtime's yields still return to the JS event loop so the page paints. No `RefCell` borrow
//! is ever held across an await. Each closure is `forget`ted, kept alive for the app's single
//! editor.

use std::rc::Rc;

use futures::lock::Mutex;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::host::{SymbolNode, Target};
use crate::monaco::{self, CompletionKindExt, DefKindExt};
use crate::workspace::Workspace;

/// The Monaco language-feature providers, registered once against the shared [`Workspace`].
pub struct Providers;

impl Providers {
    /// Register every language-feature provider, each backed by `workspace`. Called once, after the
    /// editor exists.
    pub fn install(workspace: Rc<Mutex<Workspace>>) {
        Self::install_hover(Rc::clone(&workspace));
        Self::install_completion(Rc::clone(&workspace));
        Self::install_signature_help(Rc::clone(&workspace));
        Self::install_document_symbols(Rc::clone(&workspace));
        Self::install_document_highlight(Rc::clone(&workspace));
        Self::install_definition(Rc::clone(&workspace));
        Self::install_references(workspace);
    }

    /// Hand `closure` to `register_with` (the matching `monaco::register_*`) and leak it, so the
    /// once-registered provider stays live for the app's single editor. Centralises the
    /// easy-to-drop `forget()` every installer would otherwise repeat.
    fn register<T: ?Sized + wasm_bindgen::closure::WasmClosure>(
        closure: Closure<T>,
        register_with: impl FnOnce(&js_sys::Function),
    ) {
        register_with(closure.as_ref().unchecked_ref());
        closure.forget();
    }

    /// Install a position provider: an async `(&Workspace, line, col) -> JsValue` body backed by
    /// `ws`, wired to its matching `monaco::register_*`. Folds the shared Promise bridging
    /// (`future_to_promise`), the lock-then-sync-then-query sequencing, and `register`/`forget`
    /// that the five position-based installers would otherwise repeat.
    fn install_pos(
        ws: Rc<Mutex<Workspace>>,
        body: impl AsyncFn(&Workspace, u32, u32) -> JsValue + 'static,
        register_with: impl FnOnce(&js_sys::Function),
    ) {
        let body = Rc::new(body);
        let closure = Closure::<dyn FnMut(String, u32, u32) -> js_sys::Promise>::new(
            move |text: String, line: u32, col: u32| {
                let ws = Rc::clone(&ws);
                let body = Rc::clone(&body);
                future_to_promise(async move {
                    // The lock is held across the awaits on purpose (single-flight; see the
                    // module docs). Reflect the live buffer first, then query.
                    let mut ws = ws.lock().await;
                    ws.sync_active(&text).await;
                    Ok((*body)(&ws, line, col).await)
                })
            },
        );
        Self::register(closure, register_with);
    }

    /// Marshal a navigation [`Target`] into a Monaco `Location` payload (shared by go-to-definition
    /// and find-references).
    fn location(target: &Target) -> JsValue {
        monaco::location_result(
            &target.path,
            target.range.start_line,
            target.range.start_col,
            target.range.end_line,
            target.range.end_col,
        )
    }

    fn install_hover(ws: Rc<Mutex<Workspace>>) {
        Self::install_pos(
            ws,
            async |ws, line, col| match ws.hover(line, col).await {
                Some(markdown) => monaco::hover_result(&markdown),
                None => JsValue::NULL,
            },
            monaco::register_hover,
        );
    }

    fn install_completion(ws: Rc<Mutex<Workspace>>) {
        Self::install_pos(
            ws,
            async |ws, line, col| {
                let items = js_sys::Array::new();
                for entry in ws.completions(line, col).await {
                    let kind = entry.kind.completion_kind();
                    items.push(&monaco::completion_item(&entry.label, kind, &entry.detail));
                }
                items.into()
            },
            monaco::register_completion,
        );
    }

    fn install_signature_help(ws: Rc<Mutex<Workspace>>) {
        Self::install_pos(
            ws,
            async |ws, line, col| match ws.signature_help(line, col).await {
                Some(help) => {
                    let signatures = js_sys::Array::new();
                    for sig in &help.signatures {
                        let offsets = js_sys::Array::new();
                        for (start, end) in &sig.parameters {
                            offsets.push(&js_sys::Array::of2(&(*start).into(), &(*end).into()));
                        }
                        signatures.push(&monaco::signature_info(&sig.label, &offsets));
                    }
                    monaco::signature_help_result(
                        &signatures,
                        help.active_signature,
                        help.active_parameter,
                    )
                }
                None => JsValue::NULL,
            },
            monaco::register_signature_help,
        );
    }

    fn install_document_symbols(ws: Rc<Mutex<Workspace>>) {
        let closure = Closure::<dyn FnMut(String) -> js_sys::Promise>::new(move |text: String| {
            let ws = Rc::clone(&ws);
            future_to_promise(async move {
                let mut ws = ws.lock().await;
                ws.sync_active(&text).await;
                let symbols = ws.document_symbols();
                Ok(Self::symbols_to_js(&symbols).into())
            })
        });
        Self::register(closure, monaco::register_document_symbols);
    }

    /// Recursively marshal a symbol outline into a Monaco `DocumentSymbol[]`.
    fn symbols_to_js(nodes: &[SymbolNode]) -> js_sys::Array {
        let array = js_sys::Array::new();
        for node in nodes {
            let children = Self::symbols_to_js(&node.children);
            array.push(&monaco::symbol_node(
                &node.name,
                node.kind.symbol_kind(),
                node.range.start_line,
                node.range.start_col,
                node.range.end_line,
                node.range.end_col,
                &children,
            ));
        }
        array
    }

    fn install_document_highlight(ws: Rc<Mutex<Workspace>>) {
        Self::install_pos(
            ws,
            async |ws, line, col| {
                let array = js_sys::Array::new();
                for h in ws.document_highlight(line, col).await {
                    array.push(&monaco::highlight_result(
                        h.range.start_line,
                        h.range.start_col,
                        h.range.end_line,
                        h.range.end_col,
                        h.write,
                    ));
                }
                array.into()
            },
            monaco::register_document_highlight,
        );
    }

    fn install_definition(ws: Rc<Mutex<Workspace>>) {
        Self::install_pos(
            ws,
            async |ws, line, col| match ws.goto_definition(line, col).await {
                Some(target) => Self::location(&target),
                None => JsValue::NULL,
            },
            monaco::register_definition,
        );
    }

    fn install_references(ws: Rc<Mutex<Workspace>>) {
        let closure = Closure::<dyn FnMut(String, u32, u32, bool) -> js_sys::Promise>::new(
            move |text: String, line: u32, col: u32, include_decl: bool| {
                let ws = Rc::clone(&ws);
                future_to_promise(async move {
                    let mut ws = ws.lock().await;
                    ws.sync_active(&text).await;
                    let targets = ws.references(line, col, include_decl).await;
                    let array = js_sys::Array::new();
                    for target in targets {
                        array.push(&Self::location(&target));
                    }
                    Ok(array.into())
                })
            },
        );
        Self::register(closure, monaco::register_references);
    }
}
