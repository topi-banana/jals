//! The canonical, protocol-neutral diagnostics assembly for one project.
//!
//! [`ProjectAssembly`](crate::ProjectAssembly) owns the *order* the project-assembly procedure runs
//! in. This owns the order and shape of what that procedure has to *say*.
//!
//! Every host used to sequence its own. The language server re-derived a severity per call site,
//! re-wrapped graph warnings into synthetic classpath ones to borrow an anchor, and never showed a
//! classpath input warning to a client at all. The CLI printed one `warning:` line per channel and
//! resolved no position for a failing script, so a broken `build.rhai` reported no location. The
//! browser folded a failed phase into one joined string. The three had diverged observably. The one
//! policy lives here now; hosts only map each [`ProjectDiagnostic`] to their protocol's shape — an
//! LSP `Diagnostic`, a Monaco marker, an `ariadne` report — exactly as they already do for
//! `jals_editor::FileDiagnostic`, the file-scoped assembly this mirrors.
//!
//! The policy, in order:
//!
//! 1. **The script phase.** [`Skipped`](ScriptOutcome::Skipped) reports nothing: `jals lint`
//!    analyses a folder without executing an unreviewed `build.rhai`, and declining to run one is
//!    not a diagnostic. [`Ran`](ScriptOutcome::Ran) promotes `BuildScriptOutput::diagnostics` to
//!    warnings with no severity test — a run that produced an error diverted the whole collection
//!    into `BuildScriptError::ReportedErrors` before an output existed, so the collection is
//!    warnings-only by construction. [`Failed`](ScriptOutcome::Failed) reports the failure, and
//!    `ReportedErrors` fans out into one diagnostic per carried diagnostic, in emission order and
//!    each under its own severity: a warning reported before the fatal one is context for it.
//! 2. **The graph phase.** [`NotReached`](GraphOutcome::NotReached) reports nothing — whatever
//!    stopped it has already said so. [`Failed`](GraphOutcome::Failed) reports the warnings the
//!    earlier phases produced *and then* the failure, because the dependency discovery warned was
//!    unavailable is usually the one preprocessing then failed on; that pairing is the entire
//!    reason [`GraphResolveError`](crate::GraphResolveError) carries warnings at all.
//!    [`Resolved`](GraphOutcome::Resolved) reports all three channels a [`ProjectReport`] carries,
//!    including the classpath input warnings a host reading `warnings` alone used to drop.
//! 3. **The offline advisory.** When a resolved graph's messages report a refusal by an offline
//!    fetch capability, one [`DependencyCache`](ProjectDiagnosticCode::DependencyCache)
//!    informational diagnostic states that condition — and only the condition. The sentence that
//!    clears it is [`ProjectDiagnosticCode::remedy`], beside the code rather than inside the
//!    message, so a channel with somewhere to put a follow-on line puts it there and one without
//!    appends it. Whether a host offers it at all is still the host's — a browser tab has no
//!    `jals build` to name.
//!
//! **Severity** is decided once, here.
//!
//! **Codes** are an enum rather than strings, so a host attaching a remedy to one writes a
//! compiler-checked arm instead of a string comparison repeated in three places.
//!
//! **Origin.** Every diagnostic names the project file it anchors to — the root manifest or the
//! configured build script — and carries a byte [`span`](ProjectDiagnostic::span) within it when
//! one is known. A host converts that span into its own coordinates and does nothing else.
//! Resolving Rhai's 1-based, character-counted position is
//! [`BuildScriptPosition::byte_range`](jals_build::build_script::BuildScriptPosition::byte_range),
//! and running it here is why no `ProjectDiagnostic` names a `BuildScriptPosition`. (A
//! [`ScriptOutcome`] does, transitively, through the error it borrows — the rule is about the value
//! a host maps, not about the input this assembly reads.)
//!
//! **Placement.** Most of what this reports has no span — a dependency failure has no location in
//! the consumer's tree — so a channel that must name one has to invent a place to put it.
//! [`ProjectDiagnostic::placement_in`] is that place: the span when there is one, the first line of
//! the anchor's own text otherwise. Two hosts used to answer it differently, and one of the two
//! answers left the `\r` of a CRLF line inside the range. A channel that *can* say "no location" —
//! a terminal line — reads [`span`](ProjectDiagnostic::span) instead and never asks. Which document
//! a diagnostic goes *to* is still the host's: routing an anchor to a URI, a Monaco model, or an
//! `ariadne` source is its own map, and picking the text through that map is what satisfies
//! `placement_in`'s precondition without a second test beside it.
//!
//! **Rendering.** [`GraphWarning`], [`jals_classpath::Warning`],
//! [`ProjectAssemblyError`](crate::ProjectAssemblyError), and [`GraphError`] render through their
//! whole `Display`: each carries its subject in an attribution the message does not repeat, and
//! every one of those types documents that rule. A `BuildScriptDiagnostic` renders through
//! `message()` alone, because its severity travels in [`ProjectDiagnostic::severity`] instead. A
//! host that anchored a graph warning by re-wrapping it in another warning type no longer needs to:
//! the anchor is structural.
//!
//! **Order.** Stably sorted by [`anchor`](ProjectDiagnostic::anchor), so a host publishing per file
//! gets contiguous groups. Within an anchor the production order above is preserved verbatim — it
//! is causal, and every producer's own order is already deterministic. Deliberately *not* the
//! file-scoped assembly's `(range.start, code)`: almost nothing here has a span to sort by, and
//! sorting by code would scramble the emission order `ReportedErrors` documents as load-bearing.

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

