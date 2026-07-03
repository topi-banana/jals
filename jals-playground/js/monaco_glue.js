// Thin glue between the Rust/wasm playground and the Monaco Editor.
//
// Monaco itself is loaded by the AMD loader configured in `index.html`; this
// module is pulled in as a wasm-bindgen snippet and drives a single editor
// instance imperatively (Yew owns only the container `<div>`).

let monacoReady = null;
let editor = null;
const models = new Map(); // path -> monaco.editor.ITextModel
const MARKER_OWNER = "jals";

// Resolve once Monaco's editor API has finished loading. Idempotent — the
// promise is created on the first call and reused thereafter.
export function initMonaco() {
    if (!monacoReady) {
        monacoReady = new Promise((resolve) => {
            require(["vs/editor/editor.main"], () => resolve());
        });
    }
    return monacoReady;
}

// Get-or-create the `java` model for `path`, seeded with `value` on first use.
function modelFor(path, value) {
    let model = models.get(path);
    if (!model) {
        model = monaco.editor.createModel(value, "java");
        models.set(path, model);
    }
    return model;
}

// Create the editor inside `el`, showing `path`'s model, and notify `onChange`
// (a Rust closure), debounced, with the full buffer text on every edit.
export function createEditor(el, path, value, onChange) {
    editor = monaco.editor.create(el, {
        model: modelFor(path, value),
        automaticLayout: true,
        minimap: { enabled: false },
        scrollBeyondLastLine: false,
        fontSize: 13,
        tabSize: 4,
    });
    let timer = null;
    editor.onDidChangeModelContent(() => {
        if (timer !== null) clearTimeout(timer);
        timer = setTimeout(() => {
            timer = null;
            onChange(editor.getValue());
        }, 250);
    });
}

// Switch the editor to `path`'s model (creating it from `value` if new),
// preserving each file's own undo history and cursor position.
export function switchModel(path, value) {
    if (!editor) return;
    editor.setModel(modelFor(path, value));
}

// Replace the current model's text (e.g. after formatting) as a normal edit so
// undo still works. No-op if the text is unchanged.
export function updateModel(value) {
    if (!editor) return;
    const model = editor.getModel();
    if (!model || model.getValue() === value) return;
    editor.executeEdits("jals-format", [
        { range: model.getFullModelRange(), text: value },
    ]);
    editor.pushUndoStop();
}

// The live text currently in the editor (empty before it is created).
export function currentValue() {
    return editor ? editor.getValue() : "";
}

// Build one marker object; Rust assembles these into an array for `setMarkers`.
export function marker(startLineNumber, startColumn, endLineNumber, endColumn, message, severity) {
    return { startLineNumber, startColumn, endLineNumber, endColumn, message, severity };
}

// Replace the diagnostic markers on the current model (see `marker`).
export function setMarkers(markers) {
    if (!editor) return;
    const model = editor.getModel();
    if (model) monaco.editor.setModelMarkers(model, MARKER_OWNER, markers);
}

// Register a Java document-formatting provider that calls back into `format`
// (a Rust closure) with the buffer text and applies the returned string. Wires
// up "Format Document" (Ctrl+Shift+I / right-click).
export function registerFormatter(format) {
    monaco.languages.registerDocumentFormattingEditProvider("java", {
        provideDocumentFormattingEdits(model) {
            return [
                { range: model.getFullModelRange(), text: format(model.getValue()) },
            ];
        },
    });
}
