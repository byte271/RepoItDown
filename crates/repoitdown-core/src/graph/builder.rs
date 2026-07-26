//! Code Dependency Graph construction.
//!
//! Builds a [`CodeGraph`] — a `petgraph::DiGraph` whose nodes are exported
//! symbols (one per `(file, name)` pair) and whose edges are the relationships
//! between them: imports, calls, inheritance, and type references.
//!
//! The builder is robust to:
//!
//! - **Cycles** (A → B → A): normal in real code; `DiGraph` handles them.
//! - **Parallel edges** (A imports B twice): deduplicated via a `HashSet`.
//! - **Unresolvable imports**: dropped silently by the [`Resolver`](super::resolve).
//! - **Missing symbols** (a call to `foo()` where `foo` is not exported): the
//!   edge is skipped because there is no target node.
//!
//! The resulting graph is what [`super::pagerank`] runs PageRank over.

use std::collections::HashSet;

use petgraph::graph::{DiGraph, NodeIndex};

use crate::types::FileNode;
use crate::util::paths_match;

use super::resolve::{FullyQualifiedName, Resolver, SymbolId, SymbolTable};

/// The kind of relationship encoded by a graph edge.
///
/// Knowing the kind is useful for downstream slicing: import edges carry
/// different weight than call edges, and inheritance edges are typically the
/// most important for understanding a type hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EdgeKind {
    /// `import { foo } from "./bar"` — file-level dependency.
    Import,
    /// `foo()` — a call site whose target resolves to a known exported symbol.
    Call,
    /// `class Admin extends User` — class inheritance.
    Extends,
    /// `class Svc implements Reader` — interface implementation.
    Implements,
    /// `interface Admin extends Reader` — interface inheritance.
    InterfaceExtends,
}

/// A code dependency graph: the `DiGraph` itself plus the [`SymbolTable`] that
/// gives every node index a meaningful name.
pub struct CodeGraph {
    /// The graph. Node weights are [`FullyQualifiedName`]s; edge weights are
    /// [`EdgeKind`]s.
    graph: DiGraph<FullyQualifiedName, EdgeKind, u32>,
    /// Symbol table shared with the graph's node weights.
    table: SymbolTable,
    /// Map from `SymbolId` (dense) to `NodeIndex` (petgraph). Kept in addition
    /// to `table` because petgraph's `NodeIndex` is the canonical handle for
    /// graph traversal.
    node_of: Vec<NodeIndex<u32>>,
}

impl CodeGraph {
    /// Builds a CDG from a slice of parsed [`FileNode`]s.
    ///
    /// The construction is single-pass and best-effort: every recoverable
    /// relationship becomes an edge, and anything that cannot be resolved is
    /// dropped. The result is always a valid (possibly empty) graph.
    #[must_use]
    pub fn build(nodes: &[FileNode]) -> Self {
        let table = SymbolTable::from_files(nodes);

        // Allocate one petgraph node per symbol id. `table.iter()` yields ids
        // in dense ascending order, so `node_of` can be filled with `push`.
        let mut graph: DiGraph<FullyQualifiedName, EdgeKind, u32> =
            DiGraph::with_capacity(table.len(), table.len() * 4);
        let mut node_of: Vec<NodeIndex<u32>> = Vec::with_capacity(table.len());
        for (_id, fqn) in table.iter() {
            let idx = graph.add_node(fqn.clone());
            node_of.push(idx);
        }

        let resolver = Resolver::new(nodes.iter().map(|n| n.path.clone()));

        // Track which (source, target, kind) triples we've already added so we
        // can dedup parallel edges in O(1) per insertion.
        let mut seen: HashSet<(NodeIndex<u32>, NodeIndex<u32>, EdgeKind)> = HashSet::new();

        for node in nodes {
            let importer_ids: Vec<SymbolId> = node
                .symbols
                .iter()
                .filter(|s| s.visibility().is_exported())
                .flat_map(|s| {
                    table
                        .lookup(&FullyQualifiedName::new(node.path.clone(), s.name()))
                        .iter()
                        .copied()
                })
                .collect();

            // If the file has no exported symbols, it cannot contribute edges.
            if importer_ids.is_empty() {
                continue;
            }

            // Imports: file-level dependency, fan out from every exported symbol
            // in the importer to every named (or all, for wildcard) exported
            // symbol in the imported file.
            for import in &node.imports {
                let Some(target_path) =
                    resolver.resolve(&node.path, &import.module, &node.language)
                else {
                    continue;
                };

                let targets: Vec<SymbolId> = nodes
                    .iter()
                    .find(|n| paths_match(&n.path, &target_path))
                    .map(|target_node| {
                        if import.symbols.is_empty() {
                            // Whole-module import: edge to every exported symbol.
                            target_node
                                .symbols
                                .iter()
                                .filter(|s| s.visibility().is_exported())
                                .flat_map(|s| {
                                    table
                                        .lookup(&FullyQualifiedName::new(
                                            target_node.path.clone(),
                                            s.name(),
                                        ))
                                        .iter()
                                        .copied()
                                })
                                .collect()
                        } else {
                            // Named import: edge to each named, exported symbol.
                            import
                                .symbols
                                .iter()
                                .flat_map(|name| {
                                    table
                                        .lookup(&FullyQualifiedName::new(
                                            target_node.path.clone(),
                                            name,
                                        ))
                                        .iter()
                                        .copied()
                                })
                                .collect()
                        }
                    })
                    .unwrap_or_default();

                for src in &importer_ids {
                    for dst in &targets {
                        add_edge(
                            &mut graph,
                            &mut seen,
                            node_of[src.raw()],
                            node_of[dst.raw()],
                            EdgeKind::Import,
                        );
                    }
                }
            }

            // Calls: create precise edges from the enclosing function to the
            // callee. Falls back to file-level fan-out for module-level calls.
            add_call_edges(node, &table, &node_of, &importer_ids, &mut graph, &mut seen);

            // Inheritance: edges from class/interface symbols to their parents.
            add_inheritance_edges(node, &table, &node_of, &mut graph, &mut seen);
        }

        Self {
            graph,
            table,
            node_of,
        }
    }

