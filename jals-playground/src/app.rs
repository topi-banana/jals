//! The root [`App`] component: it owns all playground state and orchestrates the UI.
//!
//! `App` holds the in-memory [`Workspace`] (behind a `futures::lock::Mutex`), the shared
//! formatter [`Config`], the editable project buffers (`jals.toml` / `jalsfmt.toml` / `build.rhai`),
//! and the current syntax-tree dump, and wires the responsibility-split child components
//! ([`Header`], [`FileTree`], [`EditorPane`], [`SyntaxPane`]) together with props and callbacks.
//! The configuration files are edited as TOML in the editor itself — selecting one opens its
//! buffer, editing `jalsfmt.toml` updates the formatter [`Config`], and editing `jals.toml`
//! re-resolves its `[dependencies]`. Editor *content* operations (switching files, applying a
//! format, repainting diagnostics) are driven imperatively against the single Monaco instance
//! through the [`crate::monaco`] service; the child components stay presentational.
//!
//! # Async shape
//!
//! Yew's `update`/`view` are synchronous, and every analyzing [`Workspace`] call is async — so
//! each handler that touches the workspace spawns a future that locks the shared mutex, does the
//! work, and reports back through a message. The lock is FIFO-fair, so futures spawned in message
//! order also run their workspace sections in that order. For `view()` the [`App`] keeps small
//! sync mirrors (`tree_entries`, `active_path`, `active_source`) refreshed by those messages.
//! Diagnostics are computed under the lock but painted only back in `update`
//! ([`Msg::MarkersComputed`]), where the current model is known — a stale result for a file no
//! longer showing is dropped instead of painted on the wrong model.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::rc::Rc;

use futures::lock::Mutex;
use jals_build::build_script::{BuildScriptEnvironment, BuildScriptLimits, BuildScriptOutput};
use jals_classpath::{LibrarySource, ProjectInputOptions, SourceFile};
use jals_config::fmt::Config;
use jals_config::{FeatureSet, Manifest, ManifestParseError};
use jals_hir::{LoweredClasspath, ProjectIndex};
use jals_project::{
    GraphOutcome, ProjectAnchor, ProjectDiagnostic, ProjectDiagnostics, ProjectScript, ScriptFile,
    ScriptOutcome,
};
use jals_storage::{ArtifactCache, DirKey, FileKey, MemoryCache, MemoryStorage};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::compile::Compile;
use crate::components::{EditorPane, FileTree, Header, PaneTab, ResultPane, TreeEntry};
use crate::download::Download;
use crate::fetcher::BrowserFetcher;
use crate::host::{MonacoRange, PlaygroundDiagnostic};
use crate::workspace::{BUILD_SCRIPT_PATH, MANIFEST_PATH, Workspace};
use crate::{monaco, providers};

/// One of the editable project files shown in the sidebar's `Config` section.
/// They are never analysed or indexed as Java and use plaintext Monaco models.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ConfigKind {
    /// `jals.toml` — the project manifest; its `[dependencies]` drive classpath resolution.
    Manifest,
    /// `jalsfmt.toml` — the formatter configuration.
    Fmt,
    /// `build.rhai` — the portable project build script.
    Script,
}

impl ConfigKind {
    /// Every config kind, in sidebar order — the single source for [`ConfigKind::from_path`] and the
    /// file-tree's `Config` section.
    const ALL: [ConfigKind; 3] = [ConfigKind::Manifest, ConfigKind::Fmt, ConfigKind::Script];

    /// Whether a project diagnostic anchored at `anchor` belongs on this config's model.
    ///
    /// The manifest is where the procedure's dependency half lands, and the script where its own
    /// half does. `jalsfmt.toml` is not part of project assembly, so nothing anchors to it.
    ///
    /// Being the map from anchor to model is also what makes a marker's range come from the right
    /// buffer: `config_marker_entries` places against `config_src(kind)`, and only what this admits
    /// gets there.
    const fn holds(self, anchor: &ProjectAnchor) -> bool {
        matches!(
            (self, anchor),
            (Self::Manifest, ProjectAnchor::Manifest) | (Self::Script, ProjectAnchor::Script(_))
        )
    }

    /// This config's pseudo-path — its Monaco model key and file-tree selection key.
    const fn path(self) -> &'static str {
        match self {
            ConfigKind::Manifest => MANIFEST_PATH,
            ConfigKind::Fmt => "jalsfmt.toml",
            ConfigKind::Script => BUILD_SCRIPT_PATH,
        }
    }

    /// This config's initial editor buffer. Every key is optional (an empty file uses the defaults),
    /// so each seed is a commented template that documents the common knobs while parsing as an empty
    /// (default) config — co-located with the kind, like [`ConfigKind::path`].
    const fn seed(self) -> &'static str {
        match self {
            // The backend is spelled out rather than left to default: the default is `javac`, and
            // the *Build* button would then only ever report that a browser tab has no process to
            // spawn it in. An empty (commented) `[dependencies]` table follows — a CORS-permissive
            // jar resolves directly; Maven Central needs the header's CORS proxy.
            ConfigKind::Manifest => {
                "[package]\n\
                 name = \"playground\"\n\
                 \n\
                 [build]\n\
                 # What compiles the project. `jals` compiles in-process and packages a downloadable\n\
                 # .jar; `jals-wasm` emits one WebAssembly module for the whole project instead.\n\
                 # `javac` needs a host process to spawn, which a browser tab does not have.\n\
                 backend = { type = \"jals\" }\n\
                 script = { type = \"rhai\", file = \"build.rhai\" }\n\
                 \n\
                 [run]\n\
                 # The jar's `Main-Class`. Without it Build still produces a library jar.\n\
                 main-class = \"com.example.Main\"\n\
                 \n\
                 [dependencies]\n\
                 # A CORS-permissive jar resolves directly; Maven Central needs the CORS proxy in the header.\n\
                 # mylib = { jar = \"https://cdn.jsdelivr.net/.../mylib.jar\" }\n"
            }
            ConfigKind::Fmt => {
                "# jalsfmt.toml — every key is optional; an empty file uses the defaults.\n\
                 # max-width = 100\n\
                 # indent-style = \"space\"  # or \"tab\"\n\
                 # indent-width = 4\n\
                 # wrap-comments = false\n\
                 # reorder-imports = false\n"
            }
            ConfigKind::Script => {
                "// Runs entirely in the browser and publishes below target/jals/build/rhai/out.\n\
                 let source = output.write_text(\n\
                     \"com/example/BuildInfo.java\",\n\
                     \"package com.example;\\npublic final class BuildInfo {\\n    public static final String MESSAGE = \\\"Generated in the browser\\\";\\n}\\n\"\n\
                 );\n\
                 build.add_source(source);\n\
                 build.warning(\"generated com.example.BuildInfo\");\n"
            }
        }
    }

    /// Recognise a config pseudo-path (see [`ConfigKind::path`]); `None` for a workspace file path.
    fn from_path(path: &str) -> Option<ConfigKind> {
        ConfigKind::ALL.into_iter().find(|kind| kind.path() == path)
    }

    /// Parse this config from `text`, returning the first error — or `None` when it parses cleanly.
    /// The error's span (when present) drives the marker's range.
    fn parse_error(self, text: &str) -> Option<ConfigParseError> {
        match self {
            ConfigKind::Fmt => ConfigParseError::parse_fmt(text).err(),
            ConfigKind::Manifest => ConfigParseError::parse_manifest(text).err(),
            ConfigKind::Script => None,
        }
    }
}

/// A config-editor parse error to paint as a marker: an optional byte `span` (the marker range; a
/// structural error carrying none falls back to the buffer's first line) plus the `message`.
struct ConfigParseError {
    span: Option<Range<usize>>,
    message: String,
}

#[derive(Clone, PartialEq, Eq)]
struct BuildInputs {
    manifest: String,
    script: String,
    proxy: String,
}

#[derive(Default)]
struct BuildInputTracker {
    last: Option<BuildInputs>,
}

struct DependencySourceTexts {
    library: Vec<(FileKey, String)>,
    source_deps: Vec<(FileKey, String)>,
}

pub struct ClasspathResolution {
    classpath: LoweredClasspath,
    feature_set: FeatureSet,
    /// The root's resolved build features (its `default` list — the browser has no command
    /// line), what `#[cfg(feature = "…")]` evaluates against when `attributes` is on.
    build_features: BTreeSet<String>,
    status: String,
    /// What the graph phase reported, to paint onto the manifest model. The script phase's own
    /// diagnostics arrive separately, with the build that produced them.
    diagnostics: Vec<ProjectDiagnostic>,
    artifacts: ArtifactCache<MemoryCache>,
    sources: DependencySourceTexts,
}

impl BuildInputTracker {
    fn begin(&mut self, inputs: BuildInputs) -> bool {
        if self.last.as_ref() == Some(&inputs) {
            return false;
        }
        self.last = Some(inputs);
        true
    }

    fn invalidate(&mut self) {
        self.last = None;
    }
}

impl ConfigParseError {
    /// Parse `jalsfmt.toml` text into a formatter [`Config`], shaping a TOML syntax/type error as a
    /// [`ConfigParseError`]. The single parse shared by [`App::apply_fmt`] (on edit) and
    /// [`ConfigKind::parse_error`] (on select).
    fn parse_fmt(text: &str) -> Result<Config, Self> {
        toml::from_str::<Config>(text).map_err(|err| Self::from_toml(&err))
    }

