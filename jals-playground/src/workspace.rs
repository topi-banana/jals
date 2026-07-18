//! The in-browser workspace: a thin Monaco adapter over the shared [`jals_editor::Editor`].
//!
//! All analysis state and sequencing — per-file parse/resolution caches, incremental index
//! rebuilds, classpath and feature-set folding — lives in `jals-editor`'s protocol-neutral
//! [`Workspace`](jals_editor::Workspace); this type only pairs it with the [`MonacoHost`] (so
//! every query answers directly in Monaco payload shapes) and tracks which file the editor shows.
//! Every analyzing method is `async` (the shared core is): callers serialize access through one
//! `futures::lock::Mutex` and hold it across the awaits. The live buffer is reflected with
//! [`sync_active`](Workspace::sync_active) — a no-op when the text is unchanged, so a hover storm
//! over an idle buffer never re-analyzes.

use jals_config::FeatureSet;
use jals_config::fmt::Config as FmtConfig;
use jals_config::lint::Config as LintConfig;
use jals_editor::{Editor, ProjectLayout};
use jals_exec::Exec;
use jals_fmt::FormatOutput;
use jals_hir::LoweredClasspath;
use jals_storage::{
    ArtifactCache, CodeTree, DirKey, Entry, EntryRef, FileKey, MemoryCache, MemorySource,
    MemoryStorage,
};
use jals_syntax::Parse;

use crate::host::{
    CompletionEntry, Highlight, MonacoHost, PlaygroundDiagnostic, SigHelp, SymbolNode, Target,
};

/// Seed files, deliberately unformatted so the formatter has visible work to do, and
/// cross-referencing so the project index resolves `Main`'s use of `Greeter` across files.
const SAMPLE_FILES: &[(&str, &str)] = &[
    (
        "com/example/Greeter.java",
        "package com.example;\n\
         public class Greeter {\n\
         private final String name;\n\
         public Greeter(String name){this.name=name;}\n\
         public String greet(){return \"Hello, \"+name+\"!\";}\n\
         }\n",
    ),
    (
        "com/example/Main.java",
        "package com.example;\n\
         public class Main {\n\
         public static void main(String[] args){\n\
         String who=args.length>0?\"there\":\"world\";\n\
         Greeter g=new Greeter(who);\n\
         System.out.println(g.greet());\n\
         }\n\
         }\n",
    ),
];

/// The shared editor core driven through the [`MonacoHost`], plus the path of the active file.
///
/// The core's [`MemoryStorage`] is the single source of truth for files, overlays, and artifacts —
/// the sidebar and Monaco models read the same revision, so there is no parallel state to sync.
pub struct Workspace {
    editor: Editor<MemorySource, MemoryCache, MonacoHost>,
    /// Path of the active file — a key into the core's tree, and the editor's backing store.
    active: FileKey,
}

impl Workspace {
    /// The execution context the aggregate runs on: the browser runtime in the real playground
    /// (`spawn_local` tasks; yields escape to the macrotask queue so the page paints during long
    /// parses), the inline executor for host-side tests.
    fn exec() -> Exec {
        #[cfg(target_arch = "wasm32")]
        {
            Exec::wasm()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Exec::inline()
        }
    }

    /// A workspace seeded with the [`SAMPLE_FILES`]; the first (sorted) file is active.
    pub async fn new() -> Self {
        let tree = CodeTree::new(SAMPLE_FILES.iter().map(|(path, text)| {
            Entry::File(
                FileKey::parse(path).expect("sample path is valid"),
                text.as_bytes().to_vec(),
            )
        }))
        .expect("sample tree is valid");
        let storage = MemoryStorage::open(
            MemorySource::new(tree),
            MemoryCache::default(),
            Self::exec(),
        )
        .await
        .expect("an in-memory snapshot is immediate and infallible");
        // The whole tree is one source root (the sample files live at its top).
        let editor =
            Editor::load(storage, ProjectLayout::new(vec![DirKey::ROOT]), MonacoHost).await;
        // The first (sorted) indexed file is active on load.
        let active = editor
            .workspace()
            .files()
            .map(|(path, _)| path.clone())
            .next()
            .expect("sample contains a Java file");
        Workspace { editor, active }
    }