    /// Number of nodes (exported symbols) in the graph.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of edges (deduplicated relationships) in the graph.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Looks up the petgraph node index for a symbol id.
    #[must_use]
    pub fn node_index(&self, id: SymbolId) -> Option<NodeIndex<u32>> {
        self.node_of.get(id.raw()).copied()
    }

    /// Returns a reference to the inner petgraph for direct traversal
    /// (PageRank, edge iteration, etc.).
    #[must_use]
    pub fn inner(&self) -> &DiGraph<FullyQualifiedName, EdgeKind, u32> {
        &self.graph
    }

    /// Returns the symbol table for lookup and iteration.
    #[must_use]
    pub fn symbols(&self) -> &SymbolTable {
        &self.table
    }
}

/// Adds call edges from the enclosing function to each callee.
///
/// If a call has a known `enclosing_symbol`, the edge source is that specific
/// function. If the call is at module level (no enclosing function), the edge
/// fans out from every exported symbol in the file as a fallback.
fn add_call_edges(
    node: &FileNode,
    table: &SymbolTable,
    node_of: &[NodeIndex<u32>],
    importer_ids: &[SymbolId],
    graph: &mut DiGraph<FullyQualifiedName, EdgeKind, u32>,
    seen: &mut HashSet<(NodeIndex<u32>, NodeIndex<u32>, EdgeKind)>,
) {
    for call in &node.calls {
        let callee_ids = table.lookup_by_name(&call.callee);
        if callee_ids.is_empty() {
            continue;
        }

        let source_ids: Vec<SymbolId> = call.enclosing_symbol.as_ref().map_or_else(
            || importer_ids.to_vec(),
            |enclosing| {
                table
                    .lookup(&FullyQualifiedName::new(node.path.clone(), enclosing))
                    .to_vec()
            },
        );

        for src in &source_ids {
            for &dst in callee_ids {
                if dst.raw() < node_of.len() {
                    add_edge(
                        graph,
                        seen,
                        node_of[src.raw()],
                        node_of[dst.raw()],
                        EdgeKind::Call,
                    );
                }
            }
        }
    }
}

/// Adds edges from class/interface symbols to their parents (extends / implements).
fn add_inheritance_edges(
    node: &FileNode,
    table: &SymbolTable,
    node_of: &[NodeIndex<u32>],
    graph: &mut DiGraph<FullyQualifiedName, EdgeKind, u32>,
    seen: &mut HashSet<(NodeIndex<u32>, NodeIndex<u32>, EdgeKind)>,
) {
    for symbol in &node.symbols {
        // A symbol may have multiple IDs if duplicates exist; use the first.
        // All IDs for the same FQN point to the same file, so any will do
        // for edge creation.
        let ids = table.lookup(&FullyQualifiedName::new(node.path.clone(), symbol.name()));
        let Some(&first_id) = ids.first() else {
            continue;
        };
        let source_idx = node_of[first_id.raw()];

        if let crate::types::Symbol::Class(class) = symbol {
            if let Some(extends) = &class.extends {
                for dst in table.lookup_by_name(extends) {
                    add_edge(
                        graph,
                        seen,
                        source_idx,
                        node_of[dst.raw()],
                        EdgeKind::Extends,
                    );
                }
            }
            for impl_name in &class.implements {
                for dst in table.lookup_by_name(impl_name) {
                    add_edge(
                        graph,
                        seen,
                        source_idx,
                        node_of[dst.raw()],
                        EdgeKind::Implements,
                    );
                }
            }
        } else if let crate::types::Symbol::Interface(iface) = symbol {
            for ext in &iface.extends {
                for dst in table.lookup_by_name(ext) {
                    add_edge(
                        graph,
                        seen,
                        source_idx,
                        node_of[dst.raw()],
                        EdgeKind::InterfaceExtends,
                    );
                }
            }
        }
    }
}