    /// Parse + validate `jals.toml` text into a `jals_config::Manifest`, shaping the parse/validation
    /// error as a [`ConfigParseError`]. The single parse shared by [`App::apply_manifest`] (on edit)
    /// and [`ConfigKind::parse_error`] (on select).
    fn parse_manifest(text: &str) -> Result<jals_config::Manifest, Self> {
        text.parse::<jals_config::Manifest>()
            .map_err(|err| Self::from_manifest(&err))
    }

    /// A manifest parse/validation error as a [`ConfigParseError`]: a TOML syntax/type error carries a
    /// span from the underlying [`toml`] error; a structural validation error has none (marked on the
    /// first line).
    fn from_manifest(err: &ManifestParseError) -> Self {
        match err {
            ManifestParseError::Parse { source, .. } => Self::from_toml(source),
            ManifestParseError::Invalid { source, .. } => Self {
                span: None,
                message: source.to_string(),
            },
        }
    }

    /// Shape a [`toml`] deserialize error as a [`ConfigParseError`] for
    /// [`App::set_config_diagnostic`] — the marker range comes from the span when the error carries
    /// one.
    fn from_toml(err: &toml::de::Error) -> Self {
        Self {
            span: err.span(),
            message: err.message().to_string(),
        }
    }
}

/// A snapshot of the current selection generation. Delayed tasks use it to avoid applying results
/// after a newer sidebar or cross-file selection has won.
struct SelectionToken {
    generation: Rc<Cell<u64>>,
    captured: u64,
}

/// A generation captured by an async pipeline — the script/classpath one, or a compile. Newer work
/// of the same kind invalidates all older results, while the workspace lock still serializes
/// aggregate writes.
struct BuildToken {
    generation: Rc<Cell<u64>>,
    captured: u64,
}

impl BuildToken {
    fn is_current(&self) -> bool {
        self.generation.get() == self.captured
    }
}

impl SelectionToken {
    fn is_current(&self) -> bool {
        self.generation.get() == self.captured
    }
}

/// A message driving an [`App`] state transition.
pub enum Msg {
    /// The async workspace construction finished: the shared workspace plus the sync view mirrors
    /// (file-tree entries, active path, active source) captured before it went behind the lock.
    WorkspaceReady {
        workspace: Rc<Mutex<Workspace>>,
        entries: Vec<TreeEntry>,
        path: String,
        source: String,
    },
    /// The editor buffer changed (debounced; edits the active Java file or config buffer).
    EditorChanged(String),
    /// Switch the active file (clicked in the file tree).
    SelectFile(String),
    /// Format the active file in place.
    Format,
    /// A format run finished (either entry point: the *Format* button or Monaco's *Format
    /// Document*), carrying whether `jals-fmt`'s fail-safe refused its own output. Reported rather
    /// than inferred: the refusal hands back the input, so it is byte-identical to "already
    /// formatted" and the buffer cannot show the difference.
    Formatted { fell_back: bool },
    /// Dump the active file's syntax tree into the right pane.
    Syntax,
    /// An async handler re-dumped the active file's syntax tree for the right pane.
    SyntaxDumped(String),
    /// The editor exists: register the language-feature providers and paint the initial markers.
    EditorReady,
    /// A cross-file navigation switched the editor's model to `path`; track it as the active file.
    ModelOpened(String),
    /// An async handler settled on a (possibly new) active Java file: refresh the sync view
    /// mirrors, plus the re-dumped syntax tree when the pane was showing.
    ActiveRefreshed {
        path: String,
        source: String,
        syntax: Option<String>,
    },
    /// Diagnostics computed for the Java file at `path` — painted only if that file is still the
    /// one showing, so a stale result never lands on another file's (or a config's) model.
    MarkersComputed {
        path: String,
        diags: Vec<PlaygroundDiagnostic>,
    },
    /// The CORS proxy changed (typed in the header); stored for the next dependency resolve.
    SetProxy(String),
    /// The async dependency resolution finished: the lowered classpath + the resolved feature set
    /// (from `[package] features`) + a status line, or an error.
    ClasspathResolved {
        generation: u64,
        result: Result<ClasspathResolution, String>,
    },
    /// A successful Rhai execution reloaded generated Java and captured the new sidebar/model set.
    BuildFinished {
        generation: u64,
        entries: Vec<TreeEntry>,
        files: Vec<(String, String)>,
        active_path: String,
        active_source: String,
        status: String,
        diagnostics: Vec<ProjectDiagnostic>,
    },
    /// Rhai compilation/evaluation failed without publishing partial generated output.
    BuildFailed {
        generation: u64,
        /// The status-line summary. The detail is in `diagnostics`, each with its own severity.
        message: String,
        diagnostics: Vec<ProjectDiagnostic>,
    },
    /// Compile the workspace with the backend `jals.toml`'s `[build] backend` selects.
    Compile,
    /// A compile produced a downloadable artifact.
    CompileFinished {
        generation: u64,
        name: String,
        bytes: Vec<u8>,
        summary: String,
    },
    /// A compile produced no artifact, with the reason to show in the Build output tab.
    CompileFailed { generation: u64, message: String },
    /// The user pressed *Download* in the Build output tab.
    Download,
    /// The right pane's tab selection changed.
    SelectTab(PaneTab),
}

/// The playground's root component. Owns every piece of state; the children are presentational.
pub struct App {
    /// The in-memory multi-file workspace; the active file backs the editor. Shared behind an
    /// `Rc<futures::lock::Mutex<…>>` so the once-registered Monaco language-feature providers
    /// (registered in [`Msg::EditorReady`]) and the app's own async handlers serialize on one
    /// FIFO-fair lock. `None` until the async construction delivers [`Msg::WorkspaceReady`].
    workspace: Option<Rc<Mutex<Workspace>>>,
    /// The formatter configuration — parsed from the `jalsfmt.toml` buffer on edit. Shared behind an
    /// `Rc<RefCell<…>>` so the once-registered Monaco *Format Document* provider (created in
    /// [`EditorPane`]) reads the latest settings without a second synced copy (cloned before any
    /// await; never borrowed across one).
    config: Rc<RefCell<Config>>,
    /// Sync mirror of the workspace's indexed Java files, rebuilt after generated-source changes.
    tree_entries: Vec<TreeEntry>,
    /// Sync mirror of the active Java file's path (the pane label / tree highlight).
    active_path: String,
    /// Sync mirror of the active Java file's last-known source — the [`EditorPane`]'s first-mount
    /// model seed (Monaco owns the live text afterwards).
    active_source: String,
    /// The most recent syntax-tree dump shown in the right pane, if any.
    syntax_dump: Option<String>,
    /// The most recent compile's report, shown in the right pane's Build output tab.
    compile_output: Option<String>,
    /// The last successful compile's downloadable artifact, as `(file name, bytes)`. Held here
    /// rather than in the pane's props so a render never clones the bytes and the download stays a
    /// direct response to the user's click.
    compile_artifact: Option<(String, Vec<u8>)>,
    /// Which tab the right pane shows.
    result_tab: PaneTab,
    /// The latest build-script/classpath status line shown in the [`Header`], if any.
    deps_status: Option<String>,
    /// Set while the last format run was one the formatter could not vouch for, cleared by the next
    /// one that is. The [`Header`]'s only channel for it — this host has no diagnostic to publish,
    /// since the fail-safe's subject is the whole file rather than a range in it.
    format_notice: Option<String>,
    /// The `jals.toml` editor buffer. Held here (not in the workspace's Java file tree) so it is
    /// never analysed/indexed; its `[dependencies]` are re-resolved on edit.
    manifest_src: String,
    /// The `jalsfmt.toml` editor buffer. Parsed into the shared formatter [`Config`] on edit.
    fmt_src: String,
    /// The editable `build.rhai` buffer, staged into the workspace aggregate before execution.
    build_src: String,
    /// Which config file is open in the editor, or `None` when a Java workspace file is active.
    active_config: Option<ConfigKind>,
    /// Monotonically increasing identity of the latest sidebar or cross-file selection. Async
    /// selection/format tasks capture it and drop model writes after a newer selection wins.
    selection_generation: Rc<Cell<u64>>,
    /// Monotonically increasing identity of the latest valid manifest/build-script edit.
    build_generation: Rc<Cell<u64>>,
    /// Monotonically increasing identity of the latest compile. Separate from `build_generation`:
    /// a compile neither runs the build script nor re-resolves dependencies, so starting one must
    /// not cancel that pipeline — or be cancelled by it.
    compile_generation: Rc<Cell<u64>>,
    /// Whether Monaco has been created; generated model/marker writes wait for this point.
    editor_ready: bool,
    /// Everything the most recent project assembly reported, ordered and graded once by
    /// [`ProjectDiagnostics`]. Painted onto the config model each one is anchored to.
    project_diagnostics: Vec<ProjectDiagnostic>,
    /// The last parse error for each config editor, retained because a config model's markers are
    /// the *union* of its parse error and the project diagnostics anchored to it — and the two
    /// arrive at different times (a parse error per keystroke, project diagnostics per build).
    config_errors: RefCell<BTreeMap<ConfigKind, ConfigParseError>>,
    /// Inputs sent through the automatic build pipeline, reset by an invalid manifest edit.
    build_inputs: BuildInputTracker,
    /// The CORS proxy for jar downloads (typed in the header); empty by default.
    proxy: String,
}