    /// Replace the external classpath folded into analysis (from a resolved `[dependencies]`
    /// spec), or clear it with `None`. The index rebuilds immediately — `Main`'s use of a library
    /// type then resolves through the downloaded `.class` files.
    pub async fn set_classpath(&mut self, classpath: Option<LoweredClasspath>) {
        self.editor
            .workspace_mut()
            .set_classpath(classpath.unwrap_or_default())
            .await;
    }

    /// Replace the resolved language feature set (from `[package] features`); the core folds it
    /// into every diagnostics run, so the feature-gated lint rules fire without any host plumbing.
    pub fn set_feature_set(&mut self, feature_set: FeatureSet) {
        self.editor.workspace_mut().set_feature_set(feature_set);
    }

    /// Clone one immutable source revision together with its cache for an async dependency task.
    pub fn storage_snapshot(&self) -> MemoryStorage {
        self.editor.workspace().storage().clone()
    }

    /// Merge artifacts produced by a detached dependency task back into the aggregate.
    pub fn replace_artifacts(&mut self, artifacts: ArtifactCache<MemoryCache>) {
        self.editor
            .workspace_mut()
            .storage_mut()
            .replace_artifacts(artifacts);
    }

    /// The path of the active file.
    pub fn active(&self) -> &FileKey {
        &self.active
    }

    /// Make `path` the active file, if it exists in the tree.
    pub fn set_active(&mut self, path: &str) {
        if let Ok(key) = FileKey::parse(path)
            && self.editor.workspace().view().tree().file(&key).is_some()
        {
            self.active = key;
        }
    }

    /// The active file's cached [`jals_editor::Document`] (the synced overlay), when indexed.
    fn active_document(&self) -> Option<&jals_editor::Document> {
        let workspace = self.editor.workspace();
        workspace.document(workspace.file_id(&self.active)?)
    }

    /// The active file's current text — the synced overlay, falling back to the tree (empty if it
    /// somehow cannot be read).
    pub fn active_source(&self) -> String {
        self.active_document()
            .map(|doc| doc.text.to_string())
            .unwrap_or_else(|| self.read(&self.active))
    }

    /// Reflect the editor's live buffer into the analysis overlay — a no-op when `text` matches
    /// the cached copy, so a query storm over an unchanged buffer never re-parses or re-indexes.
    /// Called on every keystroke and by the providers before every query.
    pub async fn sync_active(&mut self, text: &str) {
        let _ = self
            .editor
            .workspace_mut()
            .sync_overlay(&self.active, text)
            .await;
    }

    /// Every indexed file as `(path, text)`, sorted — for seeding the editor's per-file Monaco
    /// models. Answered from the core's index, so the set always matches what analysis sees.
    pub fn file_texts(&self) -> Vec<(String, String)> {
        self.editor
            .workspace()
            .files()
            .map(|(path, doc)| (path.to_string(), doc.text.to_string()))
            .collect()
    }

    /// The immediate children of directory `dir` as `(full path, is directory)`, sorted
    /// (sidebar rendering).
    pub fn read_dir(&self, dir: &str) -> Vec<(String, bool)> {
        let Ok(dir) = DirKey::parse(dir) else {
            return Vec::new();
        };
        self.editor
            .workspace()
            .view()
            .tree()
            .children(&dir)
            .map(|child| match child {
                EntryRef::Directory(directory) => (directory.to_string(), true),
                EntryRef::File(file) => (file.key().to_string(), false),
            })
            .collect()
    }

    fn read(&self, path: &FileKey) -> String {
        self.editor
            .workspace()
            .view()
            .file_text(path)
            .unwrap_or_default()
            .to_owned()
    }

    /// Format the active file (file-local; no project index needed).
    pub async fn format_active(&self, config: &FmtConfig) -> FormatOutput {
        jals_fmt::FormatOutput::format_source(&self.active_source(), config).await
    }

    /// Parse the active file for the syntax-tree dump.
    pub async fn syntax_active(&self) -> Parse {
        jals_syntax::Parse::parse(&self.active_source()).await
    }

    /// The active file's diagnostics — syntax errors, lint findings (the project's feature set
    /// folds in inside the core), and cross-file unresolved-type / type-mismatch errors — already
    /// in Monaco coordinates, so the UI layer only marshals them.
    pub async fn analyze_active(&self, config: &LintConfig) -> Vec<PlaygroundDiagnostic> {
        self.editor.diagnostics(&self.active, config).await
    }

