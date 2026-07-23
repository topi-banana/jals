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
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;

use futures::lock::Mutex;
use jals_build::build_script::{
    BuildScriptDiagnostic, BuildScriptEnvironment, BuildScriptError, BuildScriptLimits,
    BuildScriptOutput,
};
use jals_classpath::{ClasspathEntry, LibrarySource, ProjectInputOptions, SourceFile};
use jals_config::fmt::Config;
use jals_config::{FeatureSet, Manifest, ManifestParseError};
use jals_hir::{LoweredClasspath, ProjectIndex};
use jals_project::{GraphWarning, MemoryProjectGraph};
use jals_storage::{
    ArtifactCache, DirKey, EntryRef, FileKey, MemoryCache, MemoryStorage, Name, ProjectView,
    RelativePath,
};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::components::{EditorPane, FileTree, Header, SyntaxPane, TreeEntry};
use crate::fetcher::BrowserFetcher;
use crate::host::{MonacoRange, PlaygroundDiagnostic};
use crate::workspace::{BUILD_SCRIPT_PATH, MANIFEST_PATH, Workspace};
use crate::{monaco, providers};

/// One of the editable project files shown in the sidebar's `Config` section.
/// They are never analysed or indexed as Java and use plaintext Monaco models.
#[derive(Clone, Copy, PartialEq, Eq)]
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
            // An empty (commented) `[dependencies]` table. A CORS-permissive jar resolves directly;
            // Maven Central needs the header's CORS proxy.
            ConfigKind::Manifest => {
                "[build]\n\
                 script = { type = \"rhai\", file = \"build.rhai\" }\n\
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

/// A build failure retained in UI state with its optional Rhai source location.
struct BuildFailure {
    message: String,
    script_path: Option<String>,
    position: Option<(u32, u32)>,
}

impl BuildFailure {
    fn from_error(error: BuildScriptError) -> Self {
        let script_path = error.script_path().map(ToString::to_string);
        let position = error
            .position()
            .map(|position| (position.line(), position.column()));
        let message = match error {
            BuildScriptError::ReportedErrors(diagnostics) => diagnostics
                .iter()
                .map(BuildScriptDiagnostic::message)
                .collect::<Vec<_>>()
                .join("; "),
            error => error.to_string(),
        };
        Self {
            message,
            script_path,
            position,
        }
    }

    fn marker_range(&self, source: &str, fallback: MonacoRange) -> MonacoRange {
        let Some((line, column)) = self.script_path.as_ref().and(self.position) else {
            return fallback;
        };
        let Some(line_index) = line
            .checked_sub(1)
            .and_then(|line| usize::try_from(line).ok())
        else {
            return fallback;
        };
        let Some(character_index) = column
            .checked_sub(1)
            .and_then(|column| usize::try_from(column).ok())
        else {
            return fallback;
        };
        let mut line_start = 0;
        let mut selected = None;
        for (index, line_text) in source.split_inclusive('\n').enumerate() {
            if index == line_index {
                selected = Some(line_text.strip_suffix('\n').unwrap_or(line_text));
                break;
            }
            line_start += line_text.len();
        }
        let Some(line_text) = selected else {
            return fallback;
        };
        let relative = line_text
            .char_indices()
            .map(|(offset, _)| offset)
            .nth(character_index)
            .or_else(|| (character_index == line_text.chars().count()).then_some(line_text.len()));
        let Some(start) = relative.map(|offset| line_start + offset) else {
            return fallback;
        };
        let end = if start < line_start + line_text.len() {
            start + source[start..].chars().next().map_or(0, char::len_utf8)
        } else {
            start
        };
        MonacoRange::of(&jals_editor::LineIndex::new(source), source, &(start..end))
    }
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
    status: String,
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

/// A build generation captured by an async script/classpath pipeline. New valid manifest or script
/// edits invalidate all older results, while the workspace lock still serializes aggregate writes.
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
        diagnostics: Vec<BuildScriptDiagnostic>,
    },
    /// Rhai compilation/evaluation failed without publishing partial generated output.
    BuildFailed {
        generation: u64,
        error: String,
        script_path: Option<String>,
        position: Option<(u32, u32)>,
    },
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
    /// The latest build-script/classpath status line shown in the [`Header`], if any.
    deps_status: Option<String>,
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
    /// Whether Monaco has been created; generated model/marker writes wait for this point.
    editor_ready: bool,
    /// Diagnostics reported by the most recent successful script execution.
    build_diagnostics: Vec<BuildScriptDiagnostic>,
    /// Compilation/runtime failure from the most recent script execution.
    build_error: Option<BuildFailure>,
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

    /// Paint the config editor's parse diagnostics on the current model: a single error marker
    /// derived from `error` (spanning its byte range, or the first line when the error carries no
    /// span), or no markers when `error` is `None` (a clean parse). Reuses the Java marker path.
    fn set_config_diagnostic(&self, text: &str, error: Option<ConfigParseError>) {
        /// The byte length of `text`'s first line (up to the first `\n`, or the whole string) — the
        /// fallback marker range for a config error that carries no span.
        fn first_line_len(text: &str) -> usize {
            text.find('\n').unwrap_or(text.len())
        }
        let marker = error.as_ref().map(|ConfigParseError { span, message }| {
            let range = span.clone().unwrap_or_else(|| 0..first_line_len(text));
            // Built only when there is an error to place — a clean parse (the common keystroke) skips
            // the whole-buffer scan.
            let index = jals_editor::LineIndex::new(text);
            let MonacoRange {
                start_line,
                start_col,
                end_line,
                end_col,
            } = MonacoRange::of(&index, text, &range);
            monaco::Marker {
                start_line,
                start_col,
                end_line,
                end_col,
                message: message.as_str(),
                severity: jals_editor::DiagnosticSeverity::Error,
            }
        });
        // `Option<Marker>` is an iterator of zero or one marker; either paints the error or clears.
        monaco::Marker::set_diagnostics(marker);
    }

    /// Paint Rhai failures at their structured source position on the fixed script editor model,
    /// regardless of the configured storage path. Diagnostics without a position and successful
    /// build warnings use the first line.
    fn set_build_diagnostics(&self) {
        let first_line = 0..self.build_src.find('\n').unwrap_or(self.build_src.len());
        let index = jals_editor::LineIndex::new(&self.build_src);
        let fallback_range = MonacoRange::of(&index, &self.build_src, &first_line);
        let mut markers = Vec::new();
        for diagnostic in &self.build_diagnostics {
            markers.push(monaco::Marker {
                start_line: fallback_range.start_line,
                start_col: fallback_range.start_col,
                end_line: fallback_range.end_line,
                end_col: fallback_range.end_col,
                message: diagnostic.message(),
                severity: if diagnostic.is_error() {
                    jals_editor::DiagnosticSeverity::Error
                } else {
                    jals_editor::DiagnosticSeverity::Warning
                },
            });
        }
        if let Some(failure) = &self.build_error {
            let range = failure.marker_range(&self.build_src, fallback_range);
            markers.push(monaco::Marker {
                start_line: range.start_line,
                start_col: range.start_col,
                end_line: range.end_line,
                end_col: range.end_col,
                message: &failure.message,
                severity: jals_editor::DiagnosticSeverity::Error,
            });
        }
        monaco::Marker::set_diagnostics_for(BUILD_SCRIPT_PATH, markers);
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
        self.set_config_diagnostic(text, error);
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
                self.set_config_diagnostic(text, Some(err));
                return true;
            }
        };
        self.set_config_diagnostic(text, None);
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
        self.build_diagnostics.clear();
        self.build_error = None;
        if self.editor_ready {
            self.set_build_diagnostics();
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
                    Ok(output) => {
                        let status = Self::build_status(output.as_ref());
                        let diagnostics = output
                            .as_ref()
                            .map_or_else(Vec::new, |output| output.diagnostics.clone());
                        let additional_classpath =
                            output.as_ref().map_or_else(Vec::new, |output| {
                                output.additional_classpath.iter().cloned().collect()
                            });
                        let entries = Self::tree_entries(&ws);
                        let files = ws.file_texts();
                        let active_path = ws.active().to_string();
                        let active_source = ws.active_source();
                        let markers = Self::markers_of(&ws).await;
                        let storage = ws.storage_snapshot();
                        Ok((
                            status,
                            diagnostics,
                            additional_classpath,
                            entries,
                            files,
                            active_path,
                            active_source,
                            markers,
                            storage,
                        ))
                    }
                    Err(error) => Err(BuildFailure::from_error(error)),
                }
            };

            if !token.is_current() {
                return;
            }
            let (
                build_status,
                diagnostics,
                additional_classpath,
                entries,
                files,
                active_path,
                active_source,
                markers,
                storage,
            ) = match build_result {
                Ok(result) => result,
                Err(BuildFailure {
                    message: error,
                    script_path,
                    position,
                }) => {
                    link.send_message(Msg::BuildFailed {
                        generation: token.captured,
                        error,
                        script_path,
                        position,
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

            let result = Self::resolve_classpath(manifest, proxy, storage, additional_classpath)
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

    /// Human-readable result of a successful script phase.
    fn build_status(output: Option<&BuildScriptOutput>) -> String {
        let Some(output) = output else {
            return "build script disabled".to_string();
        };
        let mut status = format!("generated {} file(s)", output.generated_files.len());
        if !output.diagnostics.is_empty() {
            status.push_str(&format!(
                "; {} warning(s): {}",
                output.diagnostics.len(),
                output
                    .diagnostics
                    .iter()
                    .map(BuildScriptDiagnostic::message)
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
        manifest: Manifest,
        proxy: String,
        mut storage: MemoryStorage,
        additional_classpath: Vec<FileKey>,
    ) -> Result<ClasspathResolution, String> {
        let fetcher = BrowserFetcher::new(proxy);
        let graph = MemoryProjectGraph::discover(&manifest, &storage.view())
            .await
            .map_err(|error| error.to_string())?;
        // No command line here either, so what the root forwards to its dependencies comes from its
        // own `default` list — the same selection the root script above ran under. With nothing
        // selected, resolution cannot fail.
        let features = manifest
            .resolve_build_features(&[], false, false)
            .unwrap_or_default();
        let exec = storage.exec().clone();
        let graph = graph
            .preprocess(
                storage.artifacts_mut(),
                jals_project::GraphPreprocess {
                    exec: &exec,
                    // A dependency's build-task fetches go through the same CORS proxy as
                    // dependency resolution below; nothing else in the browser can reach a host.
                    fetcher: &fetcher,
                    environment: &BuildScriptEnvironment::new(),
                    root_features: &features,
                    limits: &BuildScriptLimits::default(),
                    network: jals_classpath::NetworkPolicy::Online,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut assembly = graph.assemble(storage.artifacts_mut()).await;
        if !assembly.errors.is_empty() {
            return Err(assembly
                .errors
                .iter()
                .map(|error| match &error.path {
                    Some(path) => format!("dependency {} `{path}`: {}", error.node, error.message),
                    None => format!("dependency {}: {}", error.node, error.message),
                })
                .collect::<Vec<_>>()
                .join("; "));
        }
        assembly.plan.feature_set = manifest.feature_set();
        let graph_classpath = std::mem::take(&mut assembly.plan.classpath);
        let (classpath_entries, root_classpath_warnings) = Self::ordered_classpath_entries(
            &manifest,
            &storage.view(),
            additional_classpath,
            graph_classpath,
        );
        assembly.plan.classpath = classpath_entries;
        let inputs = jals_classpath::ProjectInputs::assemble(
            &fetcher,
            &mut storage,
            &assembly.plan,
            ProjectInputOptions::Editor,
        )
        .await;
        let classpath = ProjectIndex::lower_classpath(&inputs.classpath_classes).await;
        let sources = Self::dependency_source_texts(&storage, &inputs).await?;
        let mut status = format!(
            "resolved {} class(es) from {} jar(s)",
            inputs.classpath_classes.len(),
            inputs.dependency_jars.len()
        );
        let mut warnings = root_classpath_warnings;
        warnings.extend(assembly.warnings.iter().map(Self::graph_warning_message));
        warnings.extend(inputs.warnings.into_iter().map(|warning| warning.message));
        if !warnings.is_empty() {
            status.push_str(&format!(
                " — {} warning(s): {}",
                warnings.len(),
                warnings.join("; ")
            ));
        }
        Ok(ClasspathResolution {
            classpath,
            feature_set: inputs.feature_set,
            status,
            artifacts: storage.into_artifacts(),
            sources,
        })
    }

    /// Lower root-project classpath strings and place all browser classpath groups in host order.
    fn ordered_classpath_entries(
        manifest: &Manifest,
        view: &ProjectView,
        additional_classpath: Vec<FileKey>,
        dependency_classpath: Vec<ClasspathEntry>,
    ) -> (Vec<ClasspathEntry>, Vec<String>) {
        let mut entries = Vec::new();
        let mut warnings = Vec::new();
        for raw in &manifest.build.classpath {
            let path = match Self::normalize_root_path(raw) {
                Ok(path) => path,
                Err(message) => {
                    warnings.push(format!("root classpath `{raw}` is invalid: {message}"));
                    continue;
                }
            };
            let found = FileKey::new(path.clone())
                .ok()
                .as_ref()
                .and_then(|key| view.tree().lookup_file(key))
                .or_else(|| view.tree().lookup_dir(&DirKey::new(path)));
            match found {
                Some(EntryRef::File(file)) => {
                    entries.push(ClasspathEntry::ProjectFile(file.key().clone()))
                }
                Some(EntryRef::Directory(directory)) => {
                    entries.push(ClasspathEntry::ProjectDirectory(directory.clone()))
                }
                None => warnings.push(format!("root classpath `{raw}` is missing or invalid")),
            }
        }
        entries.extend(
            additional_classpath
                .into_iter()
                .map(ClasspathEntry::ProjectFile),
        );
        entries.extend(dependency_classpath);
        (entries, warnings)
    }

    /// Normalize one root-relative portable path, accepting `.` while rejecting root escape.
    fn normalize_root_path(raw: &str) -> Result<RelativePath, String> {
        if raw.starts_with('/')
            || raw.starts_with('\\')
            || (raw.as_bytes().get(1) == Some(&b':') && raw.as_bytes()[0].is_ascii_alphabetic())
        {
            return Err("path must be relative to the project root".to_string());
        }
        if raw.contains('\\') {
            return Err("path must use portable `/` separators".to_string());
        }
        let mut segments = Vec::new();
        for part in raw.split('/') {
            match part {
                "." | "" => {}
                ".." => {
                    if segments.pop().is_none() {
                        return Err("path leaves the project root".to_string());
                    }
                }
                part => segments.push(
                    Name::new(part)
                        .map_err(|error| format!("path contains an invalid segment: {error:?}"))?,
                ),
            }
        }
        Ok(RelativePath::new(segments))
    }

    fn graph_warning_message(warning: &GraphWarning) -> String {
        match (&warning.dependency, &warning.node) {
            (Some(dependency), _) => format!("dependency `{dependency}`: {}", warning.message),
            (None, Some(node)) => format!("dependency project {node}: {}", warning.message),
            (None, None) => warning.message.clone(),
        }
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
        App {
            workspace: None,
            config: Rc::new(RefCell::new(Config::default())),
            tree_entries: Vec::new(),
            active_path: String::new(),
            active_source: String::new(),
            syntax_dump: None,
            deps_status: None,
            manifest_src: ConfigKind::Manifest.seed().to_string(),
            fmt_src: ConfigKind::Fmt.seed().to_string(),
            build_src: ConfigKind::Script.seed().to_string(),
            active_config: None,
            selection_generation: Rc::new(Cell::new(0)),
            build_generation: Rc::new(Cell::new(0)),
            editor_ready: false,
            build_diagnostics: Vec::new(),
            build_error: None,
            build_inputs: BuildInputTracker::default(),
            proxy: String::new(),
        }
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
                        self.set_build_diagnostics();
                    } else {
                        self.set_config_diagnostic(&src, kind.parse_error(&src));
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
                    let formatted = ws.format_active(&config).await.formatted;
                    if !selection.is_current() {
                        return;
                    }
                    monaco::update_model(&formatted);
                    ws.sync_active(&formatted).await;
                    let path = ws.active().to_string();
                    Self::report_active(&ws, &link, path, formatted, want_syntax, &selection).await;
                });
                false
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
                spawn_local(async move {
                    let mut ws = workspace.lock().await;
                    // Flush the live buffer first, so the dump matches what the editor shows.
                    ws.sync_active(&live).await;
                    link.send_message(Msg::SyntaxDumped(App::dump_of(&ws).await));
                });
                false
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
                self.set_build_diagnostics();
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
                self.build_diagnostics = diagnostics;
                self.build_error = None;
                if self.editor_ready {
                    if self.active_config.is_none() {
                        if active_changed {
                            monaco::switch_model(&active_path, &active_source);
                        } else {
                            monaco::update_model(&active_source);
                        }
                    }
                    Self::sync_models(&files);
                    self.set_build_diagnostics();
                }
                true
            }
            Msg::BuildFailed {
                generation,
                error,
                script_path,
                position,
            } => {
                if generation != self.build_generation.get() {
                    return false;
                }
                self.deps_status = Some(format!("build error: {error}"));
                self.build_diagnostics.clear();
                self.build_error = Some(BuildFailure {
                    message: error,
                    script_path,
                    position,
                });
                if self.editor_ready {
                    self.set_build_diagnostics();
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
                    on_proxy_change={link.callback(Msg::SetProxy)}
                    deps_status={self.deps_status.clone()}
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
                        <SyntaxPane dump={self.syntax_dump.clone()} />
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

    #[test]
    fn build_failure_marker_uses_structured_rhai_position_or_fallback() {
        block_on_inline(async {
            let manifest_text = ConfigKind::Manifest.seed();
            let manifest: Manifest = manifest_text.parse().expect("seed manifest is valid");
            let fallback = MonacoRange {
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 8,
            };
            for (script, expected) in [
                (
                    "let valid = 1;\nlet broken = ;\n",
                    MonacoRange {
                        start_line: 2,
                        start_col: 14,
                        end_line: 2,
                        end_col: 15,
                    },
                ),
                (
                    "let valid = 1;\nthrow \"boom\";\n",
                    MonacoRange {
                        start_line: 2,
                        start_col: 1,
                        end_line: 2,
                        end_col: 2,
                    },
                ),
                ("build.error(\"boom\");\n", fallback),
            ] {
                let mut workspace = Workspace::new().await;
                let error = workspace
                    .run_build_script(&manifest, manifest_text, script)
                    .await
                    .expect_err("script should fail");
                let failure = BuildFailure::from_error(error);
                assert_eq!(failure.marker_range(script, fallback), expected);
            }
        });
    }

    #[test]
    fn build_failure_marker_converts_character_column_to_utf16() {
        let failure = BuildFailure {
            message: "boom".to_string(),
            script_path: Some("scripts/custom.rhai".to_string()),
            position: Some((1, 2)),
        };
        let fallback = MonacoRange {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        };
        assert_eq!(
            failure.marker_range("😀x", fallback),
            MonacoRange {
                start_line: 1,
                start_col: 3,
                end_line: 1,
                end_col: 4,
            }
        );
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

    #[test]
    fn root_classpath_files_directories_and_group_order_are_portable() {
        block_on_inline(async {
            let class = include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../jals-classpath/tests/fixtures/Box.class"
            ));
            let root_file = FileKey::parse("lib/Box.class").unwrap();
            let directory_file = FileKey::parse("classes/Box.class").unwrap();
            let script_file = FileKey::parse("generated/Script.class").unwrap();
            let dependency_file = FileKey::parse("dependency/Graph.class").unwrap();
            let storage = MemoryStorage::memory(
                CodeTree::new([
                    Entry::File(root_file.clone(), class.to_vec()),
                    Entry::File(directory_file, class.to_vec()),
                    Entry::File(script_file.clone(), class.to_vec()),
                    Entry::File(dependency_file.clone(), class.to_vec()),
                ])
                .unwrap(),
            );
            let mut manifest = Manifest::default();
            manifest.build.classpath = vec!["lib/./Box.class".to_string(), "classes".to_string()];

            let (entries, warnings) = App::ordered_classpath_entries(
                &manifest,
                &storage.view(),
                vec![script_file.clone()],
                vec![ClasspathEntry::ProjectFile(dependency_file.clone())],
            );

            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(
                entries,
                vec![
                    ClasspathEntry::ProjectFile(root_file),
                    ClasspathEntry::ProjectDirectory(DirKey::parse("classes").unwrap()),
                    ClasspathEntry::ProjectFile(script_file),
                    ClasspathEntry::ProjectFile(dependency_file),
                ],
                "root manifest, root script, then graph dependency classpath"
            );
            let load = jals_classpath::ClasspathLoad::load(
                storage.exec(),
                &storage.view(),
                storage.artifacts(),
                &entries,
            )
            .await;
            assert!(load.warnings.is_empty(), "{:?}", load.warnings);
            assert_eq!(load.classes.len(), 4);
        });
    }

    #[test]
    fn malformed_root_classpath_warnings_are_visible_and_ordered() {
        block_on_inline(async {
            let mut manifest = Manifest::default();
            manifest.build.classpath = vec![
                "../escape.class".to_string(),
                "bad:name.class".to_string(),
                "missing.class".to_string(),
            ];
            let resolution = App::resolve_classpath(
                manifest,
                String::new(),
                MemoryStorage::memory(CodeTree::default()),
                Vec::new(),
            )
            .await
            .unwrap();

            assert!(
                resolution.status.contains("3 warning(s)"),
                "{}",
                resolution.status
            );
            let escape = resolution.status.find("`../escape.class`").unwrap();
            let invalid = resolution.status.find("`bad:name.class`").unwrap();
            let missing = resolution.status.find("`missing.class`").unwrap();
            assert!(
                escape < invalid && invalid < missing,
                "{}",
                resolution.status
            );
            assert!(resolution.status.contains("path leaves the project root"));
            assert!(resolution.status.contains("is missing or invalid"));
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
                    path: RelativePath::parse("Invalid.java").unwrap(),
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
            let resolution = App::resolve_classpath(manifest, String::new(), storage, Vec::new())
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