impl App {
    /// Flatten `workspace`'s files into a pre-order [`TreeEntry`] list for the [`FileTree`].
    fn tree_entries(workspace: &Workspace) -> Vec<TreeEntry> {
        let mut out = Vec::new();
        let mut previous_directories = Vec::new();
        for key in workspace.file_keys() {
            let path = key.to_string();
            let components: Vec<_> = path.split('/').collect();
            let mut directories = Vec::with_capacity(components.len().saturating_sub(1));
            let mut directory = String::new();
            for component in &components[..components.len().saturating_sub(1)] {
                if !directory.is_empty() {
                    directory.push('/');
                }
                directory.push_str(component);
                directories.push(directory.clone());
            }
            let common = previous_directories
                .iter()
                .zip(&directories)
                .take_while(|(left, right)| left == right)
                .count();
            for (depth, directory) in directories.iter().enumerate().skip(common) {
                out.push(TreeEntry {
                    path: directory.clone(),
                    name: components[depth].to_string(),
                    depth,
                    is_dir: true,
                });
            }
            let name = components.last().copied().unwrap_or_default().to_string();
            out.push(TreeEntry {
                path,
                name,
                depth: directories.len(),
                is_dir: false,
            });
            previous_directories = directories;
        }
        out
    }

    /// The state [`Component::create`] starts from, with the workspace still loading.
    ///
    /// Split out of `create` because that function also schedules the async load, which needs a
    /// yew `Context` — this half needs nothing, so state transitions can be exercised on the host.
    fn initial() -> Self {
        App {
            workspace: None,
            config: Rc::new(RefCell::new(Config::default())),
            tree_entries: Vec::new(),
            active_path: String::new(),
            active_source: String::new(),
            syntax_dump: None,
            compile_output: None,
            compile_artifact: None,
            result_tab: PaneTab::Syntax,
            deps_status: None,
            format_notice: None,
            manifest_src: ConfigKind::Manifest.seed().to_string(),
            fmt_src: ConfigKind::Fmt.seed().to_string(),
            build_src: ConfigKind::Script.seed().to_string(),
            active_config: None,
            selection_generation: Rc::new(Cell::new(0)),
            build_generation: Rc::new(Cell::new(0)),
            compile_generation: Rc::new(Cell::new(0)),
            editor_ready: false,
            project_diagnostics: Vec::new(),
            config_errors: RefCell::new(BTreeMap::new()),
            build_inputs: BuildInputTracker::default(),
            proxy: String::new(),
        }
    }

    /// The shared workspace handle, or `None` while the async construction is still running (the
    /// editor pane is not mounted yet, so handlers needing it have nothing to do).
    fn workspace(&self) -> Option<Rc<Mutex<Workspace>>> {
        self.workspace.clone()
    }

    /// Capture the current selection generation for a delayed task.
    fn selection_token(&self) -> SelectionToken {
        SelectionToken {
            generation: Rc::clone(&self.selection_generation),
            captured: self.selection_generation.get(),
        }
    }

    /// Invalidate older selection tokens and capture the new generation.
    fn advance_selection(&self) -> SelectionToken {
        self.selection_generation
            .set(self.selection_generation.get().wrapping_add(1));
        self.selection_token()
    }

    /// Invalidate older build/classpath tasks and capture the new build generation.
    fn advance_build(&self) -> BuildToken {
        self.build_generation
            .set(self.build_generation.get().wrapping_add(1));
        BuildToken {
            generation: Rc::clone(&self.build_generation),
            captured: self.build_generation.get(),
        }
    }

    /// Invalidate older compiles and capture the new compile generation.
    fn advance_compile(&self) -> BuildToken {
        self.compile_generation
            .set(self.compile_generation.get().wrapping_add(1));
        BuildToken {
            generation: Rc::clone(&self.compile_generation),
            captured: self.compile_generation.get(),
        }
    }

    /// Synchronize Monaco's Java models with the editor index after a generated-source reload.
    fn sync_models(files: &[(String, String)]) {
        let values = js_sys::Array::new();
        for (path, text) in files {
            values.push(&js_sys::Array::of2(
                &JsValue::from(path),
                &JsValue::from(text),
            ));
        }
        monaco::sync_models(&values);
    }

    /// Compute the active file's diagnostics as a [`Msg::MarkersComputed`] — sent back to `update`,
    /// which paints only if that file is still showing.
    async fn markers_of(workspace: &Workspace) -> Msg {
        // The editor core owns the project's resolved `[package] features` (set on classpath
        // resolve) and folds them into every diagnostics run, so a default config is enough.
        Msg::MarkersComputed {
            path: workspace.active().to_string(),
            diags: workspace
                .analyze_active(&jals_config::lint::Config::default())
                .await,
        }
    }

    /// Push `diags` (already in Monaco coordinates) to the current model as inline markers.
    fn set_markers(diags: &[PlaygroundDiagnostic]) {
        monaco::Marker::set_diagnostics(diags.iter().map(|d| monaco::Marker {
            start_line: d.range.start_line,
            start_col: d.range.start_col,
            end_line: d.range.end_line,
            end_col: d.range.end_col,
            message: &d.message,
            severity: d.severity,
        }));
    }

    /// The active file's syntax-tree dump for the right pane.
    async fn dump_of(workspace: &Workspace) -> String {
        format!("{:#?}", workspace.syntax_active().await.syntax())
    }

    /// Repaint everything derived from the active file after it changed: the refreshed payload
    /// (with the optional syntax dump) then fresh markers, in that order.
    async fn report_active(
        ws: &Workspace,
        link: &yew::html::Scope<Self>,
        path: String,
        source: String,
        want_syntax: bool,
        selection: &SelectionToken,
    ) {
        let syntax = if want_syntax {
            Some(Self::dump_of(ws).await)
        } else {
            None
        };
        if !selection.is_current() {
            return;
        }
        let markers = Self::markers_of(ws).await;
        if !selection.is_current() {
            return;
        }
        link.send_message(Msg::ActiveRefreshed {
            path,
            source,
            syntax,
        });
        link.send_message(markers);
    }

    /// Reflect `text` into the active Java file's analysis overlay (serialized behind the lock),
    /// without repainting markers — the flush before a switch *away* from the file, where fresh
    /// markers would land on the wrong model.
    fn flush_active_java(&self, text: String) {
        let Some(workspace) = self.workspace() else {
            return;
        };
        spawn_local(async move {
            workspace.lock().await.sync_active(&text).await;
        });
    }

    /// Commit the live text of the *config* buffer `kind` (the Fmt arm also reparses into the
    /// shared formatter [`Config`], repainting its markers). Shared by the flush-before-switch and
    /// [`Msg::EditorChanged`]'s Fmt arm; the manifest arm only stores the buffer — kicking off its
    /// dependency resolve is [`Msg::EditorChanged`]'s job (a flush must not resolve).
    fn commit_config_buffer(&mut self, kind: ConfigKind, value: String) {
        match kind {
            ConfigKind::Manifest => self.manifest_src = value,
            ConfigKind::Fmt => {
                // Commit the latest formatter config, in case the last edit debounce has not fired
                // yet (cheap: no network, unlike the manifest resolve).
                self.apply_fmt(&value);
                self.fmt_src = value;
            }
            ConfigKind::Script => self.build_src = value,
        }
    }

    /// The current buffer text of config file `kind`.
    fn config_src(&self, kind: ConfigKind) -> &str {
        match kind {
            ConfigKind::Manifest => &self.manifest_src,
            ConfigKind::Fmt => &self.fmt_src,
            ConfigKind::Script => &self.build_src,
        }
    }

    /// The `(path, source)` of the active document: the open config file's pseudo-path + buffer, else
    /// the active Java file's path + last-known source (the sync mirrors). Computed together for
    /// [`Component::view`].
    fn active_pane(&self) -> (String, String) {
        match self.active_config {
            Some(kind) => (kind.path().to_string(), self.config_src(kind).to_string()),
            None => (self.active_path.clone(), self.active_source.clone()),
        }
    }

    /// Record `error` as `kind`'s current parse state and repaint that model.
    ///
    /// The error is *retained* rather than painted and forgotten: a config model's markers are the
    /// union of its parse error and the project diagnostics anchored to it, and the two arrive at
    /// different times — a parse error on every keystroke, project diagnostics once a build
    /// finishes. Whichever changed, the union needs both.
    fn set_config_diagnostic(&self, kind: ConfigKind, error: Option<ConfigParseError>) {
        match error {
            Some(error) => self.config_errors.borrow_mut().insert(kind, error),
            None => self.config_errors.borrow_mut().remove(&kind),
        };
        self.repaint_config_model(kind);
    }

    /// Repaint every config model. Called when the project diagnostics change, since one assembly
    /// can move markers on the manifest and the script at once.
    fn repaint_config_markers(&self) {
        for kind in ConfigKind::ALL {
            self.repaint_config_model(kind);
        }
    }

