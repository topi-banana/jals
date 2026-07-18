//! The root [`App`] component: it owns all playground state and orchestrates the UI.
//!
//! `App` holds the in-memory [`Workspace`] (behind a `futures::lock::Mutex`), the shared
//! formatter [`Config`], the two editable configuration buffers (`jals.toml` / `jalsfmt.toml`),
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

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;

use futures::lock::Mutex;
use jals_config::fmt::Config;
use jals_config::{Dependency, FeatureSet, ManifestParseError};
use jals_hir::{LoweredClasspath, ProjectIndex};
use jals_storage::{ArtifactCache, MemoryCache, MemoryStorage};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::components::{EditorPane, FileTree, Header, SyntaxPane, TreeEntry};
use crate::fetcher::BrowserFetcher;
use crate::host::{MonacoRange, PlaygroundDiagnostic};
use crate::workspace::Workspace;
use crate::{monaco, providers};

/// One of the two editable project configuration files shown in the sidebar's `Config` section.
/// They live outside the [`Workspace`]'s Java file tree (so they are never analysed or indexed) and
/// are edited as plain TOML in the same Monaco editor as the Java files.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigKind {
    /// `jals.toml` — the project manifest; its `[dependencies]` drive classpath resolution.
    Manifest,
    /// `jalsfmt.toml` — the formatter configuration.
    Fmt,
}

impl ConfigKind {
    /// Every config kind, in sidebar order — the single source for [`ConfigKind::from_path`] and the
    /// file-tree's `Config` section.
    const ALL: [ConfigKind; 2] = [ConfigKind::Manifest, ConfigKind::Fmt];

    /// This config's pseudo-path — its Monaco model key and file-tree selection key.
    const fn path(self) -> &'static str {
        match self {
            ConfigKind::Manifest => "jals.toml",
            ConfigKind::Fmt => "jalsfmt.toml",
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
                "[dependencies]\n\
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
        }
    }
}

/// A config-editor parse error to paint as a marker: an optional byte `span` (the marker range; a
/// structural error carrying none falls back to the buffer's first line) plus the `message`.
struct ConfigParseError {
    span: Option<Range<usize>>,
    message: String,
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
    ClasspathResolved(
        Result<
            (
                LoweredClasspath,
                FeatureSet,
                String,
                ArtifactCache<MemoryCache>,
            ),
            String,
        >,
    ),
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
    /// Sync mirror of the workspace's file tree, flattened pre-order for the [`FileTree`]. The
    /// seeded tree never gains or loses files, so it is captured once at [`Msg::WorkspaceReady`].
    tree_entries: Vec<TreeEntry>,
    /// Sync mirror of the active Java file's path (the pane label / tree highlight).
    active_path: String,
    /// Sync mirror of the active Java file's last-known source — the [`EditorPane`]'s first-mount
    /// model seed (Monaco owns the live text afterwards).
    active_source: String,
    /// The most recent syntax-tree dump shown in the right pane, if any.
    syntax_dump: Option<String>,
    /// The last dependency-resolution status line shown in the [`Header`], if any.
    deps_status: Option<String>,
    /// The `jals.toml` editor buffer. Held here (not in the workspace's Java file tree) so it is
    /// never analysed/indexed; its `[dependencies]` are re-resolved on edit.
    manifest_src: String,
    /// The `jalsfmt.toml` editor buffer. Parsed into the shared formatter [`Config`] on edit.
    fmt_src: String,
    /// Which config file is open in the editor, or `None` when a Java workspace file is active.
    active_config: Option<ConfigKind>,
    /// The CORS proxy for jar downloads (typed in the header); empty by default.
    proxy: String,
    /// The `([dependencies], proxy)` of the last resolve kicked off from a `jals.toml` edit. A
    /// re-edit that leaves both unchanged (a comment / whitespace / other section) produces the same
    /// classpath, so [`App::apply_manifest`] skips re-running the download + jar-parse + lower
    /// pipeline; `None` until the first resolve.
    last_resolve_key: Option<(BTreeMap<String, Dependency>, String)>,
}

