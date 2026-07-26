//! RepoItDown — AST-aware codebase topology for LLM context windows.
//!
//! Transforms repositories into token-optimized Markdown topologies using
//! tree-sitter AST parsing, PageRank centrality on a Code Dependency Graph,
//! and fractional knapsack token budget allocation.

#![forbid(unsafe_code)]
#![warn(
    clippy::all,
    clippy::pedantic,
    clippy::cargo,
    rust_2018_idioms,
    unused_qualifications
)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::multiple_crate_versions,
    clippy::doc_markdown,
    reason = "workspace-level allows: module names are intentional, error docs deferred, version duplication from syn is unavoidable, markdown in doc comments is valid"
)]

pub mod ast;
pub mod error;
pub mod graph;
pub mod ingestion;
pub mod output;
pub mod pipeline;
pub mod slicing;
pub mod tokenizer;
pub mod types;
pub mod util;

pub use error::{Error, Result};
pub use graph::{
    CodeGraph, DEFAULT_CONVERGENCE, DEFAULT_DAMPING, DEFAULT_HUB_FRACTION, DEFAULT_MAX_ITERATIONS,
    EdgeKind, FullyQualifiedName, GraphEdge, GraphExport, GraphNode, Resolver, SymbolId,
    SymbolTable, export_graph_json, page_rank, top_n_indices,
};
pub use output::{RenderConfig, render};
pub use pipeline::{Pipeline, SliceMode};
pub use tokenizer::count_tokens;
pub use types::{
    CallRef, ClassDef, EnumDef, Field, FileNode, FileRefs, FunctionDef, ImportRef, InterfaceDef,
    Language, ModuleDef, Parameter, SourceLocation, StructDef, Symbol, TypeAliasDef, Visibility,
};