use jals_build::build_script::{BuildScriptError, BuildScriptOutput, BuildScriptPosition};
use jals_classpath::NetworkPolicy;
use jals_storage::FileKey;

use crate::assemble::ProjectAssemblyError;
use crate::assembly::GraphResolveError;
use crate::graph::{GraphError, GraphWarning};
use crate::task::RootBuildScriptError;

/// How a host should present a [`ProjectDiagnostic`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ProjectDiagnosticSeverity {
    /// The project could not be assembled as it is declared.
    Error,
    /// Assembly continued, but something the project declares did not take effect.
    Warning,
    /// Advisory. Not a defect in the project — today only the offline-cache condition.
    Info,
}

impl ProjectDiagnosticSeverity {
    /// The word a plain-text channel leads with — a terminal line, a browser status line, a
    /// server's stderr.
    ///
    /// Here rather than in each host for the same reason the rest of this module is: a channel that
    /// carries no severity of its own has to spell one, and three hosts spelling it themselves is
    /// the drift this assembly exists to remove. A destination with a severity field of its own
    /// maps [`ProjectDiagnostic::severity`] into it instead and never reads this.
    pub const fn lead(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            // `note`, not `info`: it matches what the CLI already leads a range-less advisory with.
            Self::Info => "note",
        }
    }
}

/// The project file a [`ProjectDiagnostic`] is reported against.
///
/// Never a dependency's own file. A dependency names itself in the message, by the location its
/// consumer's manifest reaches it through; graph node metadata exposes nothing a consumer's host
/// could open, and a node identity renders as a digest. The manifest is where a reader goes to act
/// on any of it.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ProjectAnchor {
    /// The project's root manifest.
    Manifest,
    /// The configured build script, by its project key.
    Script(FileKey),
}

/// The stable code vocabulary of the project-assembly procedure.
///
/// An enum rather than a string because a host attaching a remedy to one — the language server's
/// advice for [`DependencyCache`](Self::DependencyCache) — should write an arm the compiler checks
/// against this list, not a comparison that goes quietly stale.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ProjectDiagnosticCode {
    /// The root manifest declares something the procedure cannot use.
    ProjectManifest,
    /// Project storage could not be read or written.
    ProjectStorage,
    /// The procedure did not complete, for a reason it could not report as a value — a host
    /// observing a panic or a cancelled run. Never produced by
    /// [`assemble`](ProjectDiagnostics::assemble); it is here so a host reporting one spells it
    /// from the same vocabulary as everything else it publishes.
    ProjectAssembly,
    /// The root build script reported, or failed.
    BuildScript,
    /// Non-fatal graph discovery or preprocessing.
    DependencyResolution,
    /// A dependency node could not be projected into classpath inputs.
    DependencyAssembly,
    /// A classpath input could not be resolved, loaded, or generated.
    ClasspathInput,
    /// Dependencies are declared but not in the verified cache. Advisory; the sentence that clears
    /// it is [`remedy`](Self::remedy), and whether a host can offer it is the host's.
    DependencyCache,
    /// A `[dependencies]` entry is not usable as declared.
    DependencyInvalid,
    /// A dependency's own manifest is malformed.
    DependencyManifest,
    /// The dependency graph contains a cycle.
    DependencyCycle,
    /// A dependency's build script failed.
    DependencyBuildScript,
    /// A dependency's sources could not be acquired.
    DependencyAcquisition,
}

impl ProjectDiagnosticCode {
    /// The wire spelling, for a protocol that carries a code as a string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProjectManifest => "project-manifest",
            Self::ProjectStorage => "project-storage",
            Self::ProjectAssembly => "project-assembly",
            Self::BuildScript => "build-script",
            Self::DependencyResolution => "dependency-resolution",
            Self::DependencyAssembly => "dependency-assembly",
            Self::ClasspathInput => "classpath-input",
            Self::DependencyCache => "dependency-cache",
            Self::DependencyInvalid => "dependency-invalid",
            Self::DependencyManifest => "dependency-manifest",
            Self::DependencyCycle => "dependency-cycle",
            Self::DependencyBuildScript => "dependency-build-script",
            Self::DependencyAcquisition => "dependency-acquisition",
        }
    }

    /// What a host with a command line can tell a reader to run about this condition.
    ///
    /// Here rather than in each host for the same reason [`ProjectDiagnosticSeverity::lead`] is:
    /// two hosts spelling one sentence is how the two sentences drift, and which code carries one
    /// becomes an arm the compiler checks against this list rather than a comparison repeated in
    /// three places. *Offering* the sentence stays the host's, as does saying why that particular
    /// host is not running the command itself.
    ///
    /// Exhaustive on purpose, so a new code has to say whether a command clears it rather than
    /// inherit whichever answer a wildcard happened to give.
    pub const fn remedy(self) -> Option<&'static str> {
        match self {
            // A build is what populates the verified cache, whichever host says so: `jals lint`
            // never fetches, and a `jals build --offline` that hit this wants the run without the
            // flag.
            Self::DependencyCache => Some("run `jals build` to fetch them"),
            // Everything else is cleared by editing the project. There is no command to name.
            Self::ProjectManifest
            | Self::ProjectStorage
            | Self::ProjectAssembly
            | Self::BuildScript
            | Self::DependencyResolution
            | Self::DependencyAssembly
            | Self::ClasspathInput
            | Self::DependencyInvalid
            | Self::DependencyManifest
            | Self::DependencyCycle
            | Self::DependencyBuildScript
            | Self::DependencyAcquisition => None,
        }
    }

    /// The code for one structured graph failure.
    const fn of_graph_error(error: &GraphError) -> Self {
        match error {
            GraphError::InvalidRootManifest { .. } => Self::ProjectManifest,
            GraphError::InvalidDependency { .. } => Self::DependencyInvalid,
            GraphError::MalformedManifest { .. } => Self::DependencyManifest,
            GraphError::Cycle { .. } => Self::DependencyCycle,
            GraphError::BuildScript { .. } => Self::DependencyBuildScript,
            GraphError::Acquisition { .. } => Self::DependencyAcquisition,
        }
    }
}

