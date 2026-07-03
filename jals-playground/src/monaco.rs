//! Rust-side bridge to the single Monaco editor instance.
//!
//! The editor is a JS singleton, created and driven imperatively (see `js/monaco_glue.js`); this
//! module is the thin, typed Rust surface over it. [`crate::components::EditorPane`] mounts and
//! wires the editor, while the root [`crate::app::App`] orchestrates content operations —
//! switching files, applying a format, repainting diagnostics — through these functions.

use jals_lint::Severity;
use wasm_bindgen::prelude::*;

// The Monaco glue (see `js/monaco_glue.js`), pulled in as a wasm-bindgen snippet.
#[wasm_bindgen(module = "/js/monaco_glue.js")]
extern "C" {
    /// Resolve once Monaco's editor API has loaded.
    #[wasm_bindgen(js_name = initMonaco)]
    pub fn init_monaco() -> js_sys::Promise;

    /// Create the editor in `el`, showing `path`'s model, calling `on_change` (debounced) on edits.
    #[wasm_bindgen(js_name = createEditor)]
    pub fn create_editor(
        el: &web_sys::Element,
        path: &str,
        value: &str,
        on_change: &js_sys::Function,
    );

    /// Switch the editor to `path`'s model (created from `value` if new).
    #[wasm_bindgen(js_name = switchModel)]
    pub fn switch_model(path: &str, value: &str);

    /// Replace the current model's text (as an undoable edit).
    #[wasm_bindgen(js_name = updateModel)]
    pub fn update_model(value: &str);

    /// The live text currently in the editor.
    #[wasm_bindgen(js_name = currentValue)]
    pub fn current_value() -> String;

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
    pub fn register_formatter(format: &js_sys::Function);
}

/// One diagnostic marker in Monaco coordinates — one-based line/column, UTF-16, ready to display.
/// The caller maps its byte-offset diagnostics into these (via [`crate::line_index::LineIndex`]);
/// the `js_sys`/`JsValue` marshalling stays behind [`set_diagnostics`].
pub struct Marker<'a> {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub message: &'a str,
    pub severity: Severity,
}

/// Replace the current model's diagnostic markers with `markers`.
pub fn set_diagnostics<'a>(markers: impl IntoIterator<Item = Marker<'a>>) {
    let array = js_sys::Array::new();
    for m in markers {
        array.push(&marker(
            m.start_line,
            m.start_col,
            m.end_line,
            m.end_col,
            m.message,
            marker_severity(m.severity),
        ));
    }
    set_markers(&array);
}

/// Map a lint [`Severity`] to a Monaco `MarkerSeverity` (Error = 8, Warning = 4, Hint = 1).
fn marker_severity(severity: Severity) -> u32 {
    match severity {
        Severity::Error => 8,
        Severity::Warn => 4,
        Severity::Allow => 1,
    }
}
