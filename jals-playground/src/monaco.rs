//! Rust-side bridge to the single Monaco editor instance.
//!
//! The editor is a JS singleton, created and driven imperatively (see `js/monaco_glue.js`); this
//! module is the thin, typed Rust surface over it. [`crate::components::EditorPane`] mounts and
//! wires the editor, while the root [`crate::app::App`] orchestrates content operations —
//! switching files, applying a format, repainting diagnostics — through these functions.

use jals_editor::{CompletionKind, DiagnosticSeverity};
use jals_hir::DefKind;
use wasm_bindgen::prelude::*;

// The Monaco glue (see `js/monaco_glue.js`), pulled in as a wasm-bindgen snippet.
#[wasm_bindgen(module = "/js/monaco_glue.js")]
extern "C" {
    /// Resolve once Monaco's editor API has loaded.
    #[wasm_bindgen(js_name = initMonaco)]
    pub fn init_monaco() -> js_sys::Promise;

    /// Upsert every indexed Java model and dispose models for generated files no longer indexed.
    #[wasm_bindgen(js_name = syncModels)]
    pub fn sync_models(files: &js_sys::Array);

    /// Create the editor in `el`, showing `path`'s model, calling `on_change` (debounced) on edits
    /// and `on_open` when a cross-file navigation switches the model to another file.
    #[wasm_bindgen(js_name = createEditor)]
    pub fn create_editor(
        el: &web_sys::Element,
        path: &str,
        value: &str,
        on_change: &js_sys::Function,
        on_open: &js_sys::Function,
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

    /// Replace markers on an existing model identified by its pseudo/project path.
    #[wasm_bindgen(js_name = setModelMarkers)]
    fn set_model_markers(path: &str, markers: &js_sys::Array);

    /// Register the Java document-formatting provider, calling `format` for the text
    /// (a Rust closure returning `Promise<string>`, awaited by the glue).
    #[wasm_bindgen(js_name = registerFormatter)]
    pub fn register_formatter(format: &js_sys::Function);

    // --- Language-feature providers. Each is registered once and calls a Rust closure that
    // returns a `Promise` resolving to the plain Monaco payload (see `providers`); Monaco accepts
    // Thenables for every ProviderResult, so the glue passes most results through untouched. ---

    /// Register the hover provider. `hover(text, line, col) -> Promise<{ contents } | null>`.
    #[wasm_bindgen(js_name = registerHover)]
    pub fn register_hover(hover: &js_sys::Function);

    /// Register the completion provider. `complete(text, line, col) -> Promise<suggestions[]>`
    /// (the glue awaits it to stamp each suggestion's replace range).
    #[wasm_bindgen(js_name = registerCompletion)]
    pub fn register_completion(complete: &js_sys::Function);

    /// Register the signature-help provider. `help(text, line, col) -> Promise<value | null>`
    /// (the glue awaits it to wrap the result as `{ value, dispose }`).
    #[wasm_bindgen(js_name = registerSignatureHelp)]
    pub fn register_signature_help(help: &js_sys::Function);

    /// Register the document-symbol provider. `symbols(text) -> Promise<DocumentSymbol[]>`.
    #[wasm_bindgen(js_name = registerDocumentSymbols)]
    pub fn register_document_symbols(symbols: &js_sys::Function);

    /// Register the document-highlight provider.
    /// `highlight(text, line, col) -> Promise<DocumentHighlight[]>`.
    #[wasm_bindgen(js_name = registerDocumentHighlight)]
    pub fn register_document_highlight(highlight: &js_sys::Function);

    /// Register the definition provider. `definition(text, line, col) -> Promise<Location | null>`.
    #[wasm_bindgen(js_name = registerDefinition)]
    pub fn register_definition(definition: &js_sys::Function);

    /// Register the references provider.
    /// `references(text, line, col, includeDecl) -> Promise<Location[]>`.
    #[wasm_bindgen(js_name = registerReferences)]
    pub fn register_references(references: &js_sys::Function);

    // --- JsValue factories for provider results (plain Monaco payload objects). ---

    /// A hover payload showing `markdown` as a Java-fenced code block.
    #[wasm_bindgen(js_name = hoverResult)]
    pub fn hover_result(markdown: &str) -> JsValue;

    /// One completion suggestion (`kind` is a Monaco `CompletionItemKind`).
    #[wasm_bindgen(js_name = completionItem)]
    pub fn completion_item(label: &str, kind: u32, detail: &str) -> JsValue;

    /// One signature (`param_offsets` is an array of `[start, end]` UTF-16 label offsets).
    #[wasm_bindgen(js_name = signatureInfo)]
    pub fn signature_info(label: &str, param_offsets: &js_sys::Array) -> JsValue;

    /// A signature-help payload wrapping the overloads and the active signature/parameter.
    #[wasm_bindgen(js_name = signatureHelpResult)]
    pub fn signature_help_result(
        signatures: &js_sys::Array,
        active_signature: u32,
        active_parameter: u32,
    ) -> JsValue;

    /// One document-symbol node (`kind` is a Monaco `SymbolKind`; `children` may be empty).
    #[wasm_bindgen(js_name = symbolNode)]
    pub fn symbol_node(
        name: &str,
        kind: u32,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
        children: &js_sys::Array,
    ) -> JsValue;

    /// One occurrence highlight (`write` selects Monaco's Write vs. Read kind).
    #[wasm_bindgen(js_name = highlightResult)]
    pub fn highlight_result(
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
        write: bool,
    ) -> JsValue;

    /// One navigation location (a target file `path` plus a range within it).
    #[wasm_bindgen(js_name = locationResult)]
    pub fn location_result(
        path: &str,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
    ) -> JsValue;
}

/// The Monaco numeric kinds a `jals-hir` [`DefKind`] maps to — the icon vocabularies the
/// completion and document-symbol providers marshal with.
pub trait DefKindExt {
    /// The Monaco `SymbolKind` (the outline icon) for a document symbol's [`DefKind`].
    fn symbol_kind(self) -> u32;
}

/// Monaco mapping for the shared protocol-neutral completion categories.
pub trait CompletionKindExt {
    fn completion_kind(self) -> u32;
}

impl CompletionKindExt for CompletionKind {
    fn completion_kind(self) -> u32 {
        match self {
            CompletionKind::Method => 0,
            CompletionKind::Field => 3,
            CompletionKind::EnumMember => 16,
            CompletionKind::Variable => 4,
            CompletionKind::TypeParameter => 24,
            CompletionKind::Class => 5,
            CompletionKind::Interface => 7,
            CompletionKind::Enum => 15,
            CompletionKind::Keyword => COMPLETION_KIND_KEYWORD,
        }
    }
}

impl DefKindExt for DefKind {
    fn symbol_kind(self) -> u32 {
        use DefKind::*;
        match self {
            Class => 4,                                                             // Class
            Record => 22,                                                           // Struct
            Interface | AnnotationType => 10,                                       // Interface
            Enum => 9,                                                              // Enum
            Field => 7,                                                             // Field
            Method => 5,                                                            // Method
            Constructor => 8,                                                       // Constructor
            EnumConstant => 21,                                                     // EnumMember
            TypeParam => 25,                                                        // TypeParameter
            Local | Param | LambdaParam | CatchParam | Resource | PatternVar => 12, // Variable
        }
    }
}

/// The Monaco `CompletionItemKind` for the Java keyword items (`Keyword`).
pub const COMPLETION_KIND_KEYWORD: u32 = 17;

/// One diagnostic marker in Monaco coordinates — one-based line/column, UTF-16, ready to display.
/// The caller maps its byte-offset diagnostics into these (via [`crate::host::MonacoRange`]);
/// the `js_sys`/`JsValue` marshalling stays behind [`Marker::set_diagnostics`].
pub struct Marker<'a> {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub message: &'a str,
    pub severity: DiagnosticSeverity,
}

