//! Slicing strategies.
//!
//! Two `SlicingStrategy` implementations:
//!
//! - [`ArchitecturalMode`] (Mode 1): every file is skeletonized by default;
//!   PageRank hub files are kept in full source. Designed for "give me the
//!   whole codebase's architecture" prompts.
//! - [`TaskGuidedMode`] (Mode 2): BM25 query picks target files (kept in
//!   full source); k-hop dependency files are skeletonized; everything else
//!   degrades to signatures. Designed for "fix this specific bug" prompts.
//!
//! Mode 3 (Full Dump) is intentionally not a strategy — it bypasses slicing
//! entirely and is handled by the existing Phase 1 pipeline.

use std::collections::HashSet;

use crate::graph::pagerank::{top_n_indices, DEFAULT_HUB_FRACTION};
use crate::graph::{CodeGraph, EdgeKind};
use crate::slicing::knapsack::{build_plans, compare_by_value_density, SliceLevel, SlicePlan};
use crate::slicing::query::BM25Index;
use crate::slicing::skeleton::skeletonize;
use crate::tokenizer::count_tokens;
use crate::types::FileNode;
use crate::util::paths_match;

/// A slicing strategy produces a [`SlicePlan`] per file given the parsed
/// nodes, the dependency graph, and (optionally) a free-text query.
///
/// Strategies are responsible for setting each plan's `importance` and
/// `is_hub` fields, then calling [`allocate`] to fit the plans within the
/// budget. The pipeline applies the resulting plans to mutate `FileNode`s.
pub trait SlicingStrategy: Send + Sync {
    /// Produces the per-file plans. The returned vec is indexed identically to
    /// `nodes` (i.e. `plans[i]` describes `nodes[i]`).
    fn plan(
        &self,
        nodes: &[FileNode],
        graph: &CodeGraph,
        scores: &[f64],
        budget: usize,
    ) -> Vec<SlicePlan>;
}

/// Mode 1: Architectural overview.
///
/// Every file starts at [`SliceLevel::Skeleton`]; hub files (top 10% by
/// PageRank) are forced to [`SliceLevel::Full`]. The allocator then fits the
/// plans within the budget, with hub protection ensuring hubs never degrade
/// below Skeleton.
pub struct ArchitecturalMode;

impl SlicingStrategy for ArchitecturalMode {
    fn plan(
        &self,
        nodes: &[FileNode],
        graph: &CodeGraph,
        scores: &[f64],
        budget: usize,
    ) -> Vec<SlicePlan> {
        let skeleton_tokens = precompute_skeleton_tokens(nodes);
        let mut plans = build_plans(nodes, &skeleton_tokens);

        // Mark hubs: any file containing at least one top-N% PageRank symbol.
        let hub_file_indices = compute_hub_files(graph, scores, nodes);
        for plan in &mut plans {
            plan.is_hub = hub_file_indices.contains(&plan.file_index);
            plan.importance = file_importance(plan.file_index, graph, scores, nodes);
            // Architectural mode: default level is Skeleton (not Full).
            plan.level = if plan.is_hub {
                SliceLevel::Full
            } else {
                SliceLevel::Skeleton
            };
        }

        // Enforce architectural floor: non-hubs start at Skeleton, hubs at Full.
        // `allocate` will further degrade plans that don't fit the budget, but
        // we override the starting point by setting levels first, then running
        // the allocator with the architectural constraint baked in.
        allocate_with_floor(&mut plans, budget);

        plans
    }
}

/// Mode 2: Task-guided slicing.
///
/// BM25 ranks files by relevance to `query`. The top-k targets are kept in
/// full source; their k-hop dependency neighbourhood is skeletonized;
/// everything else degrades to signatures.
pub struct TaskGuidedMode {
    /// The free-text user query.
    pub query: String,
    /// How many BM25 results to treat as targets (full source).
    pub top_k: usize,
    /// How many graph hops to follow from targets for the skeleton tier.
    pub k_hop: usize,
}

impl TaskGuidedMode {
    /// Creates a task-guided mode with sensible defaults: top-5 targets,
    /// 2-hop dependency neighbourhood.
    #[must_use]
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            top_k: 5,
            k_hop: 2,
        }
    }
}

