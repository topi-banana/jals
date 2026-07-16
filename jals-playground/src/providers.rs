//! Wires each Monaco language-feature provider to the in-browser [`Workspace`] analysis.
//!
//! Every provider is a synchronous Rust closure (mirroring the formatter): it receives the model's
//! live text and cursor position from JS, reflects the text into the workspace overlay
//! ([`Workspace::sync_active`] — a no-op when unchanged, so a query storm never re-analyzes), runs
//! the corresponding [`Workspace`] query, and marshals the Monaco payload into a plain `JsValue`
//! via the [`crate::monaco`] factories. The closures capture a shared `Rc<RefCell<Workspace>>`;
//! the mutable sync borrow is always released before the immutable query borrow (and neither is
//! ever held across a call into `monaco::*`), so there is no clash with `App::update`. Each
//! closure is `forget`ted, kept alive for the app's single editor.

use std::cell::RefCell;
use std::rc::Rc;

use crate::host::{SymbolNode, Target};
use crate::monaco::{self, CompletionKindExt, DefKindExt};
use crate::workspace::Workspace;
use wasm_bindgen::prelude::*;

/// The Monaco language-feature providers, registered once against the shared [`Workspace`].
pub struct Providers;

impl Providers {
    /// Register every language-feature provider, each backed by `workspace`. Called once, after the
    /// editor exists.
    pub fn install(workspace: Rc<RefCell<Workspace>>) {
        Self::install_hover(workspace.clone());
        Self::install_completion(workspace.clone());
        Self::install_signature_help(workspace.clone());
        Self::install_document_symbols(workspace.clone());
        Self::install_document_highlight(workspace.clone());
        Self::install_definition(workspace.clone());
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

    /// Install a position provider: a `(&Workspace, line, col) -> JsValue` body backed by `ws`,
    /// wired to its matching `monaco::register_*`. Folds the shared `Closure` construction, the
    /// sync-then-query borrow discipline, and `register`/`forget` that the five position-based
    /// installers would otherwise repeat.
    fn install_pos(
        ws: Rc<RefCell<Workspace>>,
        body: impl Fn(&Workspace, u32, u32) -> JsValue + 'static,
        register_with: impl FnOnce(&js_sys::Function),
    ) {
        let closure = Closure::<dyn FnMut(String, u32, u32) -> JsValue>::new(
            move |text: String, line: u32, col: u32| {
                // Reflect the live buffer first; the mutable borrow ends with the statement.
                ws.borrow_mut().sync_active(&text);
                body(&ws.borrow(), line, col)
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

    fn install_hover(ws: Rc<RefCell<Workspace>>) {
        Self::install_pos(
            ws,
            |ws, line, col| match ws.hover(line, col) {
                Some(markdown) => monaco::hover_result(&markdown),
                None => JsValue::NULL,
            },
            monaco::register_hover,
        );
    }

    fn install_completion(ws: Rc<RefCell<Workspace>>) {
        Self::install_pos(
            ws,
            |ws, line, col| {
                let items = js_sys::Array::new();
                for entry in ws.completions(line, col) {
                    let kind = entry.kind.completion_kind();
                    items.push(&monaco::completion_item(&entry.label, kind, &entry.detail));
                }
                items.into()
            },
            monaco::register_completion,
        );
    }

    fn install_signature_help(ws: Rc<RefCell<Workspace>>) {
        Self::install_pos(
            ws,
            |ws, line, col| match ws.signature_help(line, col) {
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

    fn install_document_symbols(ws: Rc<RefCell<Workspace>>) {
        let closure = Closure::<dyn FnMut(String) -> JsValue>::new(move |text: String| {
            ws.borrow_mut().sync_active(&text);
            let symbols = ws.borrow().document_symbols();
            Self::symbols_to_js(&symbols).into()
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

    fn install_document_highlight(ws: Rc<RefCell<Workspace>>) {
        Self::install_pos(
            ws,
            |ws, line, col| {
                let array = js_sys::Array::new();
                for h in ws.document_highlight(line, col) {
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

    fn install_definition(ws: Rc<RefCell<Workspace>>) {
        Self::install_pos(
            ws,
            |ws, line, col| match ws.goto_definition(line, col) {
                Some(target) => Self::location(&target),
                None => JsValue::NULL,
            },
            monaco::register_definition,
        );
    }

    fn install_references(ws: Rc<RefCell<Workspace>>) {
        let closure = Closure::<dyn FnMut(String, u32, u32, bool) -> JsValue>::new(
            move |text: String, line: u32, col: u32, include_decl: bool| {
                ws.borrow_mut().sync_active(&text);
                let targets = ws.borrow().references(line, col, include_decl);
                let array = js_sys::Array::new();
                for target in targets {
                    array.push(&Self::location(&target));
                }
                array.into()
            },
        );
        Self::register(closure, monaco::register_references);
    }
}