/// Adds an edge to the graph unless the same `(source, target, kind)` triple is
/// already present. Deduplication keeps the graph small when the same import or
/// call appears multiple times.
fn add_edge(
    graph: &mut DiGraph<FullyQualifiedName, EdgeKind, u32>,
    seen: &mut HashSet<(NodeIndex<u32>, NodeIndex<u32>, EdgeKind)>,
    src: NodeIndex<u32>,
    dst: NodeIndex<u32>,
    kind: EdgeKind,
) {
    if src == dst {
        // No self-loops: a file importing itself or a function calling itself
        // adds no architectural information.
        return;
    }
    if seen.insert((src, dst, kind)) {
        graph.add_edge(src, dst, kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::ParserPool;
    use crate::ingestion::FileEntry;
    use crate::types::Language;
    use petgraph::visit::EdgeRef;
    use std::path::PathBuf;

    fn parse(src_files: &[(&str, Language, &str)]) -> Vec<FileNode> {
        let entries: Vec<FileEntry> = src_files
            .iter()
            .map(|(p, l, s)| FileEntry::new(PathBuf::from(p), l.clone(), (*s).to_owned(), 100))
            .collect();
        ParserPool::new().parse_all(&entries)
    }

    #[test]
    fn builds_empty_graph_for_no_symbols() {
        let nodes = parse(&[("empty.rs", Language::Rust, "// just a comment")]);
        let g = CodeGraph::build(&nodes);
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn builds_graph_with_call_edge() {
        let nodes = parse(&[(
            "src/lib.rs",
            Language::Rust,
            "pub fn main() { helper(); }\npub fn helper() {}",
        )]);
        let g = CodeGraph::build(&nodes);
        // Both `main` and `helper` are exported; main calls helper.
        assert!(g.edge_count() >= 1);
        let has_call_edge = g
            .inner()
            .edge_references()
            .any(|e| *e.weight() == EdgeKind::Call);
        assert!(has_call_edge, "expected at least one Call edge");
    }

    #[test]
    fn call_edge_uses_enclosing_function_not_fanout() {
        // With the enclosing-symbol fix, a call inside `main()` should create
        // exactly ONE Call edge from `main` to the callee — not one edge per
        // exported symbol in the file.
        let nodes = parse(&[(
            "src/lib.rs",
            Language::Rust,
            "pub fn alpha() { helper(); }\npub fn beta() {}\npub fn gamma() {}\npub fn helper() {}",
        )]);
        let g = CodeGraph::build(&nodes);

        // Count Call edges: should be exactly 1 (alpha → helper), not 3
        // (alpha → helper, beta → helper, gamma → helper).
        let call_edges: Vec<_> = g
            .inner()
            .edge_references()
            .filter(|e| *e.weight() == EdgeKind::Call)
            .collect();
        assert_eq!(
            call_edges.len(),
            1,
            "expected exactly 1 Call edge (from alpha to helper), got {}",
            call_edges.len()
        );

        // Verify the edge source is `alpha`, not `beta` or `gamma`.
        let edge = call_edges[0];
        let source_fqn = g.inner().node_weight(edge.source()).unwrap();
        assert_eq!(source_fqn.name, "alpha");
        let target_fqn = g.inner().node_weight(edge.target()).unwrap();
        assert_eq!(target_fqn.name, "helper");
    }

    #[test]
    fn builds_graph_with_extends_edge() {
        let nodes = parse(&[(
            "src/types.ts",
            Language::TypeScript,
            "export class Animal {}\nexport class Dog extends Animal {}",
        )]);
        let g = CodeGraph::build(&nodes);
        let has_extends = g
            .inner()
            .edge_references()
            .any(|e| *e.weight() == EdgeKind::Extends);
        assert!(has_extends, "expected an Extends edge");
    }

    #[test]
    fn deduplicates_parallel_edges() {
        // Two calls to the same function should produce one Call edge.
        let nodes = parse(&[(
            "src/lib.rs",
            Language::Rust,
            "pub fn main() { helper(); helper(); }\npub fn helper() {}",
        )]);
        let g = CodeGraph::build(&nodes);
        let call_edges = g
            .inner()
            .edge_references()
            .filter(|e| *e.weight() == EdgeKind::Call)
            .count();
        assert_eq!(call_edges, 1, "parallel Call edges should be deduplicated");
    }

    #[test]
    fn handles_cycles_without_panicking() {
        let nodes = parse(&[(
            "src/a.rs",
            Language::Rust,
            "pub fn a() { b(); }\npub fn b() { a(); }",
        )]);
        // This builds a cyclic graph (a → b → a). It must not panic.
        let g = CodeGraph::build(&nodes);
        assert!(g.node_count() >= 2);
        // Cycle exists: at least one outgoing edge from each of two nodes.
        assert!(g.edge_count() >= 2);
    }

    #[test]
    fn drops_unresolvable_imports() {
        let nodes = parse(&[(
            "src/lib.rs",
            Language::Rust,
            "use std::collections::HashMap;\npub fn main() {}",
        )]);
        let g = CodeGraph::build(&nodes);
        // No Import edges because `std::collections::HashMap` is external.
        let import_edges = g
            .inner()
            .edge_references()
            .filter(|e| *e.weight() == EdgeKind::Import)
            .count();
        assert_eq!(import_edges, 0);
    }
}
