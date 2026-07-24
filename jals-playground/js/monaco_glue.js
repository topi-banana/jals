// Thin glue between the Rust/wasm playground and the Monaco Editor.
//
// Monaco itself is loaded by the AMD loader configured in `index.html`; this
// module is pulled in as a wasm-bindgen snippet and drives a single editor
// instance imperatively (Yew owns only the container `<div>`).

let monacoReady = null;
let editor = null;
let changeTimer = null;
const models = new Map(); // path -> monaco.editor.ITextModel
const MARKER_OWNER = "jals";

// Every file gets a stable in-memory URI so cross-file navigation and peek
// references can address models that were never opened in the editor.
const URI_PREFIX = "inmemory://jals/";
function pathToUri(path) {
  return monaco.Uri.parse(URI_PREFIX + path);
}
function uriToPath(uri) {
  // Our URIs are `inmemory://jals/<path>`, so the path is `uri.path` sans its
  // leading slash. Returns null for any foreign URI.
  if (uri.scheme !== "inmemory") return null;
  return uri.path.replace(/^\//, "");
}

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

// The Monaco language for `path`, by extension: TOML and Rhai pseudo-files are
// plaintext, everything else is Java. Keeping pseudo-files off `java` also keeps
// Java-only providers and formatting from firing on them.
function langFor(path) {
  return path.endsWith(".toml") || path.endsWith(".rhai")
    ? "plaintext"
    : "java";
}

// Get-or-create the URI-backed model for `path`, seeded with `value` on first
// use (its language is chosen by `langFor`).
function modelFor(path, value) {
  let model = models.get(path);
  if (!model) {
    model = monaco.editor.createModel(value, langFor(path), pathToUri(path));
    models.set(path, model);
  }
  return model;
}

// Upsert all indexed Java models and remove stale generated models. Never replace
// the currently edited model: its live text may be newer than Rust's last debounce.
export function syncModels(files) {
  const indexed = new Set();
  const active = editor?.getModel();
  for (const [path, text] of files) {
    indexed.add(path);
    const model = modelFor(path, text);
    if (model !== active && model.getValue() !== text) model.setValue(text);
  }
  for (const [path, model] of models) {
    if (path.endsWith(".java") && !indexed.has(path) && model !== active) {
      model.dispose();
      models.delete(path);
    }
  }
}

// Create the editor inside `el`, showing `path`'s model, and notify `onChange`
// (a Rust closure), debounced, with the full buffer text on every edit.
// `onOpen` (a Rust closure) fires when a cross-file navigation switches the
// single editor to another file's model.
export function createEditor(el, path, value, onChange, onOpen) {
  editor = monaco.editor.create(el, {
    model: modelFor(path, value),
    automaticLayout: true,
    minimap: { enabled: false },
    scrollBeyondLastLine: false,
    fontSize: 13,
    tabSize: 4,
  });
  editor.onDidChangeModelContent(() => {
    if (changeTimer !== null) clearTimeout(changeTimer);
    changeTimer = setTimeout(() => {
      changeTimer = null;
      onChange(editor.getValue());
    }, 250);
  });

  // Cross-file navigation: the standalone editor only knows how to open a
  // target in its own model. Override the code-editor service so that when
  // Monaco navigates to another file's URI (go-to-definition, peek jump), we
  // switch this one editor to that model and tell Rust which file is now
  // active. This is the well-known monaco standalone multi-model hack.
  const service = editor._codeEditorService;
  if (service) {
    const openBase = service.openCodeEditor.bind(service);
    service.openCodeEditor = async (input, sourceEditor) => {
      const result = await openBase(input, sourceEditor);
      if (result === null && sourceEditor) {
        const model = monaco.editor.getModel(input.resource);
        const targetPath = uriToPath(input.resource);
        if (model && targetPath) {
          // Flush the outgoing file's live text before switching away.
          if (changeTimer !== null) clearTimeout(changeTimer);
          changeTimer = null;
          onChange(sourceEditor.getValue());
          sourceEditor.setModel(model);
          const selection = input.options?.selection;
          if (selection) {
            sourceEditor.setSelection(selection);
            sourceEditor.revealRangeInCenterIfOutsideViewport(selection);
          }
          onOpen(targetPath);
          return sourceEditor;
        }
      }
      return result;
    };
  }
}

// Switch the editor to `path`'s model (creating it from `value` if new),
// preserving each file's own undo history and cursor position.
export function switchModel(path, value) {
  if (!editor) return;
  if (changeTimer !== null) clearTimeout(changeTimer);
  changeTimer = null;
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
export function marker(
  startLineNumber,
  startColumn,
  endLineNumber,
  endColumn,
  message,
  severity,
) {
  return {
    startLineNumber,
    startColumn,
    endLineNumber,
    endColumn,
    message,
    severity,
  };
}

// Replace the diagnostic markers on the current model (see `marker`).
export function setMarkers(markers) {
  if (!editor) return;
  const model = editor.getModel();
  if (model) monaco.editor.setModelMarkers(model, MARKER_OWNER, markers);
}

// Replace markers on a model without switching the editor to it. Missing models
// are left alone; selecting a pseudo-file creates it and Rust repaints then.
export function setModelMarkers(path, markers) {
  const model = models.get(path);
  if (model) monaco.editor.setModelMarkers(model, MARKER_OWNER, markers);
}

// Register a Java document-formatting provider that calls back into `format`
// (a Rust closure returning a Promise of the formatted string) and applies the
// awaited result. Wires up "Format Document" (Ctrl+Shift+I / right-click).
export function registerFormatter(format) {
  monaco.languages.registerDocumentFormattingEditProvider("java", {
    async provideDocumentFormattingEdits(model) {
      const text = await format(model.getValue());
      return [{ range: model.getFullModelRange(), text }];
    },
  });
}

// --- Language-feature providers. Each calls a Rust closure with the model's
// text and the cursor position; the closure returns a `Promise` resolving to
// the plain Monaco payload it built (via the factories below). Monaco accepts
// Thenables for every ProviderResult, so a Promise that needs no reshaping is
// returned as-is; only results whose fields must be touched are awaited here.

export function registerHover(hover) {
  monaco.languages.registerHoverProvider("java", {
    provideHover(model, position) {
      // Resolves to `{ contents }` or null; Monaco accepts the Thenable directly.
      return hover(model.getValue(), position.lineNumber, position.column);
    },
  });
}

export function registerCompletion(complete) {
  monaco.languages.registerCompletionItemProvider("java", {
    triggerCharacters: ["."],
    async provideCompletionItems(model, position) {
      const word = model.getWordUntilPosition(position);
      const range = {
        startLineNumber: position.lineNumber,
        endLineNumber: position.lineNumber,
        startColumn: word.startColumn,
        endColumn: word.endColumn,
      };
      const suggestions = await complete(
        model.getValue(),
        position.lineNumber,
        position.column,
      );
      for (const s of suggestions) s.range = range;
      return { suggestions };
    },
  });
}

export function registerSignatureHelp(help) {
  monaco.languages.registerSignatureHelpProvider("java", {
    signatureHelpTriggerCharacters: ["(", ","],
    signatureHelpRetriggerCharacters: [","],
    async provideSignatureHelp(model, position) {
      const value = await help(
        model.getValue(),
        position.lineNumber,
        position.column,
      );
      return value ? { value, dispose() {} } : null;
    },
  });
}

export function registerDocumentSymbols(symbols) {
  monaco.languages.registerDocumentSymbolProvider("java", {
    provideDocumentSymbols(model) {
      return symbols(model.getValue());
    },
  });
}

export function registerDocumentHighlight(highlight) {
  monaco.languages.registerDocumentHighlightProvider("java", {
    provideDocumentHighlights(model, position) {
      return highlight(model.getValue(), position.lineNumber, position.column);
    },
  });
}

export function registerDefinition(definition) {
  monaco.languages.registerDefinitionProvider("java", {
    provideDefinition(model, position) {
      // Resolves to a Location or null; Monaco accepts the Thenable directly.
      return definition(model.getValue(), position.lineNumber, position.column);
    },
  });
}

export function registerReferences(references) {
  monaco.languages.registerReferenceProvider("java", {
    provideReferences(model, position, context) {
      return references(
        model.getValue(),
        position.lineNumber,
        position.column,
        context.includeDeclaration,
      );
    },
  });
}

// --- JsValue factories for provider results (plain Monaco payload objects). ---

export function hoverResult(markdown) {
  return { contents: [{ value: markdown }] };
}

export function completionItem(label, kind, detail) {
  return { label, kind, detail: detail || undefined, insertText: label };
}

export function signatureInfo(label, paramOffsets) {
  return { label, parameters: paramOffsets.map((o) => ({ label: o })) };
}

export function signatureHelpResult(
  signatures,
  activeSignature,
  activeParameter,
) {
  return { signatures, activeSignature, activeParameter };
}

export function symbolNode(
  name,
  kind,
  startLine,
  startColumn,
  endLine,
  endColumn,
  children,
) {
  const range = {
    startLineNumber: startLine,
    startColumn,
    endLineNumber: endLine,
    endColumn,
  };
  return {
    name,
    detail: "",
    kind,
    tags: [],
    range,
    selectionRange: range,
    children: children.length ? children : undefined,
  };
}

export function highlightResult(
  startLine,
  startColumn,
  endLine,
  endColumn,
  write,
) {
  return {
    range: {
      startLineNumber: startLine,
      startColumn,
      endLineNumber: endLine,
      endColumn,
    },
    // monaco.languages.DocumentHighlightKind: Read = 1, Write = 2.
    kind: write ? 2 : 1,
  };
}

export function locationResult(
  path,
  startLine,
  startColumn,
  endLine,
  endColumn,
) {
  return {
    uri: pathToUri(path),
    range: {
      startLineNumber: startLine,
      startColumn,
      endLineNumber: endLine,
      endColumn,
    },
  };
}