impl App {
    /// Flatten `workspace`'s files into a pre-order [`TreeEntry`] list for the [`FileTree`].
    fn tree_entries(workspace: &Workspace) -> Vec<TreeEntry> {
        let mut out = Vec::new();
        Self::collect_entries(workspace, "", 0, &mut out);
        out
    }

    /// Append the children of directory `dir` (each at indentation `depth`) to `out`, recursing
    /// into subdirectories so the whole tree is flattened in pre-order.
    fn collect_entries(workspace: &Workspace, dir: &str, depth: usize, out: &mut Vec<TreeEntry>) {
        for (path, is_dir) in workspace.read_dir(dir) {
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            out.push(TreeEntry {
                path: path.clone(),
                name,
                depth,
                is_dir,
            });
            if is_dir {
                Self::collect_entries(workspace, &path, depth + 1, out);
            }
        }
    }

    /// The shared workspace handle, or `None` while the async construction is still running (the
    /// editor pane is not mounted yet, so handlers needing it have nothing to do).
    fn workspace(&self) -> Option<Rc<Mutex<Workspace>>> {
        self.workspace.clone()
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
        }
    }

    /// The current buffer text of config file `kind`.
    fn config_src(&self, kind: ConfigKind) -> &str {
        match kind {
            ConfigKind::Manifest => &self.manifest_src,
            ConfigKind::Fmt => &self.fmt_src,
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

    /// Parse + validate `jals.toml` text and, on success, kick off an async `[dependencies]` resolve
    /// (reusing the already-downloaded jars); on failure paint the parse/validation error. Returns
    /// whether the header needs a re-render (a resolve was started, showing "resolving…").
    ///
    /// The resolve is skipped when neither the `[dependencies]` nor the proxy changed since the last
    /// one — a comment / whitespace / other-section edit yields the same classpath, so the full
    /// download-clone + jar-parse + classpath-lower pipeline would be wasted work.
    fn apply_manifest(&mut self, ctx: &Context<Self>, text: &str) -> bool {
        let manifest = match ConfigParseError::parse_manifest(text) {
            Ok(manifest) => manifest,
            Err(err) => {
                self.set_config_diagnostic(text, Some(err));
                return false;
            }
        };
        self.set_config_diagnostic(text, None);
        // Reference-compare against the last resolve's key first, so an unchanged edit (a comment /
        // whitespace keystroke — see above) skips the resolve without cloning the dependency map.
        let unchanged = self
            .last_resolve_key
            .as_ref()
            .is_some_and(|(deps, proxy)| deps == &manifest.dependencies && proxy == &self.proxy);
        if unchanged {
            return false;
        }
        let Some(workspace) = self.workspace() else {
            return false;
        };
        self.last_resolve_key = Some((manifest.dependencies.clone(), self.proxy.clone()));
        self.deps_status = Some("resolving…".to_string());
        let proxy = self.proxy.clone();
        let link = ctx.link().clone();
        spawn_local(async move {
            // Clone one immutable revision + cache under the lock; the resolve itself then runs
            // detached, without holding the workspace lock across the network/parse pipeline.
            let storage = workspace.lock().await.storage_snapshot();
            let result = Self::resolve_classpath(manifest, proxy, storage).await;
            link.send_message(Msg::ClasspathResolved(result));
        });
        true
    }

    /// Assemble a parsed `manifest`'s analysis inputs into a lowered classpath, in the browser:
    /// download each remote `[dependencies]` jar with a [`BrowserFetcher`] into the in-memory `cache`,
    /// load the `.class` files, and lower them for the project index. Returns the classpath, the
    /// resolved feature set from `[package] features` (for the feature-gated lint rules), and a
    /// human-readable status line (class/jar counts plus any warnings), or an error message.
    ///
    /// The whole resolution runs against a detached storage snapshot (on the same execution
    /// context, cloned with it) so the workspace lock is never held across an `.await` here; only
    /// its verified artifact cache is merged back afterwards.
    async fn resolve_classpath(
        manifest: jals_config::Manifest,
        proxy: String,
        mut storage: MemoryStorage,
    ) -> Result<
        (
            LoweredClasspath,
            FeatureSet,
            String,
            ArtifactCache<MemoryCache>,
        ),
        String,
    > {
        let fetcher = BrowserFetcher::new(proxy);
        let mut plan = jals_classpath::ProjectInputPlan {
            feature_set: manifest.feature_set(),
            ..jals_classpath::ProjectInputPlan::default()
        };
        // The browser has no project filesystem: every jar locator is external, fetched through
        // the proxy. The lowering itself is the one shared with the native host.
        let mut warnings = Vec::new();
        plan.add_jar_dependencies(
            &manifest,
            |locator| jals_classpath::DependencyLocation::External {
                locator: jals_classpath::ExternalLocator::new(locator),
                expected: None,
            },
            &mut warnings,
        );
        let mut inputs = jals_classpath::ProjectInputs::assemble(
            &fetcher,
            &mut storage,
            &plan,
            jals_classpath::ProjectInputOptions::Analysis,
        )
        .await;
        warnings.append(&mut inputs.warnings);
        inputs.warnings = warnings;
        let classpath = ProjectIndex::lower_classpath(&inputs.classpath_classes).await;
        let mut status = format!(
            "resolved {} class(es) from {} jar(s)",
            inputs.classpath_classes.len(),
            inputs.dependency_jars.len()
        );
        if !inputs.warnings.is_empty() {
            status.push_str(&format!(
                " — {} warning(s): {}",
                inputs.warnings.len(),
                inputs
                    .warnings
                    .iter()
                    .map(|warning| warning.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        Ok((
            classpath,
            inputs.feature_set,
            status,
            storage.into_artifacts(),
        ))
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
            active_config: None,
            proxy: String::new(),
            last_resolve_key: None,
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
                true
            }
            // Route the edit by what is open: a config buffer parses into its effect (formatter
            // config / dependency resolve) and repaints config markers; a Java file syncs the
            // workspace overlay and recomputes its markers. Monaco owns the live text.
            Msg::EditorChanged(value) => match self.active_config {
                // A manifest edit resolves its `[dependencies]` and stores the buffer; re-render only
                // when a resolve actually started (the header shows "resolving…").
                Some(ConfigKind::Manifest) => {
                    let rerender = self.apply_manifest(ctx, &value);
                    self.manifest_src = value;
                    rerender
                }
                // The formatter config reparses into the shared `Config` (repainting its markers).
                Some(ConfigKind::Fmt) => {
                    self.commit_config_buffer(ConfigKind::Fmt, value);
                    false
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
                // Flush the live editor text into the (still-active) file/buffer before switching.
                let live = monaco::current_value();
                if let Some(kind) = ConfigKind::from_path(&path) {
                    match self.active_config {
                        Some(outgoing) => self.commit_config_buffer(outgoing, live),
                        // Overlay-only: fresh Java markers would land on the config model.
                        None => self.flush_active_java(live),
                    }
                    self.active_config = Some(kind);
                    let src = self.config_src(kind).to_string();
                    monaco::switch_model(&path, &src);
                    // Show this config's current parse state (selecting never triggers a resolve).
                    self.set_config_diagnostic(&src, kind.parse_error(&src));
                } else {
                    let Some(workspace) = self.workspace() else {
                        return false;
                    };
                    // The outgoing Java flush and the switch share one lock hold, so the flush
                    // lands on the outgoing file before `set_active` moves the anchor.
                    let outgoing_java = match self.active_config {
                        Some(outgoing) => {
                            self.commit_config_buffer(outgoing, live);
                            None
                        }
                        None => Some(live),
                    };
                    self.active_config = None;
                    self.active_path = path.clone();
                    let want_syntax = self.syntax_dump.is_some();
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        let mut ws = workspace.lock().await;
                        if let Some(text) = outgoing_java {
                            ws.sync_active(&text).await;
                        }
                        ws.set_active(&path);
                        let source = ws.active_source();
                        monaco::switch_model(&path, &source);
                        let syntax = if want_syntax {
                            Some(App::dump_of(&ws).await)
                        } else {
                            None
                        };
                        let markers = App::markers_of(&ws).await;
                        link.send_message(Msg::ActiveRefreshed {
                            path,
                            source,
                            syntax,
                        });
                        link.send_message(markers);
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
                spawn_local(async move {
                    let mut ws = workspace.lock().await;
                    // Flush the live buffer, format it, and rewrite the editor in place.
                    ws.sync_active(&live).await;
                    let formatted = ws.format_active(&config).await.formatted;
                    monaco::update_model(&formatted);
                    ws.sync_active(&formatted).await;
                    let syntax = if want_syntax {
                        Some(App::dump_of(&ws).await)
                    } else {
                        None
                    };
                    let markers = App::markers_of(&ws).await;
                    link.send_message(Msg::ActiveRefreshed {
                        path: ws.active().to_string(),
                        source: formatted,
                        syntax,
                    });
                    link.send_message(markers);
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
                // Register the language-feature providers, backed by the shared workspace.
                providers::Providers::install(Rc::clone(&workspace));
                let link = ctx.link().clone();
                spawn_local(async move {
                    let ws = workspace.lock().await;
                    // Eagerly create a URI-backed model for every file, so cross-file navigation
                    // and peek-references can reach files never opened in the editor.
                    let files = js_sys::Array::new();
                    for (path, text) in ws.file_texts() {
                        files.push(&js_sys::Array::of2(
                            &JsValue::from(path),
                            &JsValue::from(text),
                        ));
                    }
                    monaco::create_models(&files);
                    link.send_message(App::markers_of(&ws).await);
                });
                false
            }
            Msg::ModelOpened(path) => {
                // Monaco already switched the model (and flushed the outgoing file via `on_change`,
                // whose message — and therefore its lock turn — precedes this one); only track the
                // new active file and repaint. Must not flush or `switch_model` again. Cross-file
                // navigation only ever targets Java files, so a config is no longer open.
                self.active_config = None;
                self.active_path = path.clone();
                let Some(workspace) = self.workspace() else {
                    return true;
                };
                let want_syntax = self.syntax_dump.is_some();
                let link = ctx.link().clone();
                spawn_local(async move {
                    let mut ws = workspace.lock().await;
                    ws.set_active(&path);
                    let source = ws.active_source();
                    let syntax = if want_syntax {
                        Some(App::dump_of(&ws).await)
                    } else {
                        None
                    };
                    let markers = App::markers_of(&ws).await;
                    link.send_message(Msg::ActiveRefreshed {
                        path,
                        source,
                        syntax,
                    });
                    link.send_message(markers);
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
            Msg::ClasspathResolved(result) => {
                match result {
                    Ok((classpath, feature_set, status, artifacts)) => {
                        self.deps_status = Some(status);
                        if let Some(workspace) = self.workspace() {
                            let link = ctx.link().clone();
                            spawn_local(async move {
                                // All settle in the editor core: the classpath rebuilds the index,
                                // the feature set folds into every later diagnostics run, and the
                                // detached task's verified artifacts merge back.
                                let mut ws = workspace.lock().await;
                                ws.set_classpath(Some(classpath)).await;
                                ws.set_feature_set(feature_set);
                                ws.replace_artifacts(artifacts);
                                // Re-analyse with the external types now in the index;
                                // `MarkersComputed` drops the paint if a config model is showing.
                                link.send_message(App::markers_of(&ws).await);
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