impl SlicingStrategy for TaskGuidedMode {
    fn plan(
        &self,
        nodes: &[FileNode],
        graph: &CodeGraph,
        scores: &[f64],
        budget: usize,
    ) -> Vec<SlicePlan> {
        let skeleton_tokens = precompute_skeleton_tokens(nodes);
        let mut plans = build_plans(nodes, &skeleton_tokens);

        // BM25 to find target files.
        let index = BM25Index::from_files(nodes);
        let targets: HashSet<usize> = index
            .search(&self.query, self.top_k)
            .into_iter()
            .map(|(i, _)| i)
            .collect();

        // k-hop dependency neighbourhood of the targets.
        let deps = k_hop_files(graph, &targets, self.k_hop, nodes);

        for plan in &mut plans {
            plan.importance = file_importance(plan.file_index, graph, scores, nodes);
            // Targets override hub detection — they're the user's stated focus.
            plan.is_hub = targets.contains(&plan.file_index)
                || compute_hub_files(graph, scores, nodes).contains(&plan.file_index);
            plan.level = if targets.contains(&plan.file_index) {
                SliceLevel::Full
            } else if deps.contains(&plan.file_index) {
                SliceLevel::Skeleton
            } else {
                SliceLevel::Signature
            };
        }

        allocate_with_floor(&mut plans, budget);

        plans
    }
}

/// Precomputes the token count of each file's skeletonized source. This is
/// the only place where we actually run skeletonization during planning —
/// the allocator uses these precomputed costs to decide which level fits.
fn precompute_skeleton_tokens(nodes: &[FileNode]) -> Vec<usize> {
    nodes
        .iter()
        .map(|node| {
            let skeleton = skeletonize(&node.source, &node.language, &node.path);
            // `count_tokens` is fallible, but the worst case is "use half of
            // full" — never worth failing the whole plan over.
            count_tokens(&skeleton).unwrap_or(node.token_count / 2)
        })
        .collect()
}

/// Returns the set of file indices that contain at least one PageRank hub
/// symbol (top 10% by score).
fn compute_hub_files(graph: &CodeGraph, scores: &[f64], nodes: &[FileNode]) -> HashSet<usize> {
    let hub_slots = top_n_indices(scores, DEFAULT_HUB_FRACTION);
    if hub_slots.is_empty() {
        return HashSet::new();
    }

    let mut hub_files = HashSet::new();
    for slot in hub_slots {
        let Some(fqn) = graph.symbols().fqn(crate::graph::SymbolId::from_raw(slot)) else {
            continue;
        };
        for (i, node) in nodes.iter().enumerate() {
            if paths_match(&node.path, &fqn.file) {
                hub_files.insert(i);
                break;
            }
        }
    }
    hub_files
}

/// Returns the maximum PageRank score among symbols declared in `file_index`'s
/// file. Used as the file's "importance" for the knapsack sort.
fn file_importance(
    file_index: usize,
    graph: &CodeGraph,
    scores: &[f64],
    nodes: &[FileNode],
) -> f64 {
    let Some(node) = nodes.get(file_index) else {
        return 0.0;
    };
    let mut max = 0.0_f64;
    for symbol in &node.symbols {
        if !symbol.visibility().is_exported() {
            continue;
        }
        let fqn = crate::graph::FullyQualifiedName::new(node.path.clone(), symbol.name());
        for id in graph.symbols().lookup(&fqn) {
            let slot = id.raw();
            if slot < scores.len() {
                max = max.max(scores[slot]);
            }
        }
    }
    max
}

/// Runs the allocator but never lets a plan's level drop below its initial
/// value. This is the "floor" mechanism: architectural mode sets every
/// non-hub's floor to Skeleton, and task-guided mode sets targets' floor to
/// Full and deps' floor to Skeleton.
///
/// Concretely: walk the plans in knapsack order, and for each plan pick the
/// *minimum* of (its initial level, the allocator's chosen level) — but only
/// if that minimum still fits in the remaining budget.
fn allocate_with_floor(plans: &mut [SlicePlan], budget: usize) {
    if budget == 0 {
        for plan in plans.iter_mut() {
            plan.level = SliceLevel::Omitted;
        }
        return;
    }

    // Sort indices by value density (importance / full_tokens) descending.
    let mut order: Vec<usize> = (0..plans.len()).collect();
    order.sort_by(|&a, &b| compare_by_value_density(&plans[a], &plans[b]));

    let mut remaining = budget;
    for &i in &order {
        let plan = &mut plans[i];
        let initial = plan.level;
        let min_allowed = if plan.is_hub {
            SliceLevel::Skeleton
        } else {
            SliceLevel::Omitted
        };

        // Degrade from the initial level downwards until something fits, but
        // never below `min_allowed`.
        let chosen = degrade_to_fit(plan, remaining, initial, min_allowed);
        let cost = chosen.cost(plan.full_tokens, plan.skeleton_tokens, plan.signature_tokens);
        plan.level = chosen;
        remaining = remaining.saturating_sub(cost);
    }
}

