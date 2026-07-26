//! Graph export for visualization.
//!
//! Exports the Code Dependency Graph as serializable JSON structures so that
//! external tools (D3.js visualizer, Python SDK, etc.) can render the
//! dependency topology without re-implementing graph construction.
//!
//! The exported graph includes:
//! - Nodes with name, file path, PageRank score, and symbol kind
//! - Edges with source/target node indices, edge kind, and weight

use crate::graph::{page_rank, CodeGraph, EdgeKind, DEFAULT_CONVERGENCE, DEFAULT_DAMPING, DEFAULT_MAX_ITERATIONS};
use crate::types::FileNode;
use petgraph::visit::EdgeRef;
use serde::Serialize;

/// A single node in the exported graph, representing an exported symbol.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    /// Unique index of this node in the graph.
    pub id: usize,
    /// Symbol name (e.g. `"main"`, `"User"`).
    pub name: String,
    /// File path this symbol belongs to.
    pub file: String,
    /// Symbol kind label (e.g. `"function"`, `"struct"`, `"class"`).
    pub kind: String,
    /// PageRank centrality score (0.0–1.0). Higher = more architecturally important.
    pub score: f64,
}

/// A single edge in the exported graph, representing a dependency relationship.
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub source: usize,
    pub target: usize,
    pub kind: String,
}

/// The full exported graph, ready for JSON serialization.
#[derive(Debug, Clone, Serialize)]
pub struct GraphExport {
    /// All nodes in the graph.
    pub nodes: Vec<GraphNode>,
    /// All edges in the graph.
    pub edges: Vec<GraphEdge>,
    /// Total node count.
    pub node_count: usize,
    /// Total edge count.
    pub edge_count: usize,
}

/// Builds a CDG, runs PageRank, and exports the result as a serializable
/// `GraphExport`. This is the one-stop function for visualization consumers.
///
/// Extracts symbol kinds (function, struct, class, etc.) by cross-referencing
/// the CodeGraph's node FQNs with the original `FileNode` symbols.
///
/// Returns `None` if the graph is empty (no exported symbols found).
#[must_use]
pub fn export_graph_json(files: &[FileNode]) -> Option<GraphExport> {
    let graph = CodeGraph::build(files);
    if graph.node_count() == 0 {
        return None;
    }

    let scores = page_rank(
        graph.inner(),
        DEFAULT_DAMPING,
        DEFAULT_MAX_ITERATIONS,
        DEFAULT_CONVERGENCE,
    );

    // Build a lookup from (file, name) → kind_label for fast symbol kind resolution.
    let kind_of: std::collections::HashMap<(String, String), String> = files
        .iter()
        .flat_map(|node| {
            let file = node.path.display().to_string();
            node.symbols.iter().map(move |s| {
                ((file.clone(), s.name().to_string()), s.kind_label().to_string())
            })
        })
        .collect();

    let nodes: Vec<GraphNode> = graph
        .inner()
        .node_indices()
        .enumerate()
        .map(|(i, idx)| {
            let fqn = &graph.inner()[idx];
            let file = fqn.file.display().to_string();
            let kind = kind_of
                .get(&(file.clone(), fqn.name.clone()))
                .cloned()
                .unwrap_or_else(|| "symbol".to_string());
            let score = scores.get(i).copied().unwrap_or(0.0);
            GraphNode {
                id: i,
                name: fqn.name.clone(),
                file,
                kind,
                score: (score * 1000.0).round() / 1000.0,
            }
        })
        .collect();

    let edges: Vec<GraphEdge> = graph
        .inner()
        .edge_references()
        .map(|e| {
            let kind_str = match e.weight() {
                EdgeKind::Import => "import",
                EdgeKind::Call => "call",
                EdgeKind::Extends => "extends",
                EdgeKind::Implements => "implements",
                EdgeKind::InterfaceExtends => "interface_extends",
            };
            GraphEdge {
                source: e.source().index(),
                target: e.target().index(),
                kind: kind_str.to_string(),
            }
        })
        .collect();

    Some(GraphExport {
        node_count: nodes.len(),
        edge_count: edges.len(),
        nodes,
        edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::ParserPool;
    use crate::ingestion::FileEntry;
    use crate::types::Language;
    use std::path::PathBuf;

    fn parse(src_files: &[(&str, Language, &str)]) -> Vec<FileNode> {
        let entries: Vec<FileEntry> = src_files
            .iter()
            .map(|(p, l, s)| FileEntry::new(PathBuf::from(p), l.clone(), (*s).to_owned(), 100))
            .collect();
        ParserPool::new().parse_all(&entries)
    }

    #[test]
    fn export_empty_files_returns_none() {
        let nodes = parse(&[("empty.rs", Language::Rust, "// nothing here")]);
        assert!(export_graph_json(&nodes).is_none());
    }

    #[test]
    fn export_with_call_edges_produces_json() {
        let nodes = parse(&[
            (
                "src/lib.rs",
                Language::Rust,
                "pub fn main() { helper(); }\npub fn helper() {}",
            ),
        ]);
        let export = export_graph_json(&nodes).unwrap();
        assert!(export.node_count >= 2);
        assert!(export.edge_count >= 1);
        // All scores should be non-negative and sum to ~1.0.
        let sum: f64 = export.nodes.iter().map(|n| n.score).sum();
        assert!((sum - 1.0).abs() < 0.01, "PageRank scores should sum to ~1.0, got {sum}");
    }

    #[test]
    fn export_is_json_serializable() {
        let nodes = parse(&[
            ("src/types.ts", Language::TypeScript, "export class A {}\nexport class B extends A {}"),
        ]);
        let export = export_graph_json(&nodes).unwrap();
        let json = serde_json::to_string(&export).unwrap();
        assert!(json.contains("\"nodes\""));
        assert!(json.contains("\"edges\""));
        assert!(json.contains("\"extends\""));
    }
}