    /// Replace `kind`'s markers with the union of its parse error and the project diagnostics
    /// anchored to it.
    ///
    /// Addressed by path rather than painted on the *current* model: two owners writing the current
    /// model is how a script diagnostic and a config parse error used to erase each other whenever
    /// `build.rhai` happened to be the open editor.
    fn repaint_config_model(&self, kind: ConfigKind) {
        let text = self.config_src(kind);
        let errors = self.config_errors.borrow();
        let entries = self.config_marker_entries(kind, errors.get(&kind));
        // Published even when empty: an empty marker set is what clears a previous run's.
        let index = jals_editor::LineIndex::new(text);
        let markers: Vec<_> = entries
            .iter()
            .map(|(range, severity, message)| {
                let MonacoRange {
                    start_line,
                    start_col,
                    end_line,
                    end_col,
                } = MonacoRange::of(&index, text, range);
                monaco::Marker {
                    start_line,
                    start_col,
                    end_line,
                    end_col,
                    message,
                    severity: *severity,
                }
            })
            .collect();
        monaco::Marker::set_diagnostics_for(kind.path(), markers);
    }

    /// What `kind`'s model should be marked with: its parse error, if any, then every project
    /// diagnostic anchored to it, each already placed and graded.
    ///
    /// Split out of [`repaint_config_model`](Self::repaint_config_model) because that function ends
    /// in a Monaco binding and cannot run on the host, while *this* — which range each entry gets
    /// and which severity — is the part worth pinning. The same split `compile.rs` makes for the
    /// build pipeline.
    fn config_marker_entries<'a>(
        &'a self,
        kind: ConfigKind,
        parse_error: Option<&'a ConfigParseError>,
    ) -> Vec<(Range<usize>, jals_editor::DiagnosticSeverity, &'a str)> {
        /// The first line — where *this editor's own* config parse error goes when the TOML parser
        /// reported no span.
        ///
        /// A `ConfigParseError` is not a `ProjectDiagnostic`, so it cannot go through
        /// `ProjectDiagnostic::placement_in`; this mirrors that rule rather than sharing it, `\r`
        /// trim included, because two answers to "where does a span-less thing go" a few lines
        /// apart in one function is exactly what moving the other one out was for.
        fn first_line(text: &str) -> Range<usize> {
            let end = text.find('\n').unwrap_or(text.len());
            0..text[..end].trim_end_matches('\r').len()
        }

        let text = self.config_src(kind);
        // Gathered rather than mapped straight to markers because a `Marker` borrows its message and
        // these come from two places.
        let mut entries = Vec::new();
        if let Some(ConfigParseError { span, message }) = parse_error {
            entries.push((
                span.clone().unwrap_or_else(|| first_line(text)),
                jals_editor::DiagnosticSeverity::Error,
                message.as_str(),
            ));
        }
        for diagnostic in &self.project_diagnostics {
            if !kind.holds(&diagnostic.anchor) {
                continue;
            }
            entries.push((
                // A marker has to name a range; `holds` above is what makes `text` this
                // diagnostic's own anchor.
                diagnostic.placement_in(text),
                // A marker's severity is typed on the editor's three-arm vocabulary, which is the
                // one the assembly converts into. `Hint` renders faintly, which is why the offline
                // advisory also stays in the status line.
                jals_editor::DiagnosticSeverity::from(diagnostic.severity),
                diagnostic.message.as_str(),
            ));
        }
        entries
    }

    /// Parse `jalsfmt.toml` text into the shared formatter [`Config`] and repaint the config editor's
    /// diagnostics. On success the new config takes effect immediately (the Format button and
    /// Monaco's *Format Document* both read the shared `config`); on failure the config is left as-is.
    fn apply_fmt(&self, text: &str) {
        let error = match ConfigParseError::parse_fmt(text) {
            Ok(config) => {
                *self.config.borrow_mut() = config;
                None
            }
            Err(err) => Some(err),
        };
        self.set_config_diagnostic(ConfigKind::Fmt, error);
    }

    /// Parse + validate `jals.toml` and start the Rhai/classpath pipeline. Invalid edits cancel
    /// older result delivery and paint the manifest parse/validation marker.
    fn apply_manifest(&mut self, ctx: &Context<Self>, text: &str) -> bool {
        let manifest = match ConfigParseError::parse_manifest(text) {
            Ok(manifest) => manifest,
            Err(err) => {
                self.build_inputs.invalidate();
                self.advance_build();
                self.deps_status = Some(format!("manifest error: {}", err.message));
                self.set_config_diagnostic(ConfigKind::Manifest, Some(err));
                return true;
            }
        };
        self.set_config_diagnostic(ConfigKind::Manifest, None);
        self.start_build(ctx, manifest)
    }

    /// Run the edited Rhai buffer when the current manifest configures a build script.
    fn apply_script(&mut self, ctx: &Context<Self>) -> bool {
        let Ok(manifest) = ConfigParseError::parse_manifest(&self.manifest_src) else {
            return false;
        };
        if manifest.build.script.is_none() {
            return false;
        }
        self.start_build(ctx, manifest)
    }

    /// Apply a manifest/script buffer committed while switching away from its Monaco model.
    fn apply_committed_build_input(&mut self, ctx: &Context<Self>, kind: ConfigKind) {
        match kind {
            ConfigKind::Manifest => {
                let text = self.manifest_src.clone();
                self.apply_manifest(ctx, &text);
            }
            ConfigKind::Script => {
                self.apply_script(ctx);
            }
            ConfigKind::Fmt => {}
        }
    }

    /// Serialize one build against the owning workspace aggregate, publish generated Java/model
    /// state immediately, then resolve manifest and build-output classpath inputs off a snapshot.
    fn start_build(&mut self, ctx: &Context<Self>, manifest: Manifest) -> bool {
        let Some(workspace) = self.workspace() else {
            return false;
        };
        let inputs = BuildInputs {
            manifest: self.manifest_src.clone(),
            script: self.build_src.clone(),
            proxy: self.proxy.clone(),
        };
        if !self.build_inputs.begin(inputs) {
            return false;
        }
        let token = self.advance_build();
        self.deps_status = Some("running bounded Rhai build script...".to_string());
        self.project_diagnostics.clear();
        if self.editor_ready {
            self.repaint_config_markers();
        }
        let manifest_text = self.manifest_src.clone();
        let script_text = self.build_src.clone();
        let proxy = self.proxy.clone();
        let link = ctx.link().clone();
        spawn_local(async move {
            let build_result = {
                let mut ws = workspace.lock().await;
                if !token.is_current() {
                    return;
                }
                match ws
                    .run_build_script_with_proxy(&manifest, &manifest_text, &script_text, &proxy)
                    .await
                {
                    Ok(script) => {
                        let status = Self::build_status(script.output());
                        let diagnostics = Self::script_diagnostics(
                            script
                                .output()
                                .map_or(ScriptOutcome::Skipped, ScriptOutcome::Ran),
                            &manifest,
                            &script_text,
                        );
                        let entries = Self::tree_entries(&ws);
                        let files = ws.file_texts();
                        let active_path = ws.active().to_string();
                        let active_source = ws.active_source();
                        let markers = Self::markers_of(&ws).await;
                        let storage = ws.storage_snapshot();
                        Ok((
                            status,
                            diagnostics,
                            script,
                            entries,
                            files,
                            active_path,
                            active_source,
                            markers,
                            storage,
                        ))
                    }
                    Err(error) => Err((
                        error.to_string(),
                        Self::script_diagnostics(
                            ScriptOutcome::Failed(&error),
                            &manifest,
                            &script_text,
                        ),
                    )),
                }
            };

            if !token.is_current() {
                return;
            }
            let (
                build_status,
                diagnostics,
                script,
                entries,
                files,
                active_path,
                active_source,
                markers,
                storage,
            ) = match build_result {
                Ok(result) => result,
                Err((message, diagnostics)) => {
                    link.send_message(Msg::BuildFailed {
                        generation: token.captured,
                        message,
                        diagnostics,
                    });
                    return;
                }
            };
            link.send_message(Msg::BuildFinished {
                generation: token.captured,
                entries,
                files,
                active_path,
                active_source,
                status: build_status.clone(),
                diagnostics,
            });
            link.send_message(markers);

            let result = Self::resolve_classpath(manifest, proxy, storage, script)
                .await
                .map(|mut resolution| {
                    resolution.status = format!("{build_status}; {}", resolution.status);
                    resolution
                });
            if token.is_current() {
                link.send_message(Msg::ClasspathResolved {
                    generation: token.captured,
                    result,
                });
            }
        });
        true
    }

    /// The script phase's diagnostics, assembled from whichever outcome it had.
    ///
    /// The script's key comes from the manifest and its text from the live editor buffer, so a
    /// failure carrying a Rhai position resolves to a byte span the marker path can point at.
    fn script_diagnostics(
        outcome: ScriptOutcome<'_>,
        manifest: &Manifest,
        script_text: &str,
    ) -> Vec<ProjectDiagnostic> {
        let key = manifest
            .build
            .script
            .as_ref()
            .and_then(|script| match script {
                jals_config::BuildScript::Rhai { file } => FileKey::parse(file).ok(),
            });
        ProjectDiagnostics::assemble(
            outcome,
            GraphOutcome::NotReached,
            key.as_ref().map(|key| ScriptFile {
                key,
                text: Some(script_text),
            }),
        )
    }

    /// Human-readable result of a successful script phase.
    fn build_status(output: Option<&BuildScriptOutput>) -> String {
        let Some(output) = output else {
            return "build script disabled".to_string();
        };
        let mut status = format!("generated {} file(s)", output.generated_files.len());
        if !output.diagnostics.is_empty() {
            // Each renders its own severity, so the count does not have to name one.
            status.push_str(&format!(
                "; {} diagnostic(s): {}",
                output.diagnostics.len(),
                output
                    .diagnostics
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        status
    }

    /// Assemble a parsed `manifest`'s analysis inputs into a lowered classpath, in the browser:
    /// download each remote `[dependencies]` jar with a [`BrowserFetcher`] into the in-memory `cache`,
    /// load the `.class` files, and lower them for the project index. Returns the classpath, the
    /// resolved feature set from `[package] features` (for the feature-gated lint rules), and a
    /// human-readable status line (class/jar counts plus any warnings), or an error message.
    ///
    /// The whole resolution runs against a detached storage snapshot (on the same execution
    /// context, cloned with it) so the workspace lock is never held across an `.await` here. A
    /// successful result carries its verified cache and detached dependency source texts back as one
    /// generation-guarded application.
    async fn resolve_classpath(
        mut manifest: Manifest,
        proxy: String,
        mut storage: MemoryStorage,
        script: ProjectScript,
    ) -> Result<ClasspathResolution, String> {
        let fetcher = BrowserFetcher::new(proxy, jals_classpath::NetworkPolicy::Online);
        // The script's `add_classpath` entries are lowered by exactly the rule that lowers the
        // manifest's own, and land after them — the group order a browser build has always had.
        script.augment_classpath(&mut manifest);
        // No command line here either, so what the root forwards to its dependencies comes from its
        // own `default` list — the same selection the root script above ran under. With nothing
        // selected, resolution cannot fail.
        let features = manifest
            .resolve_build_features(&[], false, false)
            .unwrap_or_default();
        let exec = storage.exec().clone();
        let assembly = script
            .resolve_memory(
                &manifest,
                &mut storage,
                jals_project::GraphPreprocess {
                    exec: &exec,
                    // A dependency's build-task fetches go through the same CORS proxy as
                    // dependency resolution; nothing else in the browser can reach a host.
                    fetcher: &fetcher,
                    environment: &BuildScriptEnvironment::new(),
                    root_features: &features,
                    limits: &BuildScriptLimits::default(),
                },
                ProjectInputOptions::Editor,
            )
            .await
            // A failed phase's warnings ride along in the message: this returns `Err(String)` for
            // the status line, and the assembly is what puts them in front of the error they
            // explain.
            .map_err(|failure| {
                Self::status_line(&ProjectDiagnostics::assemble(
                    ScriptOutcome::Skipped,
                    GraphOutcome::Failed(&failure),
                    None,
                ))
            })?;
        let diagnostics = ProjectDiagnostics::assemble(
            ScriptOutcome::Skipped,
            GraphOutcome::Resolved(assembly.report()),
            None,
        );
        // The browser was asked to produce a classpath, so an error stops it. What "error" means is
        // the assembly's, not a severity test spelled here.
        if ProjectDiagnostics::has_errors(&diagnostics) {
            return Err(Self::status_line(&diagnostics));
        }
        let inputs = assembly.inputs;
        let classpath = ProjectIndex::lower_classpath(&inputs.classpath_classes).await;
        let sources = Self::dependency_source_texts(&storage, &inputs).await?;
        let mut status = format!(
            "resolved {} class(es) from {} jar(s)",
            inputs.classpath_classes.len(),
            inputs.dependency_jars.len()
        );
        // The markers carry the detail; the status line carries the summary, because a `Hint`
        // marker (the offline advisory) renders faintly enough to miss.
        if !diagnostics.is_empty() {
            status.push_str(" — ");
            status.push_str(&Self::status_line(&diagnostics));
        }
        Ok(ClasspathResolution {
            classpath,
            feature_set: inputs.feature_set,
            build_features: features.features().clone(),
            status,
            diagnostics,
            artifacts: storage.into_artifacts(),
            sources,
        })
    }

    /// One line naming each diagnostic with its severity.
    ///
    /// The browser's only always-visible channel. Every producer renders whole, so a warning that
    /// names its dependency only in the attribution still says which one it is about.
    fn status_line(diagnostics: &[ProjectDiagnostic]) -> String {
        diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.severity.lead(), diagnostic.message))
            .collect::<Vec<_>>()
            .join("; ")
    }

    async fn dependency_source_texts(
        storage: &MemoryStorage,
        inputs: &jals_classpath::ProjectInputs,
    ) -> Result<DependencySourceTexts, String> {
        async fn read_artifact(
            storage: &MemoryStorage,
            root: &DirKey,
            source: &LibrarySource,
        ) -> Result<(FileKey, String), String> {
            let key = root.file_at(&source.path).map_err(|error| {
                format!(
                    "dependency source `{}` has no valid navigation key: {error:?}",
                    source.path
                )
            })?;
            let bytes = storage
                .artifacts()
                .lookup(&source.key)
                .await
                .map_err(|error| {
                    format!("dependency source `{}` is invalid: {error:?}", source.path)
                })?
                .ok_or_else(|| format!("dependency source `{}` is missing", source.path))?;
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("dependency source `{}` is not valid UTF-8", source.path))?;
            Ok((key, text))
        }

        let library_root =
            DirKey::parse(".jals/library").expect("constant is a portable directory key");
        let source_dep_root =
            DirKey::parse(".jals/source-dependency").expect("constant is a portable directory key");
        let view = storage.view();
        let mut library = BTreeMap::new();
        for source in &inputs.library_sources {
            let (key, text) = read_artifact(storage, &library_root, source).await?;
            library.insert(key, text);
        }
        let mut source_deps = BTreeMap::new();
        for source in &inputs.source_dep_sources {
            match source {
                SourceFile::Project(key) => {
                    let text = view
                        .file_text(key)
                        .map_err(|error| {
                            format!("dependency source `{key}` cannot be read: {error}")
                        })?
                        .to_string();
                    source_deps.insert(key.clone(), text);
                }
                SourceFile::Artifact(source) => {
                    let (key, text) = read_artifact(storage, &source_dep_root, source).await?;
                    source_deps.insert(key, text);
                }
            }
        }
        Ok(DependencySourceTexts {
            library: library.into_iter().collect(),
            source_deps: source_deps.into_iter().collect(),
        })
    }
}

