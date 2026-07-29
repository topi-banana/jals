#![cfg_attr(not(any(feature = "native", test)), no_std)]
//! Transitive project-graph discovery, preprocessing, and classpath projection.
//!
//! The portable graph owns stable identities, immutable dependency snapshots, and the phase
//! transition that prevents assembly before every node has been preprocessed. Host acquisition is
//! isolated behind the `native` feature.
//!
//! [`ProjectAssembly`] owns the *order* those parts are used in. A host names policy and hands over
//! an aggregate; it does not sequence the steps itself.

extern crate alloc;

mod assemble;
mod assembly;
mod graph;
mod memory;
#[cfg(feature = "native")]
mod native;
mod task;

pub use assemble::{
    CompileClasspathEntry, CompileClasspathFile, CompileClasspathTree, CompileClasspathTreeMember,
    ProjectAssemblyError, ProjectGraphAssembly,
};
pub use assembly::{MemoryProjectAssembly, ProjectAssembly, ProjectScript};
pub use graph::{
    CycleEdge, GraphEdge, GraphError, GraphMetadata, GraphNodeMetadata, GraphPreprocess,
    GraphWarning, NodeId, NodeKind, PreprocessedProjectGraph, ResolvedProjectGraph,
};
pub use memory::MemoryProjectGraph;
#[cfg(feature = "native")]
pub use native::{NativeProjectAssembly, NativeProjectGraph};
pub use task::{
    BuildTaskExecutor, BuildTaskHost, BuildTaskRunError, RootBuildScriptError,
    RootBuildScriptOptions, SourcePublication,
};
