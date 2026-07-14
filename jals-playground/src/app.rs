//! The root [`App`] component: it owns all playground state and orchestrates the UI.
//!
//! `App` holds the in-memory [`Workspace`], the shared formatter [`Config`], the two editable
//! configuration buffers (`jals.toml` / `jalsfmt.toml`), and the current syntax-tree dump, and
//! wires the responsibility-split child components ([`Header`], [`FileTree`], [`EditorPane`],
//! [`SyntaxPane`]) together with props and callbacks. The configuration files are edited as TOML in
//! the editor itself — selecting one opens its buffer, editing `jalsfmt.toml` updates the formatter
//! [`Config`], and editing `jals.toml` re-resolves its `[dependencies]`. Editor *content* operations
//! (switching files, applying a format, repainting diagnostics) are driven imperatively against the
//! single Monaco instance through the [`crate::monaco`] service; the child components stay
//! presentational.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;

use jals_config::fmt::Config;
use jals_config::{Dependency, FeatureSet, ManifestParseError, Severity};
use jals_fs::InMemoryFileTree;
use jals_hir::{LoweredClasspath, ProjectIndex};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::components::{EditorPane, FileTree, Header, SyntaxPane, TreeEntry};
use crate::fetcher::BrowserFetcher;
use crate::line_index::LineIndex;
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
    /// The editor buffer changed (debounced; edits the active Java file or config buffer).
    EditorChanged(String),
    /// Switch the active file (clicked in the file tree).
    SelectFile(String),
    /// Format the active file in place.
    Format,
    /// Dump the active file's syntax tree into the right pane.
    Syntax,
    /// The editor exists: register the language-feature providers and paint the initial markers.
    EditorReady,
    /// A cross-file navigation switched the editor's model to `path`; track it as the active file.
    ModelOpened(String),
    /// The CORS proxy changed (typed in the header); stored for the next dependency resolve.
    SetProxy(String),
    /// The async dependency resolution finished: the lowered classpath + the resolved feature set
    /// (from `[package] features`) + a status line, or an error.
    ClasspathResolved(Result<(LoweredClasspath, FeatureSet, String), String>),
}

/// The playground's root component. Owns every piece of state; the children are presentational.
pub struct App {
    /// The in-memory multi-file workspace; the active file backs the editor. Shared behind an
    /// `Rc<RefCell<…>>` so the once-registered Monaco language-feature providers (registered in
    /// [`Msg::EditorReady`]) can analyse it without a second synced copy.
    workspace: Rc<RefCell<Workspace>>,
    /// The formatter configuration — parsed from the `jalsfmt.toml` buffer on edit. Shared behind an
    /// `Rc<RefCell<…>>` so the once-registered Monaco *Format Document* provider (created in
    /// [`EditorPane`]) reads the latest settings without a second synced copy.
    config: Rc<RefCell<Config>>,
    /// The most recent syntax-tree dump shown in the right pane, if any.
    syntax_dump: Option<String>,
    /// The in-memory dependency cache — the browser's equivalent of `target/jals/deps`. Downloaded
    /// jars are written here and reused across resolves (skip-if-exists), shared behind an
    /// `Rc<RefCell<…>>` so the async resolve task can snapshot and repopulate it without holding a
    /// borrow across an `.await`.
    deps_cache: Rc<RefCell<InMemoryFileTree>>,
    /// The last dependency-resolution status line shown in the [`Header`], if any.
    deps_status: Option<String>,
    /// The project's resolved language feature set from the last resolved `jals.toml`'s
    /// `[package] features`, threaded into the lint [`Config`] so the feature-gated rules
    /// (`compact-source-file`, `module-import`) fire in the browser. Empty until a manifest
    /// declaring features resolves.
    feature_set: FeatureSet,
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
    /// Recompute the active file's diagnostics and push them to Monaco as inline markers. The
    /// workspace already maps each range to Monaco's UTF-16 coordinates, so this only marshals
    /// through [`monaco::Marker::set_diagnostics`].
    fn refresh_markers(&self) {
        // Fold the resolved `[package] features` into the lint config so the feature-gated rules
        // (`compact-source-file`, `module-import`) fire; every other key stays at its default.
        let config = jals_config::lint::Config {
            features: self.feature_set,
            ..Default::default()
        };
        let diags = self.workspace.borrow().analyze_active(&config);
        monaco::Marker::set_diagnostics(diags.iter().map(|d| monaco::Marker {
            start_line: d.range.start_line,
            start_col: d.range.start_col,
            end_line: d.range.end_line,
            end_col: d.range.end_col,
            message: &d.message,
            severity: d.severity,
        }));
    }