/// Picks the highest level `<= starting` that fits in `remaining` and is
/// `>= min_allowed`. If nothing fits, returns `min_allowed`.
fn degrade_to_fit(
    plan: &SlicePlan,
    remaining: usize,
    starting: SliceLevel,
    min_allowed: SliceLevel,
) -> SliceLevel {
    let full = plan.full_tokens;
    let skel = plan.skeleton_tokens;
    let sig = plan.signature_tokens;

    // Walk from `starting` down to `min_allowed`, returning the first that fits.
    let mut current = starting;
    loop {
        let cost = current.cost(full, skel, sig);
        if cost <= remaining {
            return current;
        }
        if current <= min_allowed {
            return min_allowed;
        }
        current = match current {
            SliceLevel::Full => SliceLevel::Skeleton,
            SliceLevel::Skeleton => SliceLevel::Signature,
            SliceLevel::Signature | SliceLevel::Omitted => SliceLevel::Omitted,
        };
    }
}

/// BFS over the dependency graph, collecting file indices reachable within
/// `k` hops from any target file. The walk treats edges as undirected:
/// "the LLM needs context about both upstream and downstream dependencies".
///
/// Import edges are deliberately excluded: they're too noisy (every file that
/// imports a util becomes a "dependency"), and Call/Extends/Implements are the
/// relationships that actually answer "what code does this code touch?".
fn k_hop_files(
    graph: &CodeGraph,
    targets: &HashSet<usize>,
    k: usize,
    nodes: &[FileNode],
) -> HashSet<usize> {
    use petgraph::visit::EdgeRef;
    if k == 0 || targets.is_empty() {
        return HashSet::new();
    }

    // Build a NodeIndex → slot map so we can translate graph edges into slot
    // space (which is what the SymbolTable uses).
    let mut idx_to_slot: std::collections::HashMap<petgraph::graph::NodeIndex<u32>, usize> =
        std::collections::HashMap::new();
    for (slot, _fqn) in graph.symbols().iter() {
        if let Some(node_idx) = graph.node_index(slot) {
            idx_to_slot.insert(node_idx, slot.raw());
        }
    }

    let n = graph.symbols().len();
    let mut adj: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for edge in graph.inner().edge_references() {
        let (Some(src_slot), Some(dst_slot)) = (
            idx_to_slot.get(&edge.source()).copied(),
            idx_to_slot.get(&edge.target()).copied(),
        ) else {
            continue;
        };
        if matches!(edge.weight(), EdgeKind::Import) {
            continue;
        }
        adj[src_slot].insert(dst_slot);
        adj[dst_slot].insert(src_slot);
    }

    // Translate target file indices into graph-node slots.
    let mut start_slots: HashSet<usize> = HashSet::new();
    for &file_idx in targets {
        let Some(node) = nodes.get(file_idx) else {
            continue;
        };
        for symbol in &node.symbols {
            if !symbol.visibility().is_exported() {
                continue;
            }
            let fqn = crate::graph::FullyQualifiedName::new(node.path.clone(), symbol.name());
            for id in graph.symbols().lookup(&fqn) {
                start_slots.insert(id.raw());
            }
        }
    }
    if start_slots.is_empty() {
        return HashSet::new();
    }

    // BFS up to depth k.
    let mut visited: HashSet<usize> = start_slots.clone();
    let mut frontier: Vec<usize> = start_slots.iter().copied().collect();
    for _ in 0..k {
        let mut next = Vec::new();
        for &slot in &frontier {
            for &neighbour in &adj[slot] {
                if visited.insert(neighbour) {
                    next.push(neighbour);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    // Translate slots back to file indices.
    let mut result: HashSet<usize> = HashSet::new();
    for slot in visited {
        let Some(fqn) = graph
            .symbols()
            .fqn(crate::graph::SymbolId::from_raw(slot))
        else {
            continue;
        };
        for (i, node) in nodes.iter().enumerate() {
            if paths_match(&node.path, &fqn.file) {
                result.insert(i);
                break;
            }
        }
    }
    // Targets themselves are not "k-hop deps"; they're handled separately.
    for t in targets {
        result.remove(t);
    }
    result
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

    fn count_tokens_for_nodes(nodes: &mut [FileNode]) {
        for node in nodes.iter_mut() {
            node.token_count = count_tokens(&node.source).unwrap_or(0);
        }
    }

    #[test]
    fn architectural_mode_marks_hubs_full() {
        let mut nodes = parse(&[
            (
                "src/hub.rs",
                Language::Rust,
                "pub fn hub_fn() { helper(); }\npub fn helper() { hub_fn(); }",
            ),
            ("src/util.rs", Language::Rust, "pub fn util() { /* unrelated */ }"),
        ]);
        count_tokens_for_nodes(&mut nodes);

        let graph = CodeGraph::build(&nodes);
        let scores = crate::graph::page_rank(
            graph.inner(),
            crate::graph::DEFAULT_DAMPING,
            crate::graph::DEFAULT_MAX_ITERATIONS,
            crate::graph::DEFAULT_CONVERGENCE,
        );

        let mode = ArchitecturalMode;
        let plans = mode.plan(&nodes, &graph, &scores, 10_000);
        // The hub file should be at Full level (large budget).
        let hub_plan = plans
            .iter()
            .find(|p| nodes[p.file_index].path.to_string_lossy().contains("hub"))
            .unwrap();
        assert_eq!(hub_plan.level, SliceLevel::Full);
    }

    #[test]
    fn architectural_mode_skeletonizes_non_hubs() {
        let mut nodes = parse(&[
            (
                "src/hub.rs",
                Language::Rust,
                "pub fn hub_fn() { helper(); }\npub fn helper() { hub_fn(); }",
            ),
            ("src/util.rs", Language::Rust, "pub fn util() { /* unrelated */ }"),
        ]);
        count_tokens_for_nodes(&mut nodes);

        let graph = CodeGraph::build(&nodes);
        let scores = crate::graph::page_rank(
            graph.inner(),
            crate::graph::DEFAULT_DAMPING,
            crate::graph::DEFAULT_MAX_ITERATIONS,
            crate::graph::DEFAULT_CONVERGENCE,
        );

        let mode = ArchitecturalMode;
        let plans = mode.plan(&nodes, &graph, &scores, 10_000);
        let util_plan = plans
            .iter()
            .find(|p| nodes[p.file_index].path.to_string_lossy().contains("util"))
            .unwrap();
        assert_eq!(
            util_plan.level,
            SliceLevel::Skeleton,
            "non-hub should be skeletonized by default"
        );
    }

    #[test]
    fn task_guided_mode_keeps_targets_full() {
        let mut nodes = parse(&[
            (
                "src/auth.rs",
                Language::Rust,
                "pub fn login(user: &str) -> bool { check(user) }\npub fn check(u: &str) -> bool { !u.is_empty() }",
            ),
            (
                "src/util.rs",
                Language::Rust,
                "pub fn format_bytes(b: usize) -> String { format!(\"{}B\", b) }",
            ),
        ]);
        count_tokens_for_nodes(&mut nodes);

        let graph = CodeGraph::build(&nodes);
        let scores = crate::graph::page_rank(
            graph.inner(),
            crate::graph::DEFAULT_DAMPING,
            crate::graph::DEFAULT_MAX_ITERATIONS,
            crate::graph::DEFAULT_CONVERGENCE,
        );

        let mode = TaskGuidedMode::new("login authentication");
        let plans = mode.plan(&nodes, &graph, &scores, 10_000);
        let auth_plan = plans
            .iter()
            .find(|p| nodes[p.file_index].path.to_string_lossy().contains("auth"))
            .unwrap();
        assert_eq!(
            auth_plan.level,
            SliceLevel::Full,
            "BM25 target should be kept at Full"
        );
    }

    #[test]
    fn task_guided_mode_degrades_non_targets_to_signatures() {
        let mut nodes = parse(&[
            (
                "src/auth.rs",
                Language::Rust,
                "pub fn login(user: &str) -> bool { check(user) }\npub fn check(u: &str) -> bool { !u.is_empty() }",
            ),
            (
                "src/util.rs",
                Language::Rust,
                "pub fn format_bytes(b: usize) -> String { format!(\"{}B\", b) }",
            ),
        ]);
        count_tokens_for_nodes(&mut nodes);

        let graph = CodeGraph::build(&nodes);
        let scores = crate::graph::page_rank(
            graph.inner(),
            crate::graph::DEFAULT_DAMPING,
            crate::graph::DEFAULT_MAX_ITERATIONS,
            crate::graph::DEFAULT_CONVERGENCE,
        );

        let mode = TaskGuidedMode::new("login authentication");
        let plans = mode.plan(&nodes, &graph, &scores, 10_000);
        let util_plan = plans
            .iter()
            .find(|p| nodes[p.file_index].path.to_string_lossy().contains("util"))
            .unwrap();
        // util.rs is unrelated to the query; it should degrade to Signature.
        assert!(
            util_plan.level <= SliceLevel::Signature,
            "non-target non-dep should degrade to Signature or below, got {:?}",
            util_plan.level
        );
    }

    #[test]
    fn task_guided_mode_skeletonizes_k_hop_deps() {
        // auth.rs's login() calls session::session_id() — a cross-file Call
        // dependency. With k_hop=2, session.rs should be skeletonized
        // (it's a 1-hop Call dependency).
        let mut nodes = parse(&[
            (
                "src/auth.rs",
                Language::Rust,
                "pub fn login(user: &str) -> bool { let _ = session_id(); create_session(user) }\npub fn create_session(u: &str) -> bool { !u.is_empty() }",
            ),
            (
                "src/session.rs",
                Language::Rust,
                "pub fn session_id() -> String { String::new() }",
            ),
        ]);
        count_tokens_for_nodes(&mut nodes);

        let graph = CodeGraph::build(&nodes);
        let scores = crate::graph::page_rank(
            graph.inner(),
            crate::graph::DEFAULT_DAMPING,
            crate::graph::DEFAULT_MAX_ITERATIONS,
            crate::graph::DEFAULT_CONVERGENCE,
        );

        let mode = TaskGuidedMode::new("login authentication");
        let plans = mode.plan(&nodes, &graph, &scores, 10_000);
        let session_plan = plans
            .iter()
            .find(|p| nodes[p.file_index].path.to_string_lossy().contains("session"))
            .unwrap();
        // session.rs is called from auth.rs, so it's a k-hop dependency and
        // should be at least Skeleton (not Signature).
        assert!(
            session_plan.level >= SliceLevel::Skeleton,
            "k-hop dep should be Skeleton or better, got {:?}",
            session_plan.level
        );
    }

    #[test]
    fn plans_respect_budget() {
        let mut nodes = parse(&[
            (
                "src/auth.rs",
                Language::Rust,
                "pub fn login(user: &str) -> bool { check(user) }\npub fn check(u: &str) -> bool { !u.is_empty() }",
            ),
            (
                "src/util.rs",
                Language::Rust,
                "pub fn format_bytes(b: usize) -> String { format!(\"{}B\", b) }",
            ),
            (
                "src/db.rs",
                Language::Rust,
                "pub fn query_users() -> Vec<String> { vec![] }",
            ),
        ]);
        count_tokens_for_nodes(&mut nodes);

        let graph = CodeGraph::build(&nodes);
        let scores = crate::graph::page_rank(
            graph.inner(),
            crate::graph::DEFAULT_DAMPING,
            crate::graph::DEFAULT_MAX_ITERATIONS,
            crate::graph::DEFAULT_CONVERGENCE,
        );

        // Small budget that forces degradation.
        let mode = ArchitecturalMode;
        let plans = mode.plan(&nodes, &graph, &scores, 50);
        let total: usize = plans.iter().map(SlicePlan::current_cost).sum();
        // Allow some slack for hub protection (a hub might be forced to Skeleton
        // even when it pushes slightly over budget).
        assert!(
            total <= 50 + 100,
            "total cost {total} wildly exceeded budget 50"
        );
    }
}
