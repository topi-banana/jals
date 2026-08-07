#![cfg_attr(not(any(feature = "native", test)), no_std)]
//! Transitive project-graph discovery, preprocessing, and classpath projection.
//!
//! The portable graph owns stable identities, immutable dependency snapshots, and the phase
//! transition that prevents assembly before every node has been preprocessed. Host acquisition is
//! isolated behind the `native` feature.
//!
//! [`ProjectAssembly`] owns the *order* those parts are used in, and it is the only way in: a host
//! names policy and hands over an aggregate, and the steps it sequences — discovery, preprocessing,
//! and the two projections — are not reachable from outside this crate, so they cannot be run in
//! the wrong order or with a step left out.

extern crate alloc;

mod assemble;
mod assembly;
mod diagnostics;
mod graph;
#[cfg(all(test, feature = "native"))]
mod graph_tests;
mod memory;
#[cfg(feature = "native")]
mod native;
mod remap;
mod task;
mod walk;

pub use assemble::{
    CompileClasspathEntry, CompileClasspathFile, CompileClasspathTree, CompileClasspathTreeMember,
    ProjectAssemblyError,
};
pub use assembly::{GraphResolveError, MemoryProjectAssembly, ProjectAssembly, ProjectScript};
// The canonical, protocol-neutral diagnostics assembly for one project — the project-scoped
// counterpart of `jals_editor::FileDiagnostics`. A host maps a `ProjectDiagnostic` to its
// protocol's shape and sequences nothing.
pub use diagnostics::{
    GraphOutcome, ProjectAnchor, ProjectDiagnostic, ProjectDiagnosticCode,
    ProjectDiagnosticSeverity, ProjectDiagnostics, ProjectReport, ScriptFile, ScriptOutcome,
};
// `CycleEdge` and `NodeId` are here because a public error names them — `GraphError::Cycle` carries
// the chain and `GraphError::InvalidDependency` the declaring node. A host reads neither: they
// render through their `Display`, which is the whole of what they say. `GraphWarning` and
// `ProjectAssemblyError` name a node too, but by its location rather than its identity, so neither
// puts a `NodeId` in front of a reader.
pub use graph::{CycleEdge, GraphError, GraphPreprocess, GraphWarning, NodeId};
#[cfg(feature = "native")]
pub use native::NativeProjectAssembly;
pub use remap::{CompiledClasses, RemapAbsence, RemapPlan, RemapSelection};
// `ProjectGraphAssembly`, `ResolvedProjectGraph`, `PreprocessedProjectGraph`, `MemoryProjectGraph`,
// and `NativeProjectGraph` are deliberately *not* re-exported. They are the steps `ProjectAssembly`
// sequences and the intermediate values that only exist between them; a host hands over policy and
// an aggregate and receives an assembly, so naming any of them outside this crate would mean
// hand-sequencing the phases again.
//
// `GraphMetadata`, `GraphNodeMetadata`, `GraphEdge`, and `NodeKind` are not re-exported for the same
// reason one step further on: the assemblies retain the discovered shape, but every accessor into it
// is now the crate's own, so exporting the types would publish names with nothing readable behind
// them. A host that needs to ask about the graph should get an accessor on the assembly, not these.
pub use task::{
    BuildTaskExecutor, BuildTaskHost, BuildTaskRunError, RootBuildScriptError,
    RootBuildScriptOptions, SourcePublication,
};