    /// The hover for the cursor at the Monaco position `(line, col)` in the active file: the
    /// inferred type there, rendered as a Java code block. `None` for nothing informative.
    pub async fn hover(&self, line: u32, col: u32) -> Option<String> {
        self.editor.hover(&self.active, &(line, col)).await
    }

    /// Completions for the cursor at `(line, col)` in the active file: the members after a `.`,
    /// otherwise the in-scope bindings and project types plus the Java keywords.
    pub async fn completions(&self, line: u32, col: u32) -> Vec<CompletionEntry> {
        self.editor.completions(&self.active, &(line, col)).await
    }

    /// Signature help for the call at `(line, col)` in the active file, with cross-file type
    /// resolution. `None` if the cursor is in no resolvable call.
    pub async fn signature_help(&self, line: u32, col: u32) -> Option<SigHelp> {
        self.editor.signature_help(&self.active, &(line, col)).await
    }

    /// The document-symbol outline of the active file (types with their members nested).
    pub fn document_symbols(&self) -> Vec<SymbolNode> {
        self.editor.outline(&self.active)
    }

    /// Occurrence highlights for the cursor at `(line, col)` in the active file. Empty if the
    /// cursor is not on an identifier.
    pub async fn document_highlight(&self, line: u32, col: u32) -> Vec<Highlight> {
        self.editor.highlights(&self.active, &(line, col)).await
    }

    /// Go-to-definition for the cursor at `(line, col)` in the active file: a file-local binding,
    /// then the project type a reference names, then — for a member access — the member the
    /// receiver type declares. `None` if nothing resolves.
    pub async fn goto_definition(&self, line: u32, col: u32) -> Option<Target> {
        self.editor.definition(&self.active, &(line, col)).await
    }

    /// Find-references for the cursor at `(line, col)` in the active file: every occurrence of
    /// the symbol under the cursor — across the whole project when it is a project type, or
    /// within this one file for a file-local binding. The declaration is included when
    /// `include_declaration`. Empty if the cursor is on no resolvable symbol.
    pub async fn references(&self, line: u32, col: u32, include_declaration: bool) -> Vec<Target> {
        self.editor
            .references(&self.active, &(line, col), include_declaration)
            .await
    }
}

#[cfg(test)]
mod tests {
    use jals_editor::CompletionKind;
    use jals_exec::block_on_inline;

    use crate::host::MonacoRange;

    use super::*;

    #[test]
    fn seed_files_parse_clean() {
        for (path, contents) in SAMPLE_FILES {
            let parse = block_on_inline(jals_syntax::Parse::parse(contents));
            assert!(
                parse.errors().is_empty(),
                "seed file {path} has syntax errors: {:?}",
                parse.errors()
            );
        }
    }

    #[test]
    fn tree_lists_the_package_then_the_files() {
        let ws = block_on_inline(Workspace::new());
        assert_eq!(ws.read_dir(""), vec![("com".to_string(), true)]);
        assert_eq!(ws.read_dir("com"), vec![("com/example".to_string(), true)]);
        assert_eq!(
            ws.read_dir("com/example"),
            vec![
                ("com/example/Greeter.java".to_string(), false),
                ("com/example/Main.java".to_string(), false),
            ]
        );
        // The first sorted file is active on load.
        assert_eq!(ws.active().to_string(), "com/example/Greeter.java");
    }

