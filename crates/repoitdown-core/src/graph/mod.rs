//! Stage 3 — Code Dependency Graph (CDG).
//!
//! Builds a directed graph of exported symbols connected by imports, calls,
//! and inheritance edges, then runs PageRank centrality to identify the
//! architectural "hubs" of a repository.
//!
//! ## Module layout
//!
//! - [`resolve`] — `SymbolTable` (FQN ↔ SymbolId) and `Resolver` (raw module
//!   specifiers → real file paths).
//! - [`builder`] — `CodeGraph::build` turns a slice of `FileNode`s into a
//!   `petgraph::DiGraph` of deduplicated edges.
//! - [`pagerank`] — hand-written sparse power iteration (do not use
//!   `petgraph::algo::page_rank`; see the module docs for why).

pub mod builder;
pub mod export;
pub mod pagerank;
pub mod resolve;

pub use builder::{CodeGraph, EdgeKind};
pub use export::{export_graph_json, GraphEdge, GraphExport, GraphNode};
pub use pagerank::{
    page_rank, top_n_indices, DEFAULT_CONVERGENCE, DEFAULT_DAMPING, DEFAULT_HUB_FRACTION,
    DEFAULT_MAX_ITERATIONS,
};
pub use resolve::{FullyQualifiedName, Resolver, SymbolId, SymbolTable};