impl fmt::Display for ProjectDiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One diagnostic about one project, anchored to a project file in byte coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectDiagnostic {
    /// The project file this is about.
    pub anchor: ProjectAnchor,
    /// The byte range within that file, when one is known.
    ///
    /// `None` for everything the procedure reports about a *declaration* rather than a *location* —
    /// which is most of it, since a dependency failure has no span in the consumer's tree. A host
    /// places those wherever it puts a file-level diagnostic.
    pub span: Option<Range<usize>>,
    /// How to present it.
    pub severity: ProjectDiagnosticSeverity,
    /// The producing part of the procedure.
    pub code: ProjectDiagnosticCode,
    /// The condition. No remedy and no severity restated — a host adds the first from
    /// [`ProjectDiagnosticCode::remedy`] and the second from [`severity`](Self::severity), each in
    /// whatever shape its own channel has for one.
    pub message: String,
}

impl ProjectDiagnostic {
    /// Where this goes on a channel that cannot express "no location": the [`span`](Self::span)
    /// when the procedure resolved one, and the first line of `text` otherwise.
    ///
    /// `text` must be the text of this diagnostic's own [`anchor`](Self::anchor) — the same
    /// precondition `ScriptFile` guards on the way *in*, since resolving against the wrong file's
    /// text is a silently wrong answer rather than a missing one. Taking the text as an argument
    /// rather than reaching for it is what lets a host that holds no text at all decline this and
    /// keep its own channel's answer.
    pub fn placement_in(&self, text: &str) -> Range<usize> {
        /// The first line of `text`, with the terminator excluded and the `\r` of a `\r\n` with
        /// it: a range ending on the `\r` highlights a character the reader cannot see. An empty
        /// text gives `0..0`, which every channel renders as a caret at the head of the file.
        fn first_line(text: &str) -> Range<usize> {
            let end = text.find('\n').unwrap_or(text.len());
            0..text[..end].trim_end_matches('\r').len()
        }

        self.span.clone().unwrap_or_else(|| first_line(text))
    }
}

/// The configured build script, as a host holds it.
#[derive(Clone, Copy, Debug)]
pub struct ScriptFile<'a> {
    /// The script's project key, as `[build] script` spells it.
    pub key: &'a FileKey,
    /// The script's current text.
    ///
    /// `None` costs only the byte span of a positioned failure — the diagnostic is still reported,
    /// still anchored to the script, and still says the same thing. A host with no cheap read of
    /// the script passes it.
    pub text: Option<&'a str>,
}

impl ScriptFile<'_> {
    /// The span a position addresses, when this is the file the position was reported against.
    ///
    /// The guard matters: `key` is what the *manifest* configured and the error names what the
    /// *run* compiled. They agree today, and resolving a range in the wrong file's text would be a
    /// silently wrong answer rather than a compile failure if they ever stopped.
    fn span_in(
        self,
        script: &FileKey,
        position: Option<BuildScriptPosition>,
    ) -> Option<Range<usize>> {
        if self.key != script {
            return None;
        }
        position?.byte_range(self.text?)
    }
}

/// Everything one resolved assembly reported, borrowed from it.
///
/// The three channels together, and only together. `warnings` and `errors` are the graph's;
/// `inputs.warnings` is the classpath's, and a host that reached for the first two alone silently
/// dropped the third — which is how an unreadable jar became something only a server's stderr ever
/// mentioned. The fields are private and the only constructors are
/// [`MemoryProjectAssembly::report`](crate::MemoryProjectAssembly::report) and its native sibling,
/// so a [`GraphOutcome::Resolved`] cannot be built with a channel left out.
#[derive(Clone, Copy, Debug)]
pub struct ProjectReport<'a> {
    warnings: &'a [GraphWarning],
    errors: &'a [ProjectAssemblyError],
    inputs: &'a [jals_classpath::Warning],
}

impl<'a> ProjectReport<'a> {
    pub(crate) const fn new(
        warnings: &'a [GraphWarning],
        errors: &'a [ProjectAssemblyError],
        inputs: &'a [jals_classpath::Warning],
    ) -> Self {
        Self {
            warnings,
            errors,
            inputs,
        }
    }
}