    #[test]
    fn cross_file_reference_resolves_and_seed_is_clean() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            ws.set_active("com/example/Main.java");
            let diags = ws.analyze_active(&LintConfig::default()).await;
            // `Greeter` (another file), `String`/`System` (stdlib stubs) all resolve — the seed
            // must stay clean so the diagnostic-free demo holds.
            assert!(
                diags.is_empty(),
                "seed workspace should be diagnostic-free, got: {:?}",
                diags.iter().map(|d| &d.message).collect::<Vec<_>>()
            );
        });
    }

    #[test]
    fn diagnostics_carry_one_based_ranges_and_the_code_prefix() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            ws.set_active("com/example/Main.java");
            // Introduce a reference to a type declared nowhere in the workspace.
            ws.sync_active(
                "package com.example;\npublic class Main { void f(){ Missing m = null; } }\n",
            )
            .await;
            let src = ws.active_source();
            let diags = ws.analyze_active(&LintConfig::default()).await;
            let diag = diags
                .iter()
                .find(|d| d.message.contains("Missing"))
                .unwrap_or_else(|| {
                    panic!(
                        "expected an unresolved-type diagnostic for `Missing`, got: {:?}",
                        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
                    )
                });
            // The adapter prefixes the producing code onto the neutral message...
            assert!(
                diag.message.starts_with("cannot-resolve: "),
                "{}",
                diag.message
            );
            // ...and maps the byte range to one-based Monaco coordinates.
            let (line, col) = monaco_pos(&src, src.find("Missing").unwrap());
            assert_eq!((diag.range.start_line, diag.range.start_col), (line, col));
            assert_eq!(diag.range.start_line, 2);
        });
    }

    #[test]
    fn format_active_rewrites_messy_source() {
        block_on_inline(async {
            let ws = Workspace::new().await;
            let out = ws.format_active(&FmtConfig::default()).await;
            assert!(out.formatted.contains("class Greeter"));
            // The seed is deliberately unformatted, so formatting must change it.
            assert_ne!(out.formatted, ws.active_source());
        });
    }

    #[test]
    fn sync_active_no_op_keeps_the_cached_parse() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            let src = ws.active_source();
            let before = std::sync::Arc::as_ptr(&ws.active_document().expect("indexed").parse);
            // Unchanged text: the overlay (and its parse) is untouched — a hover storm
            // re-analyzes nothing.
            ws.sync_active(&src).await;
            assert_eq!(
                std::sync::Arc::as_ptr(&ws.active_document().expect("indexed").parse),
                before
            );
            // A real edit replaces the document.
            ws.sync_active("package com.example;\npublic class Greeter { }\n")
                .await;
            assert_ne!(
                std::sync::Arc::as_ptr(&ws.active_document().expect("indexed").parse),
                before
            );
        });
    }

    /// The Monaco `(line, col)` position of byte `offset` within `text` (one-based UTF-16).
    fn monaco_pos(text: &str, offset: usize) -> (u32, u32) {
        let index = jals_editor::LineIndex::new(text);
        let range = MonacoRange::of(&index, text, &(offset..offset));
        (range.start_line, range.start_col)
    }

    #[test]
    fn hover_shows_inferred_type() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            ws.set_active("com/example/Main.java");
            let src = ws.active_source();
            // The `g` receiver in `g.greet()` is a local of the cross-file type `Greeter`.
            let (line, col) = monaco_pos(&src, src.find("g.greet").unwrap());
            assert_eq!(
                ws.hover(line, col).await,
                Some("```java\nGreeter\n```".to_string())
            );
        });
    }

    #[test]
    fn goto_definition_navigates_cross_file() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            ws.set_active("com/example/Main.java");
            let src = ws.active_source();
            // The type name `Greeter` in `Greeter g` is declared in the other file.
            let (line, col) = monaco_pos(&src, src.find("Greeter g").unwrap());
            let target = ws.goto_definition(line, col).await.expect("type resolves");
            assert_eq!(target.path, "com/example/Greeter.java");
            // The range maps against the *target* file's text: `class Greeter` sits on its
            // line 2.
            assert_eq!(target.range.start_line, 2);
        });
    }

    #[test]
    fn references_span_the_workspace() {
        block_on_inline(async {
            let ws = Workspace::new().await;
            // `Greeter` is the default active file; anchor on its class-name declaration.
            let src = ws.active_source();
            let (line, col) = monaco_pos(&src, src.find("Greeter {").unwrap());

            let without_decl = ws.references(line, col, false).await;
            // Used twice in `Main.java`: `Greeter g` and `new Greeter(who)`.
            let main_refs = without_decl
                .iter()
                .filter(|t| t.path == "com/example/Main.java")
                .count();
            assert_eq!(main_refs, 2, "got {without_decl:?}");

            // Including the declaration adds exactly one more target, in `Greeter.java`.
            let with_decl = ws.references(line, col, true).await;
            assert_eq!(with_decl.len(), without_decl.len() + 1);
            assert!(
                with_decl
                    .iter()
                    .any(|t| t.path == "com/example/Greeter.java")
            );
        });
    }

    #[test]
    fn document_symbols_lists_the_type_and_members() {
        let ws = block_on_inline(Workspace::new()); // `Greeter.java` is active by default.
        let syms = ws.document_symbols();
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Greeter");
        let child_names: Vec<&str> = syms[0].children.iter().map(|c| c.name.as_str()).collect();
        assert!(child_names.contains(&"name"), "got {child_names:?}"); // field
        assert!(child_names.contains(&"Greeter"), "got {child_names:?}"); // constructor
        assert!(child_names.contains(&"greet"), "got {child_names:?}"); // method
    }

    #[test]
    fn completions_after_dot_list_members_without_keywords() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            ws.set_active("com/example/Main.java");
            let src = ws.active_source();
            // Just after `g.` in `g.greet()`.
            let (line, col) = monaco_pos(&src, src.find("g.greet").unwrap() + 2);
            let entries = ws.completions(line, col).await;
            assert!(
                entries
                    .iter()
                    .any(|e| e.label == "greet" && e.kind == CompletionKind::Method)
            );
            // A member-access context never offers keywords.
            assert!(entries.iter().all(|e| e.kind != CompletionKind::Keyword));
        });
    }

    #[test]
    fn signature_help_marks_the_active_parameter() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            ws.set_active("com/example/Main.java");
            ws.sync_active(
                "package com.example;\npublic class C { int area(int w, int h){return 0;} void g(){ area(1, ); } }\n",
            )
            .await;
            let src = ws.active_source();
            let (line, col) = monaco_pos(&src, src.find("area(1, ").unwrap() + "area(1, ".len());
            let help = ws.signature_help(line, col).await.expect("inside a call");
            assert_eq!(help.signatures.len(), 1);
            assert_eq!(help.signatures[0].label, "area(int w, int h)");
            // The parameter spans are UTF-16 offsets into the label: `int h` follows
            // `area(int w, `.
            assert_eq!(help.signatures[0].parameters.len(), 2);
            assert_eq!(help.active_parameter, 1);
        });
    }

    #[test]
    fn document_highlight_covers_a_local() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            ws.set_active("com/example/Main.java");
            let src = ws.active_source();
            // `who` is declared (`String who=...`) and used (`new Greeter(who)`).
            let (line, col) = monaco_pos(&src, src.find("who=").unwrap());
            let highlights = ws.document_highlight(line, col).await;
            assert_eq!(highlights.len(), 2, "got {highlights:?}");
            assert!(highlights.iter().any(|h| h.write)); // the declaration
            assert!(highlights.iter().any(|h| !h.write)); // the use
        });
    }

    #[test]
    fn folded_classpath_resolves_an_external_library_type() {
        block_on_inline(async {
            // A compiled `Box<T>` fed to the same wasm-compatible core the browser uses — loaded
            // off an in-memory tree, then lowered for the index. This is the payoff of external
            // dependencies in the playground: a library type resolves without any of its `.java`
            // in the workspace.
            let key = FileKey::parse("deps/Box.class").unwrap();
            let storage = MemoryStorage::memory(
                CodeTree::new([Entry::File(
                    key.clone(),
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../jals-classpath/tests/fixtures/Box.class"
                    ))
                    .to_vec(),
                )])
                .unwrap(),
            );
            let load = jals_classpath::ClasspathLoad::load(
                storage.exec(),
                &storage.view(),
                storage.artifacts(),
                &[jals_classpath::ClasspathEntry::ProjectFile(key)],
            )
            .await;
            assert!(load.warnings.is_empty(), "{:?}", load.warnings);
            let lowered = jals_hir::ProjectIndex::lower_classpath(&load.classes).await;

            let mut ws = Workspace::new().await;
            ws.set_active("com/example/Main.java");
            // A default-package class using the external `Box` type (the package `Box.class`
            // declares).
            ws.sync_active("class Uses { void f() { Box<String> b = null; } }\n")
                .await;

            // Unresolved before folding the classpath: no `Box` declaration exists in the
            // workspace.
            let before = ws.analyze_active(&LintConfig::default()).await;
            assert!(
                before.iter().any(|d| d.message.contains("Box")),
                "expected `Box` unresolved before folding the classpath, got: {:?}",
                before.iter().map(|d| &d.message).collect::<Vec<_>>()
            );

            // Resolved after folding `Box.class` in.
            ws.set_classpath(Some(lowered)).await;
            let after = ws.analyze_active(&LintConfig::default()).await;
            assert!(
                !after.iter().any(|d| d.message.contains("Box")),
                "expected `Box` to resolve once the classpath is folded, got: {:?}",
                after.iter().map(|d| &d.message).collect::<Vec<_>>()
            );
        });
    }
}