impl Component for App {
    type Message = Msg;
    type Properties = ();

    fn create(ctx: &Context<Self>) -> Self {
        // The workspace loads asynchronously (parsing the seed runs on the browser executor); the
        // editor pane mounts once `WorkspaceReady` delivers it together with the view mirrors.
        ctx.link().send_future(async {
            let workspace = Workspace::new().await;
            let entries = Self::tree_entries(&workspace);
            let path = workspace.active().to_string();
            let source = workspace.active_source();
            Msg::WorkspaceReady {
                workspace: Rc::new(Mutex::new(workspace)),
                entries,
                path,
                source,
            }
        });
        Self::initial()
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::WorkspaceReady {
                workspace,
                entries,
                path,
                source,
            } => {
                self.workspace = Some(workspace);
                self.tree_entries = entries;
                self.active_path = path;
                self.active_source = source;
                let Ok(manifest) = ConfigParseError::parse_manifest(&self.manifest_src) else {
                    unreachable!("seed manifest is valid");
                };
                self.start_build(ctx, manifest);
                true
            }
            // Route the edit by what is open: a config buffer parses into its effect (formatter
            // config / dependency resolve) and repaints config markers; a Java file syncs the
            // workspace overlay and recomputes its markers. Monaco owns the live text.
            Msg::EditorChanged(value) => match self.active_config {
                // A manifest edit resolves its `[dependencies]` and stores the buffer; re-render only
                // when a resolve actually started (the header shows "resolving…").
                Some(ConfigKind::Manifest) => {
                    self.manifest_src = value;
                    let text = self.manifest_src.clone();
                    self.apply_manifest(ctx, &text)
                }
                // The formatter config reparses into the shared `Config` (repainting its markers).
                Some(ConfigKind::Fmt) => {
                    self.commit_config_buffer(ConfigKind::Fmt, value);
                    false
                }
                Some(ConfigKind::Script) => {
                    self.build_src = value;
                    self.apply_script(ctx)
                }
                // A Java file: sync the overlay and recompute markers behind the lock; the paint
                // comes back as `MarkersComputed` so it lands only if this file is still showing.
                None => {
                    self.active_source = value.clone();
                    if let Some(workspace) = self.workspace() {
                        let link = ctx.link().clone();
                        spawn_local(async move {
                            let mut ws = workspace.lock().await;
                            ws.sync_active(&value).await;
                            link.send_message(Self::markers_of(&ws).await);
                        });
                    }
                    false
                }
            },
            Msg::SelectFile(path) => {
                let selection = self.advance_selection();
                // Flush the live editor text into the (still-active) file/buffer before switching.
                let live = monaco::current_value();
                if let Some(kind) = ConfigKind::from_path(&path) {
                    match self.active_config {
                        Some(outgoing) => {
                            self.commit_config_buffer(outgoing, live);
                            self.apply_committed_build_input(ctx, outgoing);
                        }
                        // Overlay-only: fresh Java markers would land on the config model.
                        None => self.flush_active_java(live),
                    }
                    self.active_config = Some(kind);
                    let src = self.config_src(kind).to_string();
                    monaco::switch_model(&path, &src);
                    // Selecting never executes a script or starts dependency resolution.
                    if kind == ConfigKind::Script {
                        self.repaint_config_markers();
                    } else {
                        self.set_config_diagnostic(kind, kind.parse_error(&src));
                    }
                } else {
                    let Some(workspace) = self.workspace() else {
                        return false;
                    };
                    // The outgoing Java flush and the switch share one lock hold, so the flush
                    // lands on the outgoing file before `set_active` moves the anchor.
                    let outgoing_java = match self.active_config {
                        Some(outgoing) => {
                            self.commit_config_buffer(outgoing, live);
                            self.apply_committed_build_input(ctx, outgoing);
                            None
                        }
                        None => Some(live),
                    };
                    self.active_config = None;
                    let want_syntax = self.syntax_dump.is_some();
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        let mut ws = workspace.lock().await;
                        if !selection.is_current() {
                            return;
                        }
                        if let Some(text) = outgoing_java {
                            ws.sync_active(&text).await;
                        }
                        if !selection.is_current() {
                            return;
                        }
                        if !ws.set_active(&path) {
                            let path = ws.active().to_string();
                            let source = ws.active_source();
                            monaco::switch_model(&path, &source);
                            Self::report_active(&ws, &link, path, source, want_syntax, &selection)
                                .await;
                            return;
                        }
                        let path = ws.active().to_string();
                        let source = ws.active_source();
                        monaco::switch_model(&path, &source);
                        Self::report_active(&ws, &link, path, source, want_syntax, &selection)
                            .await;
                    });
                }
                true
            }
            // Format and Syntax are Java-only; a config file is plain TOML, so ignore them there.
            Msg::Format => {
                if self.active_config.is_some() {
                    return false;
                }
                let Some(workspace) = self.workspace() else {
                    return false;
                };
                let live = monaco::current_value();
                let config = self.config.borrow().clone();
                let want_syntax = self.syntax_dump.is_some();
                let link = ctx.link().clone();
                let selection = self.selection_token();
                spawn_local(async move {
                    let mut ws = workspace.lock().await;
                    // Flush the live buffer, format it, and rewrite the editor in place.
                    ws.sync_active(&live).await;
                    let out = ws.format_active(&config).await;
                    let fell_back = out.fell_back();
                    let formatted = out.formatted;
                    if !selection.is_current() {
                        return;
                    }
                    link.send_message(Msg::Formatted { fell_back });
                    monaco::update_model(&formatted);
                    ws.sync_active(&formatted).await;
                    let path = ws.active().to_string();
                    Self::report_active(&ws, &link, path, formatted, want_syntax, &selection).await;
                });
                false
            }
            Msg::Formatted { fell_back } => {
                let notice = fell_back.then(|| {
                    "format: jals-fmt could not vouch for its output; the file was left unchanged"
                        .to_string()
                });
                // Repainting on every successful format would re-render the header for nothing, so
                // only a change of verdict is worth a render.
                if self.format_notice == notice {
                    return false;
                }
                self.format_notice = notice;
                true
            }
            Msg::Syntax => {
                if self.active_config.is_some() {
                    return false;
                }
                let Some(workspace) = self.workspace() else {
                    return false;
                };
                let live = monaco::current_value();
                let link = ctx.link().clone();
                // Asking for a dump while the Build output is showing must bring the dump forward,
                // or the button looks broken.
                self.result_tab = PaneTab::Syntax;
                spawn_local(async move {
                    let mut ws = workspace.lock().await;
                    // Flush the live buffer first, so the dump matches what the editor shows.
                    ws.sync_active(&live).await;
                    link.send_message(Msg::SyntaxDumped(App::dump_of(&ws).await));
                });
                true
            }
            Msg::SyntaxDumped(dump) => {
                self.syntax_dump = Some(dump);
                true
            }
            Msg::EditorReady => {
                let Some(workspace) = self.workspace() else {
                    return false;
                };
                self.editor_ready = true;
                self.repaint_config_markers();
                // Register the language-feature providers, backed by the shared workspace.
                providers::Providers::install(Rc::clone(&workspace));
                let link = ctx.link().clone();
                spawn_local(async move {
                    let ws = workspace.lock().await;
                    // Eagerly create URI-backed models for cross-file navigation and discard any
                    // stale generated models left by an earlier script execution.
                    App::sync_models(&ws.file_texts());
                    link.send_message(App::markers_of(&ws).await);
                });
                false
            }
            Msg::ModelOpened(path) => {
                let selection = self.advance_selection();
                // Monaco already switched the model (and flushed the outgoing file via `on_change`,
                // whose message — and therefore its lock turn — precedes this one); only track the
                // new active file and repaint. Must not flush or `switch_model` again. Cross-file
                // navigation only ever targets Java files, so a config is no longer open.
                self.active_config = None;
                let Some(workspace) = self.workspace() else {
                    return true;
                };
                let want_syntax = self.syntax_dump.is_some();
                let link = ctx.link().clone();
                spawn_local(async move {
                    let mut ws = workspace.lock().await;
                    if !selection.is_current() {
                        return;
                    }
                    if !ws.set_active(&path) {
                        let path = ws.active().to_string();
                        let source = ws.active_source();
                        monaco::switch_model(&path, &source);
                        Self::report_active(&ws, &link, path, source, want_syntax, &selection)
                            .await;
                        return;
                    }
                    let path = ws.active().to_string();
                    let source = ws.active_source();
                    Self::report_active(&ws, &link, path, source, want_syntax, &selection).await;
                });
                true
            }
            Msg::ActiveRefreshed {
                path,
                source,
                syntax,
            } => {
                self.active_path = path;
                self.active_source = source;
                if syntax.is_some() {
                    self.syntax_dump = syntax;
                }
                true
            }
            Msg::MarkersComputed { path, diags } => {
                // Paint only when the diagnosed file is still the one showing; a result computed
                // before a switch (to another file or a config buffer) is stale — drop it.
                if self.active_config.is_none() && self.active_path == path {
                    Self::set_markers(&diags);
                }
                false
            }
            Msg::SetProxy(proxy) => {
                // The input is uncontrolled — just record the value for the next resolve; no re-render.
                self.proxy = proxy;
                false
            }
            Msg::BuildFinished {
                generation,
                entries,
                files,
                active_path,
                active_source,
                status,
                diagnostics,
            } => {
                if generation != self.build_generation.get() {
                    return false;
                }
                let active_changed = self.active_path != active_path;
                self.tree_entries = entries;
                self.active_path = active_path.clone();
                self.active_source = active_source.clone();
                self.deps_status = Some(status);
                self.project_diagnostics = diagnostics;
                if self.editor_ready {
                    if self.active_config.is_none() {
                        if active_changed {
                            monaco::switch_model(&active_path, &active_source);
                        } else {
                            monaco::update_model(&active_source);
                        }
                    }
                    Self::sync_models(&files);
                    self.repaint_config_markers();
                }
                true
            }
            Msg::BuildFailed {
                generation,
                message,
                diagnostics,
            } => {
                if generation != self.build_generation.get() {
                    return false;
                }
                self.deps_status = Some(format!("build error: {message}"));
                self.project_diagnostics = diagnostics;
                if self.editor_ready {
                    self.repaint_config_markers();
                }
                true
            }
            Msg::ClasspathResolved { generation, result } => {
                if generation != self.build_generation.get() {
                    return false;
                }
                match result {
                    Ok(resolution) => {
                        self.deps_status = Some(resolution.status);
                        // The graph phase's diagnostics replace the previous graph phase's while
                        // leaving the script phase's in place: both are anchored, and each anchor
                        // has one producer.
                        //
                        // The retain is *unconditional* — driven by the anchor this phase owns, not
                        // by what arrived. That is what clears the previous run's manifest markers
                        // when this run had nothing to say; deriving the set from `resolution`
                        // instead would leave a resolved project wearing a failed one's warnings.
                        self.project_diagnostics
                            .retain(|diagnostic| diagnostic.anchor != ProjectAnchor::Manifest);
                        self.project_diagnostics.extend(resolution.diagnostics);
                        if self.editor_ready {
                            self.repaint_config_markers();
                        }
                        if let Some(workspace) = self.workspace() {
                            let link = ctx.link().clone();
                            let build_generation = Rc::clone(&self.build_generation);
                            spawn_local(async move {
                                if build_generation.get() != generation {
                                    return;
                                }
                                // All settle in the editor core: the classpath rebuilds the index,
                                // the feature set folds into every later diagnostics run, and the
                                // detached task's verified artifacts merge back.
                                let mut ws = workspace.lock().await;
                                if build_generation.get() != generation {
                                    return;
                                }
                                ws.apply_project_inputs(
                                    resolution.classpath,
                                    resolution.feature_set,
                                    resolution.build_features,
                                    resolution.artifacts,
                                    resolution.sources.library,
                                    resolution.sources.source_deps,
                                )
                                .await;
                                // Re-analyse with the external types now in the index;
                                // `MarkersComputed` drops the paint if a config model is showing.
                                let markers = App::markers_of(&ws).await;
                                if build_generation.get() == generation {
                                    link.send_message(markers);
                                }
                            });
                        }
                    }
                    Err(err) => self.deps_status = Some(format!("error: {err}")),
                }
                true
            }
            Msg::Compile => {
                let Some(workspace) = self.workspace() else {
                    return false;
                };
                // Unlike Format and Syntax, this must *not* early-return while a config buffer is
                // open: editing `jals.toml` and pressing Build is how the backend gets switched.
                // What it skips instead is the Java flush. `current_value()` is empty before the
                // editor mounts, so it is only trusted once Monaco owns real text.
                let live = self.editor_ready.then(monaco::current_value);
                let outgoing_java = match (self.active_config, live) {
                    // Committing stores the buffer without starting the dependency pipeline — a
                    // compile is not an edit. The debounce may not have fired, so this is what
                    // makes a just-typed `backend = …` count.
                    (Some(kind), Some(value)) => {
                        self.commit_config_buffer(kind, value);
                        None
                    }
                    (None, live) => live,
                    (Some(_), None) => None,
                };
                self.result_tab = PaneTab::Output;
                // Dropped before the compile rather than after it fails, so a stale jar is never
                // downloadable while a newer compile is in flight.
                self.compile_artifact = None;
                let manifest = match ConfigParseError::parse_manifest(&self.manifest_src) {
                    Ok(manifest) => manifest,
                    Err(error) => {
                        self.compile_output = Some(format!("{MANIFEST_PATH}: {}", error.message));
                        return true;
                    }
                };
                let token = self.advance_compile();
                self.compile_output = Some("compiling…".to_owned());
                let link = ctx.link().clone();
                spawn_local(async move {
                    let files = {
                        let mut ws = workspace.lock().await;
                        if !token.is_current() {
                            return;
                        }
                        if let Some(text) = outgoing_java {
                            ws.sync_active(&text).await;
                        }
                        ws.file_texts()
                    };
                    // The lock is released before compiling: it is the longest-running thing the
                    // playground does, and the language-feature providers queue behind that lock.
                    if !token.is_current() {
                        return;
                    }
                    let message = match Compile::workspace(&manifest, &files).await {
                        Ok(artifact) => Msg::CompileFinished {
                            generation: token.captured,
                            name: artifact.name,
                            bytes: artifact.bytes,
                            summary: artifact.summary,
                        },
                        Err(error) => Msg::CompileFailed {
                            generation: token.captured,
                            message: error.to_string(),
                        },
                    };
                    if token.is_current() {
                        link.send_message(message);
                    }
                });
                true
            }
            Msg::CompileFinished {
                generation,
                name,
                bytes,
                summary,
            } => {
                if generation != self.compile_generation.get() {
                    return false;
                }
                self.compile_output = Some(summary);
                self.compile_artifact = Some((name, bytes));
                self.result_tab = PaneTab::Output;
                true
            }
            Msg::CompileFailed {
                generation,
                message,
            } => {
                if generation != self.compile_generation.get() {
                    return false;
                }
                self.compile_output = Some(message);
                self.compile_artifact = None;
                self.result_tab = PaneTab::Output;
                true
            }
            Msg::Download => {
                if let Some((name, bytes)) = &self.compile_artifact {
                    Download::save(name, bytes);
                }
                false
            }
            Msg::SelectTab(tab) => {
                self.result_tab = tab;
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        // The editor seeds its model from `source` only on first mount (always a Java file then);
        // it is therefore mounted only once the workspace exists and the mirrors are real.
        let (active_path, source) = self.active_pane();
        let config_entries = ConfigKind::ALL
            .into_iter()
            .map(|kind| TreeEntry {
                path: kind.path().to_string(),
                name: kind.path().to_string(),
                depth: 0,
                is_dir: false,
            })
            .collect::<Vec<_>>();
        let editor = if self.workspace.is_some() {
            html! {
                <EditorPane
                    path={active_path.clone()}
                    source={source}
                    on_change={link.callback(Msg::EditorChanged)}
                    on_ready={link.callback(|_| Msg::EditorReady)}
                    on_open={link.callback(Msg::ModelOpened)}
                    on_formatted={link.callback(|fell_back| Msg::Formatted { fell_back })}
                    config={self.config.clone()}
                />
            }
        } else {
            html! {
                <section class="flex min-h-0 items-center justify-center font-mono text-xs text-mute">
                    { "loading workspace…" }
                </section>
            }
        };
        html! {
            <div class="flex h-screen flex-col bg-canvas-soft text-ink">
                <Header
                    on_format={link.callback(|_| Msg::Format)}
                    on_syntax={link.callback(|_| Msg::Syntax)}
                    on_compile={link.callback(|_| Msg::Compile)}
                    on_proxy_change={link.callback(Msg::SetProxy)}
                    deps_status={self.deps_status.clone()}
                    format_notice={self.format_notice.clone()}
                />
                <div class="flex min-h-0 flex-1">
                    <FileTree
                        config_entries={config_entries}
                        entries={self.tree_entries.clone()}
                        active={active_path}
                        on_select={link.callback(Msg::SelectFile)}
                    />
                    <main class="grid min-h-0 flex-1 grid-cols-1 md:grid-cols-2">
                        { editor }
                        <ResultPane
                            tab={self.result_tab}
                            on_tab={link.callback(Msg::SelectTab)}
                            dump={self.syntax_dump.clone()}
                            output={self.compile_output.clone()}
                            artifact={self.compile_artifact.as_ref().map(|(name, _)| name.clone())}
                            on_download={link.callback(|_| Msg::Download)}
                        />
                    </main>
                </div>
            </div>
        }
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;
    use jals_storage::{CodeTree, Entry};

    use super::*;

    // Only the tests still name the producing severity: production converts through
    // `DiagnosticSeverity::from` and never matches on it.
    use jals_project::ProjectDiagnosticSeverity;

    /// Switching backend is a `jals.toml` edit followed by *Build*, with no file switch in
    /// between — the case `Msg::Compile`'s "commit the live buffer first" rule exists for. What it
    /// relies on is that committing the manifest buffer is what the next `parse_manifest` reads,
    /// and that committing does not restart the dependency pipeline (a compile is not an edit).
    #[test]
    fn committing_the_open_manifest_buffer_is_what_a_compile_reads() {
        let mut app = App::initial();
        let edited = "[package]\nname = \"playground\"\n\n\
                      [build]\nbackend = { type = \"jals-wasm\" }\n";
        app.active_config = Some(ConfigKind::Manifest);
        app.commit_config_buffer(ConfigKind::Manifest, edited.to_owned());

        let Ok(manifest) = ConfigParseError::parse_manifest(&app.manifest_src) else {
            panic!("the committed buffer is what a compile parses");
        };
        assert_eq!(
            manifest.build.backend,
            jals_config::BackendKind::JalsWasm {}
        );
        // A commit must not have advanced the script/classpath pipeline behind the user's back.
        assert_eq!(app.build_generation.get(), 0);
    }

    /// There is one *Build* button and it follows `[build] backend`, so the seed has to name a
    /// backend that exists in a browser — the default is `javac`, which does not.
    #[test]
    fn the_seed_manifest_selects_the_in_process_backend() {
        let manifest: Manifest = ConfigKind::Manifest
            .seed()
            .parse()
            .expect("seed manifest is valid");
        assert_eq!(manifest.build.backend, jals_config::BackendKind::Jals {});
        assert_eq!(
            jals_build::RunTarget::resolve(&manifest, None),
            Ok("com.example.Main")
        );
    }

    /// Two things a marker needs that a `ProjectDiagnostic` may not carry: a range, and a severity
    /// in this editor's vocabulary. The range comes from the placement rule, so a diagnostic with no
    /// span still lands somewhere a reader can see — the first line of the model it is anchored to,
    /// and that model only.
    #[test]
    fn a_span_less_project_diagnostic_marks_the_first_line_of_its_own_model() {
        let mut app = App::initial();
        app.manifest_src = "[package]\r\nname = \"playground\"\r\n".to_owned();
        app.build_src = "build.error(\"boom\");\n".to_owned();
        let script = FileKey::parse(BUILD_SCRIPT_PATH).expect("script pseudo-path is valid");
        app.project_diagnostics = vec![
            ProjectDiagnostic {
                anchor: ProjectAnchor::Manifest,
                span: None,
                severity: ProjectDiagnosticSeverity::Info,
                code: jals_project::ProjectDiagnosticCode::DependencyCache,
                message: "some dependencies are not in the verified cache".to_owned(),
            },
            ProjectDiagnostic {
                anchor: ProjectAnchor::Script(script),
                span: None,
                severity: ProjectDiagnosticSeverity::Error,
                code: jals_project::ProjectDiagnosticCode::BuildScript,
                message: "boom".to_owned(),
            },
        ];

        // The manifest model gets the manifest-anchored one, on its first line — and the `\r` of a
        // CRLF buffer stays out of the range rather than being drawn as a character.
        assert_eq!(
            app.config_marker_entries(ConfigKind::Manifest, None),
            [(
                0..9,
                jals_editor::DiagnosticSeverity::Hint,
                "some dependencies are not in the verified cache",
            )]
        );
        // The script model gets the script-anchored one, placed against *its* buffer. Neither model
        // shows the other's, which is what makes placing against `config_src(kind)` sound.
        assert_eq!(
            app.config_marker_entries(ConfigKind::Script, None),
            [(0..20, jals_editor::DiagnosticSeverity::Error, "boom")]
        );
        // Nothing anchors to `jalsfmt.toml`; it is not part of project assembly.
        assert!(app.config_marker_entries(ConfigKind::Fmt, None).is_empty());
    }

    /// The span the assembly resolved, converted into Monaco's one-based UTF-16 coordinates —
    /// which is the whole of what this host still does with a Rhai position. Resolving the position
    /// itself is `BuildScriptPosition::byte_range`, tested in `jals-build`.
    #[test]
    fn a_script_failure_marks_the_position_it_reports() {
        block_on_inline(async {
            let manifest_text = ConfigKind::Manifest.seed();
            let manifest: Manifest = manifest_text.parse().expect("seed manifest is valid");
            for (script, expected) in [
                ("let valid = 1;\nlet broken = ;\n", Some(28..29)),
                ("let valid = 1;\nthrow \"boom\";\n", Some(15..16)),
                // `build.error` is reported by the script, not thrown by Rhai, so it has no
                // position and the marker falls back to the head of the file.
                ("build.error(\"boom\");\n", None),
            ] {
                let mut workspace = Workspace::new().await;
                let error = workspace
                    .run_build_script(&manifest, manifest_text, script)
                    .await
                    .expect_err("script should fail");
                let diagnostics =
                    App::script_diagnostics(ScriptOutcome::Failed(&error), &manifest, script);
                assert_eq!(
                    diagnostics
                        .iter()
                        .find(|d| d.severity == ProjectDiagnosticSeverity::Error)
                        .and_then(|d| d.span.clone()),
                    expected,
                    "script: {script:?}"
                );
            }
        });
    }

    #[test]
    fn reported_diagnostics_keep_their_own_severity_rather_than_one_message() {
        block_on_inline(async {
            let manifest_text = ConfigKind::Manifest.seed();
            let manifest: Manifest = manifest_text.parse().expect("seed manifest is valid");
            let script = "build.warning(\"check the version features\");\nbuild.error(\"select at most one\");\n";
            let mut workspace = Workspace::new().await;
            let error = workspace
                .run_build_script(&manifest, manifest_text, script)
                .await
                .expect_err("script should fail");

            // Two markers with two severities. The whole run used to be painted as one error
            // marker, so a warning that arrived before the fatal one read as part of it.
            let diagnostics =
                App::script_diagnostics(ScriptOutcome::Failed(&error), &manifest, script);
            assert_eq!(
                diagnostics
                    .iter()
                    .map(|d| (d.severity, d.message.as_str()))
                    .collect::<Vec<_>>(),
                [
                    (
                        ProjectDiagnosticSeverity::Warning,
                        "check the version features"
                    ),
                    (ProjectDiagnosticSeverity::Error, "select at most one"),
                ]
            );
        });
    }

    /// A marker's severity is typed on the editor's three-arm vocabulary, so the advisory collapses
    /// onto `Hint`. That renders faintly, which is why the status line keeps it too.
    #[test]
    fn a_multi_byte_column_converts_to_utf16() {
        let index = jals_editor::LineIndex::new("😀x");
        assert_eq!(
            MonacoRange::of(&index, "😀x", &(4..5)),
            MonacoRange {
                start_line: 1,
                start_col: 3,
                end_line: 1,
                end_col: 4,
            }
        );
    }

    #[test]
    fn build_status_reports_a_diagnostic_through_its_own_rendering() {
        block_on_inline(async {
            let manifest_text = ConfigKind::Manifest.seed();
            let manifest: Manifest = manifest_text.parse().expect("seed manifest is valid");
            let mut workspace = Workspace::new().await;
            let output = workspace
                .run_build_script(&manifest, manifest_text, "build.warning(\"kept\");\n")
                .await
                .expect("a warning does not fail the script");

            assert_eq!(
                App::build_status(output.as_ref()),
                "generated 0 file(s); 1 diagnostic(s): warning: kept"
            );
        });
    }

    #[test]
    fn invalidation_allows_identical_build_inputs_to_run_again() {
        let inputs = BuildInputs {
            manifest: "valid".to_string(),
            script: "script".to_string(),
            proxy: String::new(),
        };
        let mut tracker = BuildInputTracker::default();
        assert!(tracker.begin(inputs.clone()));
        assert!(!tracker.begin(inputs.clone()));
        tracker.invalidate();
        assert!(tracker.begin(inputs));
    }

    #[test]
    fn stale_build_tokens_do_not_become_current_again() {
        let generation = Rc::new(Cell::new(7));
        let token = BuildToken {
            generation: Rc::clone(&generation),
            captured: 7,
        };
        assert!(token.is_current());
        generation.set(8);
        assert!(!token.is_current());
    }

    /// The lowering rules themselves are pinned in `jals-classpath`; what this asserts is the
    /// playground's own contract — that a root-plan warning reaches the status line the header shows.
    #[test]
    fn root_classpath_lowering_warnings_reach_the_status_line() {
        block_on_inline(async {
            let mut manifest = Manifest::default();
            manifest.build.classpath = vec!["../escape.class".to_string()];
            let resolution = App::resolve_classpath(
                manifest,
                String::new(),
                MemoryStorage::memory(CodeTree::default()),
                ProjectScript::skipped(),
            )
            .await
            .unwrap();

            // The status line carries a summary of what the assembly reported; the markers carry
            // the detail. Both name the entry, because the warning renders whole.
            assert!(
                resolution.status.contains("warning: "),
                "{}",
                resolution.status
            );
            assert!(
                resolution.status.contains("path leaves the project root"),
                "{}",
                resolution.status
            );
            assert_eq!(
                resolution
                    .diagnostics
                    .iter()
                    .filter(|d| d.severity == ProjectDiagnosticSeverity::Warning)
                    .count(),
                1
            );
            // The entry the user wrote, which this host reports only because it renders the whole
            // warning: the lowering's message names no path, so a status line without the locator
            // tells a browser user their classpath is broken and nothing about where.
            assert!(
                resolution.status.contains("`../escape.class`"),
                "{}",
                resolution.status
            );
        });
    }

    #[test]
    fn detached_dependency_artifacts_must_be_utf8() {
        block_on_inline(async {
            let bytes = [0xff];
            let key = jals_storage::CacheKey::new(
                jals_storage::CacheNamespace::ExtractedSource,
                jals_storage::ContentDigest::of(b"invalid source"),
                jals_storage::ContentDigest::of(&bytes),
            );
            let mut storage = MemoryStorage::memory(CodeTree::default());
            storage.artifacts_mut().publish(&key, &bytes).await.unwrap();
            let inputs = jals_classpath::ProjectInputs {
                library_sources: vec![LibrarySource {
                    path: jals_storage::RelativePath::parse("Invalid.java").unwrap(),
                    key,
                }],
                ..jals_classpath::ProjectInputs::default()
            };

            let Err(error) = App::dependency_source_texts(&storage, &inputs).await else {
                panic!("non-UTF-8 Java sources must not reach the editor");
            };
            assert!(error.contains("not valid UTF-8"), "{error}");
        });
    }

    #[test]
    fn browser_resolution_runs_the_memory_graph_and_returns_detached_source_texts() {
        block_on_inline(async {
            let manifest: Manifest = "[package]\nfeatures = [\"java24\"]\n\
                [dependencies]\nchild = { path = \"deps/child\" }\n\
                git-dep = { git = \"https://example.invalid/repository.git\" }\n"
                .parse()
                .unwrap();
            let storage = MemoryStorage::memory(
                CodeTree::new([
                    Entry::File(
                        FileKey::parse("deps/child/jals.toml").unwrap(),
                        b"[build]\nsource-dirs = [\"src\"]\nclasspath = [\"lib/Box.class\"]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n".to_vec(),
                    ),
                    Entry::File(
                        FileKey::parse("deps/child/build.rhai").unwrap(),
                        br#"let source = output.write_text("Generated.java", "class Generated {}"); build.add_source(source);"#.to_vec(),
                    ),
                    Entry::File(
                        FileKey::parse("deps/child/src/Child.java").unwrap(),
                        b"class Child {}".to_vec(),
                    ),
                    Entry::File(
                        FileKey::parse("deps/child/lib/Box.class").unwrap(),
                        include_bytes!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/../jals-classpath/tests/fixtures/Box.class"
                        ))
                        .to_vec(),
                    ),
                ])
                .unwrap(),
            );

            let expected_features = manifest.feature_set();
            let resolution =
                App::resolve_classpath(manifest, String::new(), storage, ProjectScript::skipped())
                    .await
                    .unwrap();

            assert_eq!(resolution.feature_set, expected_features);
            assert!(
                resolution
                    .status
                    .contains("Git dependencies cannot be acquired")
            );
            assert_eq!(resolution.sources.source_deps.len(), 2);
            assert!(resolution.sources.source_deps.iter().any(|(key, text)| {
                key.to_string().ends_with("Generated.java") && text == "class Generated {}"
            }));
            assert!(
                resolution
                    .sources
                    .source_deps
                    .iter()
                    .all(|(key, _)| key.to_string().starts_with(".jals/source-dependency/"))
            );
            assert!(
                resolution
                    .sources
                    .library
                    .iter()
                    .any(|(key, _)| key.to_string().starts_with(".jals/library/"))
            );
        });
    }

    #[test]
    fn sidebar_entries_are_flattened_from_sorted_file_keys() {
        let workspace = block_on_inline(Workspace::new());
        let entries = App::tree_entries(&workspace);
        let rows: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.path.as_str(),
                    entry.name.as_str(),
                    entry.depth,
                    entry.is_dir,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("com", "com", 0, true),
                ("com/example", "example", 1, true),
                ("com/example/Greeter.java", "Greeter.java", 2, false,),
                ("com/example/Main.java", "Main.java", 2, false),
            ]
        );
    }
}