/// What became of the script phase.
#[derive(Clone, Copy, Debug)]
pub enum ScriptOutcome<'a> {
    /// The script ran. Its diagnostics are warnings by construction.
    Ran(&'a BuildScriptOutput),
    /// The script phase failed.
    Failed(&'a RootBuildScriptError),
    /// No script ran — the manifest declares none, a host deliberately runs none, or this call
    /// reports only the graph phase because another call reports the script.
    Skipped,
}

/// What became of the graph phase.
#[derive(Clone, Copy, Debug)]
pub enum GraphOutcome<'a> {
    /// The graph resolved. Carries every channel it reported.
    Resolved(ProjectReport<'a>),
    /// The graph phase failed, carrying the warnings the phases before it produced.
    Failed(&'a GraphResolveError),
    /// The phase never started — the script phase failed, or this call reports only the script
    /// phase because another call reports the graph.
    NotReached,
}

/// Assembles the canonical diagnostics for one run of the project-assembly procedure.
pub struct ProjectDiagnostics;

impl ProjectDiagnostics {
    /// Assemble what one run of the procedure has to say.
    ///
    /// Both phases in one call, because a host that reports them separately has to remember not to
    /// report the script phase twice — which is exactly what a re-run graph phase over a reduced
    /// manifest would otherwise do. A host that genuinely splits the phases across two call sites,
    /// as the browser does when it releases its workspace lock between them, passes
    /// [`GraphOutcome::NotReached`] at the first and [`ScriptOutcome::Skipped`] at the second.
    pub fn assemble(
        script: ScriptOutcome<'_>,
        graph: GraphOutcome<'_>,
        script_file: Option<ScriptFile<'_>>,
    ) -> Vec<ProjectDiagnostic> {
        let mut out = Vec::new();
        Self::script_phase(&mut out, script, script_file);
        let graph_start = out.len();
        Self::graph_phase(&mut out, graph);

        // Advisory last, and only about what the graph phase just said. A refusal reads the same in
        // a graph warning and a classpath one, so the scan runs over both rather than over the type
        // that happens to produce most of them.
        if matches!(graph, GraphOutcome::Resolved(_))
            && out[graph_start..]
                .iter()
                .any(|diagnostic| NetworkPolicy::refused_offline(&diagnostic.message))
        {
            out.push(ProjectDiagnostic {
                anchor: ProjectAnchor::Manifest,
                span: None,
                severity: ProjectDiagnosticSeverity::Info,
                code: ProjectDiagnosticCode::DependencyCache,
                // The condition only. This crate knows neither which host it is under nor that
                // `jals build` is something a browser could not run.
                message: "some dependencies are not in the verified cache".to_owned(),
            });
        }

        // Stable, so the production order survives inside each group.
        out.sort_by(|a, b| a.anchor.cmp(&b.anchor));
        out
    }

    /// Whether the project could not be assembled as it is declared — the gate a host that has to
    /// stop applies before it goes on.
    ///
    /// Here rather than in each host because "could not be assembled" is a statement about
    /// [`ProjectDiagnosticSeverity::Error`], and a host that spells the test itself is a host that
    /// will still be spelling the old one when this vocabulary grows. A host that keeps working
    /// over a broken project reads this and does nothing with it: a language server whose whole
    /// value is being useful *while* the project is wrong publishes the errors and loads anyway,
    /// which is a decision about that host and not about the diagnostics.
    pub fn has_errors(diagnostics: &[ProjectDiagnostic]) -> bool {
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == ProjectDiagnosticSeverity::Error)
    }

    /// The anchor a script diagnostic takes when it has nothing narrower to point at: the script
    /// when one is configured, the manifest otherwise.
    fn script_anchor(file: Option<ScriptFile<'_>>) -> ProjectAnchor {
        file.map_or(ProjectAnchor::Manifest, |file| {
            ProjectAnchor::Script(file.key.clone())
        })
    }

    fn script_phase(
        out: &mut Vec<ProjectDiagnostic>,
        script: ScriptOutcome<'_>,
        file: Option<ScriptFile<'_>>,
    ) {
        match script {
            ScriptOutcome::Skipped => {}
            ScriptOutcome::Ran(output) => {
                // No severity test: an error diverts the whole collection into `ReportedErrors`
                // before an output exists, so writing one here would describe a state that cannot
                // arise while erasing the one that would.
                for diagnostic in &output.diagnostics {
                    out.push(ProjectDiagnostic {
                        anchor: Self::script_anchor(file),
                        span: None,
                        severity: ProjectDiagnosticSeverity::Warning,
                        code: ProjectDiagnosticCode::BuildScript,
                        message: diagnostic.message().to_owned(),
                    });
                }
            }
            ScriptOutcome::Failed(RootBuildScriptError::BuildScript(error)) => {
                Self::script_failure(out, error, file);
            }
            ScriptOutcome::Failed(error @ RootBuildScriptError::Task(_)) => out.push(Self::error(
                Self::script_anchor(file),
                ProjectDiagnosticCode::BuildScript,
                error,
            )),
            ScriptOutcome::Failed(error @ RootBuildScriptError::Storage(_)) => {
                out.push(Self::error(
                    Self::script_anchor(file),
                    ProjectDiagnosticCode::ProjectStorage,
                    error,
                ));
            }
            // `[build] source-dirs` content, so the manifest is what a reader goes to fix.
            ScriptOutcome::Failed(error @ RootBuildScriptError::InvalidSourceRoot(_)) => {
                out.push(Self::error(
                    ProjectAnchor::Manifest,
                    ProjectDiagnosticCode::ProjectManifest,
                    error,
                ));
            }
        }
    }

    fn script_failure(
        out: &mut Vec<ProjectDiagnostic>,
        error: &BuildScriptError,
        file: Option<ScriptFile<'_>>,
    ) {
        match error {
            // Every diagnostic the run produced, in emission order and each under its own severity.
            // The warnings reported before the fatal one are its context, and this is their only
            // carrier once publication is refused.
            BuildScriptError::ReportedErrors(diagnostics) => {
                for diagnostic in diagnostics {
                    out.push(ProjectDiagnostic {
                        anchor: Self::script_anchor(file),
                        span: None,
                        severity: if diagnostic.is_error() {
                            ProjectDiagnosticSeverity::Error
                        } else {
                            ProjectDiagnosticSeverity::Warning
                        },
                        code: ProjectDiagnosticCode::BuildScript,
                        message: diagnostic.message().to_owned(),
                    });
                }
            }
            // A complaint about `[build] script`, not about the script — which the host could not
            // anchor to anyway, since the path it names is not a usable key.
            BuildScriptError::InvalidScriptPath { .. } => out.push(Self::error(
                ProjectAnchor::Manifest,
                ProjectDiagnosticCode::ProjectManifest,
                error,
            )),
            BuildScriptError::ScriptTooLarge { script, .. } => out.push(Self::error(
                ProjectAnchor::Script(script.clone()),
                ProjectDiagnosticCode::BuildScript,
                error,
            )),
            BuildScriptError::Compile {
                script, position, ..
            }
            | BuildScriptError::Execute {
                script, position, ..
            } => out.push(ProjectDiagnostic {
                anchor: ProjectAnchor::Script(script.clone()),
                span: file.and_then(|file| file.span_in(script, *position)),
                severity: ProjectDiagnosticSeverity::Error,
                code: ProjectDiagnosticCode::BuildScript,
                message: error.to_string(),
            }),
            BuildScriptError::Storage { .. } => out.push(Self::error(
                Self::script_anchor(file),
                ProjectDiagnosticCode::ProjectStorage,
                error,
            )),
            BuildScriptError::InvalidLimit(_) | BuildScriptError::EnvironmentLimit(_) => {
                out.push(Self::error(
                    Self::script_anchor(file),
                    ProjectDiagnosticCode::BuildScript,
                    error,
                ));
            }
        }
    }

    fn graph_phase(out: &mut Vec<ProjectDiagnostic>, graph: GraphOutcome<'_>) {
        match graph {
            GraphOutcome::NotReached => {}
            GraphOutcome::Failed(failure) => {
                // Warnings first: the dependency discovery warned was unavailable is usually the
                // one preprocessing then failed on, and reading the failure without them is reading
                // half of it.
                for warning in &failure.warnings {
                    out.push(Self::warning(
                        ProjectAnchor::Manifest,
                        ProjectDiagnosticCode::DependencyResolution,
                        warning,
                    ));
                }
                out.push(Self::error(
                    ProjectAnchor::Manifest,
                    ProjectDiagnosticCode::of_graph_error(&failure.error),
                    &failure.error,
                ));
            }
            GraphOutcome::Resolved(report) => {
                for warning in report.warnings {
                    out.push(Self::warning(
                        ProjectAnchor::Manifest,
                        ProjectDiagnosticCode::DependencyResolution,
                        warning,
                    ));
                }
                for error in report.errors {
                    out.push(Self::error(
                        ProjectAnchor::Manifest,
                        ProjectDiagnosticCode::DependencyAssembly,
                        error,
                    ));
                }
                for warning in report.inputs {
                    out.push(Self::warning(
                        ProjectAnchor::Manifest,
                        ProjectDiagnosticCode::ClasspathInput,
                        warning,
                    ));
                }
            }
        }
    }

    /// A warning rendered through its producer's whole `Display`.
    ///
    /// Every type this is used on carries its subject in an attribution the message does not
    /// repeat — a graph warning's node, a classpath warning's origin — so rendering the message
    /// alone drops the half a user can act on.
    fn warning(
        anchor: ProjectAnchor,
        code: ProjectDiagnosticCode,
        subject: &dyn fmt::Display,
    ) -> ProjectDiagnostic {
        ProjectDiagnostic {
            anchor,
            span: None,
            severity: ProjectDiagnosticSeverity::Warning,
            code,
            message: subject.to_string(),
        }
    }

    /// [`warning`](Self::warning) at error severity — the same whole-`Display` rendering.
    fn error(
        anchor: ProjectAnchor,
        code: ProjectDiagnosticCode,
        subject: &dyn fmt::Display,
    ) -> ProjectDiagnostic {
        ProjectDiagnostic {
            severity: ProjectDiagnosticSeverity::Error,
            ..Self::warning(anchor, code, subject)
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use jals_build::build_script::BuildScriptDiagnostic;
    use jals_classpath::{Warning, WarningOrigin};

    use super::*;
    use crate::graph::CycleEdge;

    /// Every code, so a test about *all* of them cannot silently stop covering one. Two tests read
    /// it, and a new variant fails to compile until it is listed.
    const EVERY_CODE: [ProjectDiagnosticCode; 13] = [
        ProjectDiagnosticCode::ProjectManifest,
        ProjectDiagnosticCode::ProjectStorage,
        ProjectDiagnosticCode::ProjectAssembly,
        ProjectDiagnosticCode::BuildScript,
        ProjectDiagnosticCode::DependencyResolution,
        ProjectDiagnosticCode::DependencyAssembly,
        ProjectDiagnosticCode::ClasspathInput,
        ProjectDiagnosticCode::DependencyCache,
        ProjectDiagnosticCode::DependencyInvalid,
        ProjectDiagnosticCode::DependencyManifest,
        ProjectDiagnosticCode::DependencyCycle,
        ProjectDiagnosticCode::DependencyBuildScript,
        ProjectDiagnosticCode::DependencyAcquisition,
    ];

    fn key(path: &str) -> FileKey {
        FileKey::parse(path).expect("test path is a portable file key")
    }

    /// A diagnostic carrying `span` and `severity`, with the rest of the shape irrelevant to both
    /// the placement rule and the error gate.
    fn shaped(
        span: Option<Range<usize>>,
        severity: ProjectDiagnosticSeverity,
    ) -> ProjectDiagnostic {
        ProjectDiagnostic {
            anchor: ProjectAnchor::Manifest,
            span,
            severity,
            code: ProjectDiagnosticCode::DependencyResolution,
            message: String::new(),
        }
    }

    /// The same, at the severity placement does not care about.
    fn placed(span: Option<Range<usize>>) -> ProjectDiagnostic {
        shaped(span, ProjectDiagnosticSeverity::Warning)
    }

    /// The assembly under one outcome pair, with no script file configured.
    fn assemble(script: ScriptOutcome<'_>, graph: GraphOutcome<'_>) -> Vec<ProjectDiagnostic> {
        ProjectDiagnostics::assemble(script, graph, None)
    }

    fn codes(diagnostics: &[ProjectDiagnostic]) -> Vec<ProjectDiagnosticCode> {
        diagnostics.iter().map(|d| d.code).collect()
    }

    fn severities(diagnostics: &[ProjectDiagnostic]) -> Vec<ProjectDiagnosticSeverity> {
        diagnostics.iter().map(|d| d.severity).collect()
    }

    fn messages(diagnostics: &[ProjectDiagnostic]) -> Vec<&str> {
        diagnostics.iter().map(|d| d.message.as_str()).collect()
    }

    #[test]
    fn a_run_that_did_neither_phase_reports_nothing() {
        // `jals lint` opens a folder without executing an unreviewed script. Declining to run one
        // is not a diagnostic, and neither is a graph phase that was never asked for.
        assert!(assemble(ScriptOutcome::Skipped, GraphOutcome::NotReached).is_empty());
    }

    #[test]
    fn reported_errors_keep_their_own_severity_in_emission_order() {
        // The whole point of the fan-out: a `build.warning` before the fatal `build.error` is
        // context for it, and flattening both into one error message loses which was which.
        let error = RootBuildScriptError::BuildScript(BuildScriptError::ReportedErrors(vec![
            BuildScriptDiagnostic::warning("generated sources are stale"),
            BuildScriptDiagnostic::error("no toolchain"),
        ]));
        let out = assemble(ScriptOutcome::Failed(&error), GraphOutcome::NotReached);

        assert_eq!(
            messages(&out),
            ["generated sources are stale", "no toolchain"]
        );
        assert_eq!(
            severities(&out),
            [
                ProjectDiagnosticSeverity::Warning,
                ProjectDiagnosticSeverity::Error
            ]
        );
        // Bare: the severity travels in the diagnostic, so spelling it in the text too would say it
        // twice.
        assert!(out.iter().all(|d| !d.message.contains("warning:")));
        assert_eq!(codes(&out), [ProjectDiagnosticCode::BuildScript; 2]);
    }

    #[test]
    fn a_manifest_complaint_anchors_to_the_manifest_even_from_the_script_phase() {
        // `[build] script` and `[build] source-dirs` are manifest content. Anchoring either to the
        // script is anchoring it to a file the reader cannot fix it in — and for an unusable path,
        // to a file that has no key at all.
        let script = key("build.rhai");
        let file = ScriptFile {
            key: &script,
            text: None,
        };
        for error in [
            RootBuildScriptError::BuildScript(BuildScriptError::InvalidScriptPath {
                path: "../outside.rhai".into(),
                reason: "escapes the project root".into(),
            }),
            RootBuildScriptError::InvalidSourceRoot("../src".into()),
        ] {
            let out = ProjectDiagnostics::assemble(
                ScriptOutcome::Failed(&error),
                GraphOutcome::NotReached,
                Some(file),
            );
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].anchor, ProjectAnchor::Manifest);
            assert_eq!(out[0].code, ProjectDiagnosticCode::ProjectManifest);
            assert_eq!(out[0].severity, ProjectDiagnosticSeverity::Error);
        }
    }

    #[test]
    fn a_positioned_script_failure_resolves_a_span_against_the_script_it_names() {
        let script = key("build.rhai");
        let source = "let a = 1;\nlet b = ;\n";
        let error = RootBuildScriptError::BuildScript(BuildScriptError::Compile {
            script: script.clone(),
            position: None,
            message: "syntax error".into(),
        });
        let out = ProjectDiagnostics::assemble(
            ScriptOutcome::Failed(&error),
            GraphOutcome::NotReached,
            Some(ScriptFile {
                key: &script,
                text: Some(source),
            }),
        );
        assert_eq!(out[0].anchor, ProjectAnchor::Script(script.clone()));
        assert_eq!(out[0].span, None, "no position reported, so no span");

        // A different script than the error names: resolving in the wrong text would be a silently
        // wrong range, so the diagnostic is still reported and simply carries no span.
        let other = key("other.rhai");
        let out = ProjectDiagnostics::assemble(
            ScriptOutcome::Failed(&error),
            GraphOutcome::NotReached,
            Some(ScriptFile {
                key: &other,
                text: Some(source),
            }),
        );
        assert_eq!(out[0].anchor, ProjectAnchor::Script(script));
        assert_eq!(out[0].span, None);
    }

    #[test]
    fn a_graph_failure_reports_its_warnings_before_the_error() {
        // `GraphResolveError` carries warnings precisely because the dependency discovery warned
        // was unavailable is usually the one preprocessing then failed on. Reporting the error
        // alone reports half of it.
        let failure = GraphResolveError::reporting(
            GraphError::Cycle {
                chain: Vec::<CycleEdge>::new(),
            },
            vec![GraphWarning::node(
                "../lib",
                "classpath entry is unavailable",
            )],
        );
        let out = assemble(ScriptOutcome::Skipped, GraphOutcome::Failed(&failure));

        assert_eq!(
            codes(&out),
            [
                ProjectDiagnosticCode::DependencyResolution,
                ProjectDiagnosticCode::DependencyCycle
            ]
        );
        assert_eq!(
            severities(&out),
            [
                ProjectDiagnosticSeverity::Warning,
                ProjectDiagnosticSeverity::Error
            ]
        );
        // Rendered whole: the message alone does not say which dependency.
        assert!(out[0].message.contains("../lib"));
    }

    #[test]
    fn every_graph_error_has_its_own_code() {
        let cases = [
            (
                GraphError::InvalidRootManifest {
                    message: "bad".into(),
                },
                ProjectDiagnosticCode::ProjectManifest,
            ),
            (
                GraphError::InvalidDependency {
                    declaring: None,
                    dependency: "lib".into(),
                    message: "bad".into(),
                },
                ProjectDiagnosticCode::DependencyInvalid,
            ),
            (
                GraphError::Cycle { chain: Vec::new() },
                ProjectDiagnosticCode::DependencyCycle,
            ),
            (
                GraphError::Acquisition {
                    operation: "clone".into(),
                    message: "bad".into(),
                },
                ProjectDiagnosticCode::DependencyAcquisition,
            ),
        ];
        for (error, expected) in cases {
            let failure = GraphResolveError::unreported(error);
            let out = assemble(ScriptOutcome::Skipped, GraphOutcome::Failed(&failure));
            assert_eq!(codes(&out), [expected]);
        }
    }

    #[test]
    fn a_resolved_graph_reports_all_three_channels() {
        // The regression this test exists for: a host reading `warnings` and `errors` alone dropped
        // the classpath's input warnings, so an unreadable jar reached no client at all.
        let warnings = vec![GraphWarning::node(
            "../lib",
            "source directory is unavailable",
        )];
        let errors = vec![ProjectAssemblyError::new(
            "../lib",
            None,
            "no manifest".to_owned(),
        )];
        let inputs = vec![Warning::new(
            WarningOrigin::Skeleton,
            "unrecognized classpath file",
        )];
        let report = ProjectReport::new(&warnings, &errors, &inputs);
        let out = assemble(ScriptOutcome::Skipped, GraphOutcome::Resolved(report));

        assert_eq!(
            codes(&out),
            [
                ProjectDiagnosticCode::DependencyResolution,
                ProjectDiagnosticCode::DependencyAssembly,
                ProjectDiagnosticCode::ClasspathInput
            ]
        );
        assert_eq!(
            severities(&out),
            [
                ProjectDiagnosticSeverity::Warning,
                ProjectDiagnosticSeverity::Error,
                ProjectDiagnosticSeverity::Warning
            ]
        );
        // Each rendered whole — the classpath one names its origin, which its message does not.
        assert!(out[2].message.starts_with("generated source: "));
    }

    #[test]
    fn an_offline_refusal_adds_one_advisory_that_does_not_restate_its_remedy() {
        let inputs = vec![
            Warning::new(
                WarningOrigin::Skeleton,
                alloc::format!("`a.jar` was {}", NetworkPolicy::OFFLINE_REFUSAL),
            ),
            Warning::new(
                WarningOrigin::Skeleton,
                alloc::format!("`b.jar` was {}", NetworkPolicy::OFFLINE_REFUSAL),
            ),
        ];
        let report = ProjectReport::new(&[], &[], &inputs);
        let out = assemble(ScriptOutcome::Skipped, GraphOutcome::Resolved(report));

        let advisories: Vec<_> = out
            .iter()
            .filter(|d| d.code == ProjectDiagnosticCode::DependencyCache)
            .collect();
        assert_eq!(advisories.len(), 1, "two refusals, one advisory");
        assert_eq!(advisories[0].severity, ProjectDiagnosticSeverity::Info);
        // The message states the condition and nothing else. The sentence that clears it travels
        // beside the code, so a host appending it never says it twice — and a host that cannot run
        // it (a browser tab has no `jals build`) simply does not ask for it.
        assert!(!advisories[0].message.contains("jals build"));
        assert!(advisories[0].code.remedy().is_some());
    }

    #[test]
    fn a_failed_graph_raises_no_advisory() {
        // The advisory is a statement about the resolved input set. A failed phase resolved none,
        // so there is nothing for it to be about.
        let failure = GraphResolveError::reporting(
            GraphError::Acquisition {
                operation: "fetch".into(),
                message: NetworkPolicy::OFFLINE_REFUSAL.into(),
            },
            Vec::new(),
        );
        let out = assemble(ScriptOutcome::Skipped, GraphOutcome::Failed(&failure));
        assert!(
            out.iter()
                .all(|d| d.code != ProjectDiagnosticCode::DependencyCache)
        );
    }

    #[test]
    fn manifest_diagnostics_group_before_script_ones_and_keep_their_order() {
        // A host publishing per file wants contiguous groups; within a group the production order
        // is causal and survives, because the sort is stable.
        let script = key("build.rhai");
        let error = RootBuildScriptError::BuildScript(BuildScriptError::ReportedErrors(vec![
            BuildScriptDiagnostic::warning("first"),
            BuildScriptDiagnostic::error("second"),
        ]));
        let warnings = vec![
            GraphWarning::node("../a", "one"),
            GraphWarning::node("../b", "two"),
        ];
        let report = ProjectReport::new(&warnings, &[], &[]);
        let out = ProjectDiagnostics::assemble(
            ScriptOutcome::Failed(&error),
            GraphOutcome::Resolved(report),
            Some(ScriptFile {
                key: &script,
                text: None,
            }),
        );

        let anchors: Vec<_> = out.iter().map(|d| d.anchor.clone()).collect();
        assert_eq!(
            anchors,
            [
                ProjectAnchor::Manifest,
                ProjectAnchor::Manifest,
                ProjectAnchor::Script(script.clone()),
                ProjectAnchor::Script(script),
            ]
        );
        assert!(out[0].message.contains("../a"));
        assert!(out[1].message.contains("../b"));
        assert_eq!(messages(&out)[2..], ["first", "second"]);
    }

    #[test]
    fn every_code_has_a_distinct_wire_spelling() {
        // A host carries the code as a string; two arms sharing one spelling would silently merge
        // two conditions a client can filter on.
        let mut spellings: Vec<&str> = EVERY_CODE.iter().map(|code| code.as_str()).collect();
        spellings.sort_unstable();
        let count = spellings.len();
        spellings.dedup();
        assert_eq!(spellings.len(), count);
    }

    #[test]
    fn only_the_dependency_cache_advisory_names_a_command() {
        // Every other condition is cleared by editing the project, so naming a command for one
        // would tell a reader to run something that cannot help.
        let with_remedy: Vec<_> = EVERY_CODE
            .iter()
            .filter(|code| code.remedy().is_some())
            .collect();
        assert_eq!(with_remedy, [&ProjectDiagnosticCode::DependencyCache]);
        assert_eq!(
            ProjectDiagnosticCode::DependencyCache.remedy(),
            Some("run `jals build` to fetch them")
        );
    }

    #[test]
    fn a_span_less_diagnostic_is_placed_on_the_first_line_of_its_anchor() {
        // The one answer two hosts used to give differently. The CRLF case is the reason it is
        // worth having one: a range ending on the `\r` highlights a character nobody can see.
        for (text, expected) in [
            ("[package]\nname = \"a\"\n", 0..9),
            ("[package]\r\nname = \"a\"\r\n", 0..9),
            ("only line, no terminator", 0..24),
            ("", 0..0),
            ("\n", 0..0),
        ] {
            assert_eq!(placed(None).placement_in(text), expected, "text: {text:?}");
        }
    }

    #[test]
    fn a_positioned_diagnostic_is_placed_at_its_span() {
        // A resolved span is the narrowest thing known, so the text is not consulted at all — not
        // even to clamp, which is `LineIndex`'s job in whichever coordinates the host wants.
        assert_eq!(
            placed(Some(11..15)).placement_in("[package]\nname\n"),
            11..15
        );
        assert_eq!(placed(Some(11..15)).placement_in(""), 11..15);
    }

    #[test]
    fn only_an_error_makes_a_project_unassemblable() {
        let warning = shaped(None, ProjectDiagnosticSeverity::Warning);
        let info = shaped(None, ProjectDiagnosticSeverity::Info);
        let error = shaped(None, ProjectDiagnosticSeverity::Error);

        assert!(!ProjectDiagnostics::has_errors(&[]));
        assert!(!ProjectDiagnostics::has_errors(&[warning.clone(), info]));
        assert!(ProjectDiagnostics::has_errors(&[warning, error]));
    }
}