impl<'a> Marker<'a> {
    /// Replace the current model's diagnostic markers with `markers`.
    pub fn set_diagnostics(markers: impl IntoIterator<Item = Marker<'a>>) {
        let array = js_sys::Array::new();
        for m in markers {
            array.push(&marker(
                m.start_line,
                m.start_col,
                m.end_line,
                m.end_col,
                m.message,
                m.monaco_severity(),
            ));
        }
        set_markers(&array);
    }

    /// Replace diagnostic markers on the existing model at `path`.
    pub fn set_diagnostics_for(path: &str, markers: impl IntoIterator<Item = Marker<'a>>) {
        let array = js_sys::Array::new();
        for diagnostic in markers {
            array.push(&marker(
                diagnostic.start_line,
                diagnostic.start_col,
                diagnostic.end_line,
                diagnostic.end_col,
                diagnostic.message,
                diagnostic.monaco_severity(),
            ));
        }
        set_model_markers(path, &array);
    }

    /// Map this marker's [`DiagnosticSeverity`] to a Monaco `MarkerSeverity` (Error = 8,
    /// Warning = 4, Hint = 1).
    fn monaco_severity(&self) -> u32 {
        match self.severity {
            DiagnosticSeverity::Error => 8,
            DiagnosticSeverity::Warning => 4,
            DiagnosticSeverity::Hint => 1,
        }
    }
}
