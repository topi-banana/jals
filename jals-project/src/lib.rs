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
mod graph;
#[cfg(all(test, feature = "native"))]
mod graph_tests;
mod memory;
#[cfg(feature = "native")]
mod native;
mod task;

pub use assemble::{
    CompileClasspathEntry, CompileClasspathFile, CompileClasspathTree, CompileClasspathTreeMember,
    ProjectAssemblyError,
};
pub use assembly::{GraphResolveError, MemoryProjectAssembly, ProjectAssembly, ProjectScript};
pub use graph::{
    CycleEdge, GraphEdge, GraphError, GraphMetadata, GraphNodeMetadata, GraphPreprocess,
    GraphWarning, NodeId, NodeKind,
};
#[cfg(feature = "native")]
pub use native::NativeProjectAssembly;
// `ProjectGraphAssembly`, `ResolvedProjectGraph`, `PreprocessedProjectGraph`, `MemoryProjectGraph`,
// and `NativeProjectGraph` are deliberately *not* re-exported. They are the steps `ProjectAssembly`
// sequences and the intermediate values that only exist between them; a host hands over policy and
// an aggregate and receives an assembly, so naming any of them outside this crate would mean
// hand-sequencing the phases again.
pub use task::{
    BuildTaskExecutor, BuildTaskHost, BuildTaskRunError, RootBuildScriptError,
    RootBuildScriptOptions, SourcePublication,
};
