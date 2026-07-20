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

use jals_build::build_script::{
    BuildScriptEnvironment, BuildScriptError, BuildScriptLimits, BuildScriptOutput,
    BuildScriptSession, clear_build_script_outputs,
};
use jals_config::fmt::Config as FmtConfig;
use jals_config::lint::Config as LintConfig;
use jals_config::{BuildScript, FeatureSet, Manifest};
use jals_editor::{Editor, ProjectLayout};
use jals_exec::Exec;
use jals_fmt::FormatOutput;
use jals_hir::LoweredClasspath;
use jals_project::{
    BuildTaskExecutor, BuildTaskHost, RootBuildScriptError, RootBuildScriptOptions,
};
use jals_storage::{
    ArtifactCache, CodeTree, DirKey, Entry, FileKey, MemoryCache, MemorySource, MemoryStorage,
};
use jals_syntax::Parse;

use crate::fetcher::BrowserFetcher;
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

pub const MANIFEST_PATH: &str = "jals.toml";
pub const BUILD_SCRIPT_PATH: &str = "build.rhai";

/// The shared editor core driven through the [`MonacoHost`], plus the path of the active file.
///
/// The core's [`MemoryStorage`] is the single source of truth for files, overlays, and artifacts —
/// the sidebar and Monaco models read the same revision, so there is no parallel state to sync.
pub struct Workspace {
    editor: Editor<MemorySource, MemoryCache, MonacoHost>,
    /// Path of the active file — a key into the core's tree, and the editor's backing store.
    active: FileKey,
    /// Aggregate-local knowledge of files published by earlier Rhai executions.
    build_script_session: BuildScriptSession,
    /// Configured script path whose editor buffer is currently staged as an overlay.
    staged_script: Option<FileKey>,
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
        let source_root = DirKey::parse("com/example").expect("sample source root is valid");
        let editor = Editor::load(storage, ProjectLayout::new(vec![source_root]), MonacoHost).await;
        // The first (sorted) indexed file is active on load.
        let active = editor
            .workspace()
            .files()
            .map(|(path, _)| path.clone())
            .next()
            .expect("sample contains a Java file");
        Workspace {
            editor,
            active,
            build_script_session: BuildScriptSession::new(),
            staged_script: None,
        }
    }

    /// Clone one immutable source revision together with its cache for an async dependency task.
    pub fn storage_snapshot(&self) -> MemoryStorage {
        self.editor.workspace().storage().clone()
    }

    /// Install one coherent browser-resolution result. Feature metadata and verified artifacts are
    /// visible before the async index rebuild starts; once mutation begins, the whole operation runs
    /// to completion under the playground workspace lock.
    pub async fn apply_project_inputs(
        &mut self,
        classpath: LoweredClasspath,
        feature_set: FeatureSet,
        artifacts: ArtifactCache<MemoryCache>,
        library_sources: Vec<(FileKey, String)>,
        source_dep_sources: Vec<(FileKey, String)>,
    ) {
        let workspace = self.editor.workspace_mut();
        workspace.set_feature_set(feature_set);
        workspace.storage_mut().replace_artifacts(artifacts);
        workspace
            .set_dependency_source_texts(library_sources, source_dep_sources)
            .await;
        workspace.set_classpath(classpath).await;
    }

    /// Stage the live manifest and Rhai buffers into this workspace's own aggregate, execute the
    /// configured script there, then reload project Java files so generated sources join analysis.
    /// Existing Java overlays remain in storage and therefore survive the reload.
    pub async fn run_build_script(
        &mut self,
        manifest: &Manifest,
        manifest_text: &str,
        script_text: &str,
    ) -> Result<Option<BuildScriptOutput>, BuildScriptError> {
        let manifest_key = FileKey::parse(MANIFEST_PATH).expect("manifest pseudo-path is valid");
        let configured_script = match manifest.build.script.as_ref() {
            Some(BuildScript::Rhai { file }) => {
                Some(
                    FileKey::parse(file).map_err(|error| BuildScriptError::InvalidScriptPath {
                        path: file.clone(),
                        reason: format!("{error:?}"),
                    })?,
                )
            }
            None => None,
        };
        let script_changed = self.staged_script != configured_script;
        if script_changed {
            if let Some(old_script) = &self.staged_script {
                let workspace = self.editor.workspace_mut();
                let storage = workspace.storage_mut();
                storage
                    .remove_overlay(storage.revision(), old_script)
                    .map_err(|error| BuildScriptError::Storage {
                        operation: "remove the previous playground script overlay",
                        error,
                    })?;
            }
            // If staging the replacement below fails, no hidden old alias remains tracked.
            self.staged_script = None;
        }
        let mut overlays = vec![(manifest_key, manifest_text.as_bytes().to_vec())];
        if let Some(script) = &configured_script {
            overlays.push((script.clone(), script_text.as_bytes().to_vec()));
        }

        let output = {
            let workspace = self.editor.workspace_mut();
            let storage = workspace.storage_mut();
            storage
                .set_overlays(storage.revision(), overlays)
                .map_err(|error| BuildScriptError::Storage {
                    operation: "stage the playground manifest and script",
                    error,
                })?;
            self.staged_script.clone_from(&configured_script);
            let environment = BuildScriptEnvironment::new().for_project(manifest);
            if manifest.build.script.is_none() {
                clear_build_script_outputs(storage, &mut self.build_script_session).await?;
                None
            } else {
                let root_output = BuildTaskExecutor::execute_root(
                    &Self::exec(),
                    &BrowserFetcher::new(String::new()),
                    storage,
                    &mut self.build_script_session,
                    RootBuildScriptOptions {
                        manifest,
                        environment: &environment,
                        limits: &BuildScriptLimits::default(),
                        network: jals_classpath::NetworkPolicy::Online,
                        host: BuildTaskHost::NoTerminals,
                        blocked_files: &[],
                    },
                )
                .await
                .map_err(|error| match error {
                    RootBuildScriptError::BuildScript(error) => error,
                    other => BuildScriptError::Execute {
                        script: configured_script
                            .clone()
                            .expect("a configured script was parsed"),
                        position: None,
                        message: other.to_string(),
                    },
                })?;
                debug_assert!(root_output.task_classpath.is_empty());
                root_output.script
            }
        };
        let project_sources = output.as_ref().map_or_else(Vec::new, |output| {
            output.generated_sources.iter().cloned().collect()
        });
        let workspace = self.editor.workspace_mut();
        workspace.set_project_sources(project_sources);
        workspace.reload_project_files().await;
        self.ensure_active_indexed();
        Ok(output)
    }

    /// Keep the active anchor inside the reloaded Java index. Generated-file cleanup falls back to
    /// the first sorted project file, which is deterministic across runtimes.
    fn ensure_active_indexed(&mut self) {
        let workspace = self.editor.workspace();
        if workspace.file_id(&self.active).is_none() {
            self.active = workspace
                .files()
                .map(|(path, _)| path.clone())
                .next()
                .expect("the playground retains its seed Java files");
        }
    }

    /// The path of the active file.
    pub fn active(&self) -> &FileKey {
        &self.active
    }

    /// Make `path` the active indexed Java file. Returns whether the selection was accepted.
    pub fn set_active(&mut self, path: &str) -> bool {
        let Ok(key) = FileKey::parse(path) else {
            return false;
        };
        if self.editor.workspace().file_id(&key).is_none() {
            return false;
        }
        self.active = key;
        true
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

    /// Indexed project paths in deterministic order, used to construct the sidebar in one pass.
    pub fn file_keys(&self) -> impl Iterator<Item = &FileKey> {
        self.editor.workspace().files().map(|(path, _)| path)
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
    use jals_build::build_script::RHAI_OUTPUT_ROOT;
    use jals_editor::CompletionKind;
    use jals_exec::block_on_inline;

    use crate::host::MonacoRange;

    use super::*;

    const BUILD_MANIFEST: &str = "[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n";

    fn build_manifest() -> Manifest {
        BUILD_MANIFEST.parse().expect("test manifest is valid")
    }

    fn output_key(path: &str) -> FileKey {
        FileKey::parse(&format!("{RHAI_OUTPUT_ROOT}/{path}")).expect("test output path is valid")
    }

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
    fn indexed_file_keys_are_sorted() {
        let ws = block_on_inline(Workspace::new());
        assert_eq!(
            ws.file_keys().map(ToString::to_string).collect::<Vec<_>>(),
            vec![
                "com/example/Greeter.java".to_string(),
                "com/example/Main.java".to_string(),
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
    fn only_registered_generated_java_is_indexed_and_resolves_from_unsaved_source() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            ws.set_active("com/example/Main.java");
            ws.sync_active("package com.example;\npublic class Main { Generated value; }\n")
                .await;
            let generated = output_key("com/example/Generated.java");
            let unregistered = output_key("com/example/Unregistered.java");

            let output = ws
                .run_build_script(
                    &build_manifest(),
                    BUILD_MANIFEST,
                    r#"
                        let source = output.write_text(
                            "com/example/Generated.java",
                            "package com.example; public class Generated {}\n"
                        );
                        output.write_text(
                            "com/example/Unregistered.java",
                            "package com.example; public class Unregistered {}\n"
                        );
                        build.add_source(source);
                    "#,
                )
                .await
                .expect("build script succeeds")
                .expect("script is configured");

            assert!(output.generated_sources.contains(&generated));
            assert!(!output.generated_sources.contains(&unregistered));
            assert!(output.generated_files.contains(&generated));
            assert!(output.generated_files.contains(&unregistered));
            let view = ws.editor.workspace().view();
            assert!(view.file(&generated).is_ok());
            assert!(view.file(&unregistered).is_ok());
            assert!(
                ws.file_texts()
                    .iter()
                    .any(|(path, _)| path == &generated.to_string())
            );
            assert!(
                ws.file_texts()
                    .iter()
                    .all(|(path, _)| path != &unregistered.to_string())
            );
            assert!(ws.editor.workspace().file_id(&generated).is_some());
            assert!(ws.editor.workspace().file_id(&unregistered).is_none());
            assert_eq!(
                ws.active_source(),
                "package com.example;\npublic class Main { Generated value; }\n",
                "reloading generated files must preserve the unsaved Java overlay"
            );
            let diagnostics = ws.analyze_active(&LintConfig::default()).await;
            assert!(
                !diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains("Generated")),
                "generated type should resolve after reload: {:?}",
                diagnostics
                    .iter()
                    .map(|diagnostic| &diagnostic.message)
                    .collect::<Vec<_>>()
            );
        });
    }

    #[test]
    fn build_script_environment_matches_host_project_metadata() {
        block_on_inline(async {
            let manifest_text = "[package]\nname = \"playground\"\nversion = \"1.2.3\"\n\
                                 [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n";
            let manifest: Manifest = manifest_text.parse().expect("test manifest is valid");
            let mut ws = Workspace::new().await;
            ws.run_build_script(
                &manifest,
                manifest_text,
                r#"
                    if build.env("OUT_DIR") != "target/jals/build/rhai/out" {
                        throw "bad OUT_DIR";
                    }
                    if build.env("JALS_MANIFEST_DIR") != "." {
                        throw "bad manifest directory";
                    }
                    if build.env("JALS_PACKAGE_NAME") != "playground" {
                        throw "bad package name";
                    }
                    if build.env("JALS_PACKAGE_VERSION") != "1.2.3" {
                        throw "bad package version";
                    }
                "#,
            )
            .await
            .expect("host-parity environment is visible to Rhai");

            ws.run_build_script(
                &build_manifest(),
                BUILD_MANIFEST,
                r#"
                    if build.env("JALS_PACKAGE_NAME") != () ||
                       build.env("JALS_PACKAGE_VERSION") != () {
                        throw "optional package metadata must be absent";
                    }
                "#,
            )
            .await
            .expect("omitted package metadata stays absent");
        });
    }

    #[test]
    fn failed_rerun_publishes_no_partial_outputs() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            let stable = output_key("Stable.java");
            let partial = output_key("Partial.java");
            ws.run_build_script(
                &build_manifest(),
                BUILD_MANIFEST,
                r#"output.write_text("Stable.java", "class Stable {}\n");"#,
            )
            .await
            .expect("initial build succeeds");

            let result = ws
                .run_build_script(
                    &build_manifest(),
                    BUILD_MANIFEST,
                    r#"
                        output.write_text("Stable.java", "class Changed {}\n");
                        output.write_text("Partial.java", "class Partial {}\n");
                        throw "stop";
                    "#,
                )
                .await;

            assert!(result.is_err());
            let view = ws.editor.workspace().view();
            assert_eq!(
                view.file(&stable).expect("prior output remains").bytes(),
                b"class Stable {}\n"
            );
            assert!(view.file(&partial).is_err());
        });
    }

    #[test]
    fn physical_task_publication_is_rejected_before_browser_fetch() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            let error = ws
                .run_build_script(
                    &build_manifest(),
                    BUILD_MANIFEST,
                    r#"
                        let jar = tasks.fetch_jar(
                            tasks.https_url("https://example.invalid/sources.jar"),
                            tasks.sha256("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                            tasks.bytes(1024)
                        );
                        let sources = tasks.extract_java(jar, "net/example");
                        tasks.publish_tree(
                            "sources",
                            sources,
                            "src/main/java/net/example",
                            "replace-root"
                        );
                    "#,
                )
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                BuildScriptError::Execute { message, .. }
                    if message.contains("physical source-tree publication is not supported")
            ));
            assert!(
                ws.storage_snapshot()
                    .view()
                    .file(&FileKey::parse("src/main/java/net/example/A.java").unwrap())
                    .is_err()
            );
        });
    }

    #[test]
    fn rerun_replaces_outputs_and_removes_known_stale_files() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            let a = output_key("A.java");
            let b = output_key("B.java");
            let c = output_key("C.java");
            ws.run_build_script(
                &build_manifest(),
                BUILD_MANIFEST,
                r#"
                    let a = output.write_text("A.java", "class A { int oldValue; }\n");
                    let b = output.write_text("B.java", "class B {}\n");
                    build.add_source(a);
                    build.add_source(b);
                "#,
            )
            .await
            .expect("initial build succeeds");

            ws.run_build_script(
                &build_manifest(),
                BUILD_MANIFEST,
                r#"
                    let a = output.write_text("A.java", "class A { int newValue; }\n");
                    let c = output.write_text("C.java", "class C {}\n");
                    build.add_source(a);
                    build.add_source(c);
                "#,
            )
            .await
            .expect("rerun succeeds");

            let view = ws.editor.workspace().view();
            assert_eq!(
                view.file(&a).expect("A is replaced").bytes(),
                b"class A { int newValue; }\n"
            );
            assert!(view.file(&b).is_err(), "known stale B must be removed");
            assert_eq!(
                view.file(&c).expect("C is generated").bytes(),
                b"class C {}\n"
            );
            let indexed: Vec<_> = ws.file_texts().into_iter().map(|(path, _)| path).collect();
            assert!(indexed.contains(&a.to_string()));
            assert!(!indexed.contains(&b.to_string()));
            assert!(indexed.contains(&c.to_string()));
        });
    }

    #[test]
    fn disabling_script_clears_outputs_and_repairs_removed_active_file() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            let generated = output_key("Generated.java");
            ws.run_build_script(
                &build_manifest(),
                BUILD_MANIFEST,
                r#"
                    let generated = output.write_text("Generated.java", "class Generated {}\n");
                    build.add_source(generated);
                "#,
            )
            .await
            .expect("generation succeeds");
            assert!(ws.set_active(&generated.to_string()));

            let output = ws
                .run_build_script(&Manifest::default(), "", "")
                .await
                .expect("disabling the script clears owned output");

            assert!(output.is_none());
            assert!(ws.editor.workspace().view().file(&generated).is_err());
            assert!(ws.file_keys().all(|path| path != &generated));
            assert_eq!(ws.active().to_string(), "com/example/Greeter.java");
            assert!(!ws.set_active(&generated.to_string()));
        });
    }

    #[test]
    fn script_overlay_moves_from_default_to_custom_then_is_removed_when_disabled() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            let default_key = FileKey::parse(BUILD_SCRIPT_PATH).unwrap();
            let custom_key = FileKey::parse("scripts/custom.rhai").unwrap();
            let generated = output_key("Custom.java");
            ws.run_build_script(&build_manifest(), BUILD_MANIFEST, "let first = 1;")
                .await
                .expect("fixed playground script path succeeds");
            let custom_text =
                "[build]\nscript = { type = \"rhai\", file = \"scripts/custom.rhai\" }\n";
            let custom: Manifest = custom_text.parse().expect("custom manifest is valid");

            let output = ws
                .run_build_script(
                    &custom,
                    custom_text,
                    r#"output.write_text("Custom.java", "class Custom {}\n");"#,
                )
                .await
                .expect("the editor buffer executes at the custom path")
                .expect("custom script is enabled");

            let view = ws.editor.workspace().view();
            assert!(view.file(&default_key).is_err());
            assert_eq!(
                view.file_text(&custom_key).unwrap(),
                r#"output.write_text("Custom.java", "class Custom {}\n");"#
            );
            assert!(output.generated_files.contains(&generated));
            assert_eq!(ws.staged_script.as_ref(), Some(&custom_key));

            let error = ws
                .run_build_script(&custom, custom_text, "let broken = ;")
                .await
                .expect_err("custom-path compile error is reported");
            assert_eq!(error.script_path(), Some(&custom_key));

            ws.run_build_script(&Manifest::default(), "", "")
                .await
                .expect("disabling clears the custom script and its outputs");

            let view = ws.editor.workspace().view();
            assert!(view.file(&default_key).is_err());
            assert!(view.file(&custom_key).is_err());
            assert!(view.file(&generated).is_err());
            assert!(ws.staged_script.is_none());
        });
    }

    #[test]
    fn staged_pseudo_files_are_not_indexed_or_listed_as_project_files() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            ws.run_build_script(&build_manifest(), BUILD_MANIFEST, "")
                .await
                .expect("empty script succeeds");

            let view = ws.editor.workspace().view();
            assert!(view.file(&FileKey::parse(MANIFEST_PATH).unwrap()).is_ok());
            assert!(
                view.file(&FileKey::parse(BUILD_SCRIPT_PATH).unwrap())
                    .is_ok()
            );
            assert!(
                ws.file_texts()
                    .iter()
                    .all(|(path, _)| path != MANIFEST_PATH && path != BUILD_SCRIPT_PATH)
            );
            assert!(
                ws.file_keys().all(|path| path.to_string() != MANIFEST_PATH
                    && path.to_string() != BUILD_SCRIPT_PATH)
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
            ws.editor.workspace_mut().set_classpath(lowered).await;
            let after = ws.analyze_active(&LintConfig::default()).await;
            assert!(
                !after.iter().any(|d| d.message.contains("Box")),
                "expected `Box` to resolve once the classpath is folded, got: {:?}",
                after.iter().map(|d| &d.message).collect::<Vec<_>>()
            );
        });
    }

    #[test]
    fn dependency_sources_are_navigable_but_never_enter_project_storage() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            ws.set_active("com/example/Main.java");
            ws.sync_active("package com.example; class Main { Dependency value; }")
                .await;
            let dependency =
                FileKey::parse(".jals/source-dependency/dependencies/node/sources/Dependency.java")
                    .unwrap();
            ws.apply_project_inputs(
                LoweredClasspath::default(),
                FeatureSet::default(),
                ArtifactCache::new(MemoryCache::default()),
                Vec::new(),
                vec![(
                    dependency.clone(),
                    "package com.example; class Dependency {}".to_string(),
                )],
            )
            .await;

            let source = ws.active_source();
            let (line, col) = monaco_pos(&source, source.find("Dependency value").unwrap());
            let target = ws
                .goto_definition(line, col)
                .await
                .expect("dependency source is an index and navigation input");
            assert_eq!(target.path, dependency.to_string());
            assert!(ws.editor.workspace().file_id(&dependency).is_none());
            assert!(ws.file_keys().all(|path| path != &dependency));
            assert!(ws.editor.workspace().view().file(&dependency).is_err());
            assert!(ws.storage_snapshot().view().file(&dependency).is_err());

            ws.run_build_script(
                &build_manifest(),
                BUILD_MANIFEST,
                r#"if project.exists(".jals") { throw "dependency source leaked"; }"#,
            )
            .await
            .expect("a later build script view excludes detached dependency sources");
            assert!(ws.storage_snapshot().view().file(&dependency).is_err());

            ws.apply_project_inputs(
                LoweredClasspath::default(),
                FeatureSet::default(),
                ArtifactCache::new(MemoryCache::default()),
                Vec::new(),
                Vec::new(),
            )
            .await;
            assert!(ws.editor.workspace().view().file(&dependency).is_err());
            assert!(ws.goto_definition(line, col).await.is_none());
        });
    }

    #[test]
    fn detached_dependency_text_never_replaces_same_key_project_bytes() {
        block_on_inline(async {
            let mut ws = Workspace::new().await;
            let collision = FileKey::parse(".jals/source-dependency/collision.java").unwrap();
            let revision = ws.editor.workspace().storage().revision();
            let mut transaction = ws
                .editor
                .workspace_mut()
                .storage_mut()
                .transaction(revision)
                .unwrap();
            transaction
                .create_file(collision.clone(), b"class UserOwned {}".to_vec())
                .unwrap();
            transaction.commit().await.unwrap();

            ws.apply_project_inputs(
                LoweredClasspath::default(),
                FeatureSet::default(),
                ArtifactCache::new(MemoryCache::default()),
                Vec::new(),
                vec![(collision.clone(), "class DependencyOwned {}".to_string())],
            )
            .await;

            assert_eq!(
                ws.editor
                    .workspace()
                    .view()
                    .file(&collision)
                    .unwrap()
                    .bytes(),
                b"class UserOwned {}"
            );
        });
    }
}