    /// Flush Monaco's live buffer into whatever is currently active — a config buffer when a config
    /// file is open, else the active Java file's `fs` mirror. Monaco owns the live text (the mirror
    /// lags by the edit debounce), so any handler about to read the active source must flush first.
    fn flush_editor(&mut self) {
        self.commit_active_buffer(monaco::current_value());
    }

    /// Commit `value` (the live editor text) into whatever is currently active — a config buffer when
    /// a config file is open, else the active Java file's `fs` mirror. Shared by [`App::flush_editor`]
    /// (before a file switch) and [`Msg::EditorChanged`]. The manifest arm only stores the buffer;
    /// kicking off its dependency resolve is [`Msg::EditorChanged`]'s job (a flush must not resolve).
    fn commit_active_buffer(&mut self, value: String) {
        match self.active_config {
            Some(ConfigKind::Manifest) => self.manifest_src = value,
            Some(ConfigKind::Fmt) => {
                // Commit the latest formatter config, in case the last edit debounce has not fired
                // yet (cheap: no network, unlike the manifest resolve).
                self.apply_fmt(&value);
                self.fmt_src = value;
            }
            None => self.workspace.borrow_mut().edit_active(&value),
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
    /// the active Java file's path + source. Computed together for [`Component::view`].
    fn active_pane(&self) -> (String, String) {
        match self.active_config {
            Some(kind) => (kind.path().to_string(), self.config_src(kind).to_string()),
            None => {
                let ws = self.workspace.borrow();
                (ws.active().to_string(), ws.active_source())
            }
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
            let index = LineIndex::new(text);
            let (start_line, start_col, end_line, end_col) = index.to_monaco(text, &range);
            monaco::Marker {
                start_line,
                start_col,
                end_line,
                end_col,
                message: message.as_str(),
                severity: Severity::Error,
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
        self.last_resolve_key = Some((manifest.dependencies.clone(), self.proxy.clone()));
        self.deps_status = Some("resolving…".to_string());
        let cache = self.deps_cache.clone();
        let proxy = self.proxy.clone();
        let link = ctx.link().clone();
        spawn_local(async move {
            let result = Self::resolve_classpath(manifest, proxy, cache).await;
            link.send_message(Msg::ClasspathResolved(result));
        });
        true
    }

    /// Refresh the right pane's syntax-tree dump from the active file.
    fn dump_syntax(&mut self) {
        let parse = self.workspace.borrow().syntax_active();
        self.syntax_dump = Some(format!("{:#?}", parse.syntax()));
    }

    /// If the syntax pane is currently shown, re-dump it from the active file. Returns whether it
    /// was refreshed (i.e. whether a re-render is needed to show the new dump).
    fn refresh_syntax_if_shown(&mut self) -> bool {
        if self.syntax_dump.is_some() {
            self.dump_syntax();
            true
        } else {
            false
        }
    }

    /// Flatten the workspace's files into a pre-order [`TreeEntry`] list for the [`FileTree`].
    fn tree_entries(&self) -> Vec<TreeEntry> {
        let mut out = Vec::new();
        self.collect_entries("", 0, &mut out);
        out
    }

    /// Append the children of directory `dir` (each at indentation `depth`) to `out`, recursing
    /// into subdirectories so the whole tree is flattened in pre-order.
    fn collect_entries(&self, dir: &str, depth: usize, out: &mut Vec<TreeEntry>) {
        for path in self.workspace.borrow().read_dir(dir) {
            let name = jals_fs::path::VPath::file_name(&path)
                .map(str::to_string)
                .unwrap_or_else(|| path.clone());
            let is_dir = self.workspace.borrow().is_dir(&path);
            out.push(TreeEntry {
                path: path.clone(),
                name,
                depth,
                is_dir,
            });
            if is_dir {
                self.collect_entries(&path, depth + 1, out);
            }
        }
    }

    /// Assemble a parsed `manifest`'s analysis inputs into a lowered classpath, in the browser:
    /// download each remote `[dependencies]` jar with a [`BrowserFetcher`] into the in-memory `cache`,
    /// load the `.class` files, and lower them for the project index. Returns the classpath, the
    /// resolved feature set from `[package] features` (for the feature-gated lint rules), and a
    /// human-readable status line (class/jar counts plus any warnings), or an error message.
    ///
    /// The whole resolution runs against a *snapshot clone* of `cache` so no `RefCell` borrow is held
    /// across an `.await`; the populated snapshot is written back afterwards, so a re-resolve reuses
    /// the already-downloaded jars (skip-if-exists) — the browser's `target/jals/deps`.
    async fn resolve_classpath(
        manifest: jals_config::Manifest,
        proxy: String,
        cache: Rc<RefCell<InMemoryFileTree>>,
    ) -> Result<(LoweredClasspath, FeatureSet, String), String> {
        let fetcher = BrowserFetcher::new(proxy);
        let mut snapshot = cache.borrow().clone();
        let mut warnings = Vec::new();
        // A synthetic `/` root: remote (`https://`) jars ignore it; local `file://`/`path` jars are
        // not reachable in the browser anyway. No `Git` capability (browser), and analysis-only
        // options — jars → classpath; sources jars, `git`/`path` source deps, and skeletons are
        // host-only features.
        let inputs = jals_classpath::ProjectInputsIn::assemble_project_inputs_in(
            &fetcher,
            None,
            &mut snapshot,
            &manifest,
            "/",
            jals_classpath::ProjectInputOptions::Analysis,
            |message| warnings.push(message),
        )
        .await;
        *cache.borrow_mut() = snapshot;
        let classpath = ProjectIndex::lower_classpath(&inputs.classpath_classes);
        let mut status = format!(
            "resolved {} class(es) from {} jar(s)",
            inputs.classpath_classes.len(),
            inputs.dependency_jars.len()
        );
        if !warnings.is_empty() {
            status.push_str(&format!(
                " — {} warning(s): {}",
                warnings.len(),
                warnings.join("; ")
            ));
        }
        Ok((classpath, inputs.feature_set, status))
    }
}

impl Component for App {
    type Message = Msg;
    type Properties = ();

    fn create(_ctx: &Context<Self>) -> Self {
        App {
            workspace: Rc::new(RefCell::new(Workspace::new())),
            config: Rc::new(RefCell::new(Config::default())),
            syntax_dump: None,
            deps_cache: Rc::new(RefCell::new(InMemoryFileTree::new())),
            deps_status: None,
            feature_set: FeatureSet::default(),
            manifest_src: ConfigKind::Manifest.seed().to_string(),
            fmt_src: ConfigKind::Fmt.seed().to_string(),
            active_config: None,
            proxy: String::new(),
            last_resolve_key: None,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            // Route the edit by what is open: a config buffer parses into its effect (formatter
            // config / dependency resolve) and repaints config markers; a Java file syncs the
            // workspace and repaints Java markers. All imperative — Monaco owns the live text.
            Msg::EditorChanged(value) => match self.active_config {
                // A manifest edit resolves its `[dependencies]` and stores the buffer; re-render only
                // when a resolve actually started (the header shows "resolving…").
                Some(ConfigKind::Manifest) => {
                    let rerender = self.apply_manifest(ctx, &value);
                    self.manifest_src = value;
                    rerender
                }
                // A Java file or the formatter config: `commit_active_buffer` applies the edit (the
                // Fmt arm reparses into the shared `Config`); only a Java file repaints its markers.
                other => {
                    let is_java = other.is_none();
                    self.commit_active_buffer(value);
                    if is_java {
                        self.refresh_markers();
                    }
                    false
                }
            },
            Msg::SelectFile(path) => {
                // Flush the live editor text into the (still-active) file/buffer before switching.
                self.flush_editor();
                if let Some(kind) = ConfigKind::from_path(&path) {
                    self.active_config = Some(kind);
                    let src = self.config_src(kind).to_string();
                    monaco::switch_model(&path, &src);
                    // Show this config's current parse state (selecting never triggers a resolve).
                    self.set_config_diagnostic(&src, kind.parse_error(&src));
                } else {
                    self.active_config = None;
                    self.workspace.borrow_mut().set_active(&path);
                    let src = self.workspace.borrow().active_source();
                    monaco::switch_model(&path, &src);
                    self.refresh_syntax_if_shown();
                    self.refresh_markers();
                }
                true
            }
            // Format and Syntax are Java-only; a config file is plain TOML, so ignore them there.
            Msg::Format => {
                if self.active_config.is_some() {
                    return false;
                }
                self.flush_editor();
                let formatted = self
                    .workspace
                    .borrow()
                    .format_active(&self.config.borrow())
                    .formatted;
                monaco::update_model(&formatted);
                self.workspace.borrow_mut().edit_active(&formatted);
                let rerender = self.refresh_syntax_if_shown();
                self.refresh_markers();
                rerender
            }
            Msg::Syntax => {
                if self.active_config.is_some() {
                    return false;
                }
                self.flush_editor();
                self.dump_syntax();
                true
            }
            Msg::EditorReady => {
                // Eagerly create a URI-backed model for every file, so cross-file navigation and
                // peek-references can reach files never opened in the editor.
                let files = js_sys::Array::new();
                for (path, text) in self.workspace.borrow().file_texts() {
                    files.push(&js_sys::Array::of2(
                        &JsValue::from(path),
                        &JsValue::from(text),
                    ));
                }
                monaco::create_models(&files);
                // Register the language-feature providers, backed by the shared workspace.
                providers::Providers::install(self.workspace.clone());
                self.refresh_markers();
                false
            }
            Msg::ModelOpened(path) => {
                // Monaco already switched the model (and flushed the outgoing file via `on_change`);
                // only track the new active file and repaint. Must not flush or `switch_model` again.
                // Cross-file navigation only ever targets Java files, so a config is no longer open.
                self.active_config = None;
                self.workspace.borrow_mut().set_active(&path);
                self.refresh_syntax_if_shown();
                self.refresh_markers();
                true
            }
            Msg::SetProxy(proxy) => {
                // The input is uncontrolled — just record the value for the next resolve; no re-render.
                self.proxy = proxy;
                false
            }
            Msg::ClasspathResolved(result) => {
                match result {
                    Ok((classpath, feature_set, status)) => {
                        self.workspace.borrow_mut().set_classpath(Some(classpath));
                        self.feature_set = feature_set;
                        self.deps_status = Some(status);
                        // Re-analyse the active file with the external types now in the index — but
                        // only when a Java file is showing, so we never paint Java markers on a
                        // config model.
                        if self.active_config.is_none() {
                            self.refresh_markers();
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
        // still, keep it consistent with the active pane in case the editor ever re-mounts.
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
                        entries={self.tree_entries()}
                        active={active_path.clone()}
                        on_select={link.callback(Msg::SelectFile)}
                    />
                    <main class="grid min-h-0 flex-1 grid-cols-1 md:grid-cols-2">
                        <EditorPane
                            path={active_path}
                            source={source}
                            on_change={link.callback(Msg::EditorChanged)}
                            on_ready={link.callback(|_| Msg::EditorReady)}
                            on_open={link.callback(Msg::ModelOpened)}
                            config={self.config.clone()}
                        />
                        <SyntaxPane dump={self.syntax_dump.clone()} />
                    </main>
                </div>
            </div>
        }
    }
}
