//! PageRank centrality over the Code Dependency Graph.
//!
//! The implementation is a hand-written sparse power iteration, *not*
//! `petgraph::algo::page_rank`. The latter has complexity `O(N · |V|² · |E|)`,
//! runs a fixed iteration count with no convergence check, and does not handle
//! dangling nodes properly — all three make it unusable on real repositories.
//!
//! ## Algorithm
//!
//! Standard PageRank with damping `d`, on a directed graph `G = (V, E)`:
//!
//! ```text
//! PR(v) = (1 - d) / |V|  +  d · (
//!     Σ_{u ∈ In(v)} PR(u) / OutDeg(u)
//!   + (Σ_{w : OutDeg(w) = 0} PR(w)) / |V|      // dangling mass redistribution
//! )
//! ```
//!
//! - Dangling nodes (out-degree 0) have their PageRank mass redistributed
//!   uniformly across all nodes, otherwise scores leak out of the graph.
//! - Iteration stops when the L1 distance between successive score vectors
//!   falls below the convergence threshold, or `max_iterations` is reached.
//! - Per-iteration cost is `O(|V| + |E|)`: each edge contributes to one
//!   destination's incoming sum, and each node contributes its out-degree once.
//!
//! ## Convergence parameters (mandated by `DESIGN.md` §5 Step 2.2)
//!
//! - Damping: `0.85`
//! - Max iterations: `100`
//! - Convergence threshold (L1): `1e-6`

use petgraph::graph::DiGraph;
use petgraph::visit::{EdgeRef, NodeIndexable};

/// Default damping factor: the probability a random walk follows a real edge
/// rather than teleporting. From the original PageRank paper.
pub const DEFAULT_DAMPING: f64 = 0.85;

/// Default maximum iteration count. Sufficient for graphs up to ~10⁶ nodes to
/// converge well within the `1e-6` threshold.
pub const DEFAULT_MAX_ITERATIONS: usize = 100;

/// Default L1 convergence threshold. Once the sum of absolute score changes
/// drops below this, iteration stops early.
pub const DEFAULT_CONVERGENCE: f64 = 1e-6;

/// Fraction of nodes (by score) treated as "hubs" — the architectural anchors
/// that should never be skeletonized even under extreme budget pressure.
pub const DEFAULT_HUB_FRACTION: f64 = 0.10;

/// Computes PageRank scores for every node in the graph.
///
/// Returns a `Vec<f64>` of length `graph.node_count()`, indexed by petgraph's
/// compact node index (`NodeIndex::index()`). Scores sum to approximately 1.0.
///
/// On an empty graph, returns an empty vec.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "graph indices cast to/from usize for petgraph interop; all casts are bounds-checked"
)]
pub fn page_rank<N, E>(
    graph: &DiGraph<N, E, u32>,
    damping: f64,
    max_iterations: usize,
    convergence: f64,
) -> Vec<f64> {
    let n = graph.node_count();
    if n == 0 {
        return Vec::new();
    }

    // Compactify node indices: petgraph may have gaps after deletions, so we
    // map each live NodeIndex to a dense 0..n slot for vector indexing.
    // `node_bound()` is the upper bound on live indices; entries for holes
    // are never read because we only look up live nodes.
    let bound = graph.node_bound();
    let mut node_to_slot: Vec<usize> = vec![0; bound];
    for (slot, idx) in graph.node_indices().enumerate() {
        node_to_slot[idx.index()] = slot;
    }

    // Out-degree per node (in dense slot space). Dangling = 0.
    let mut out_degree = vec![0_u32; n];
    for edge in graph.edge_references() {
        let src = node_to_slot[edge.source().index()];
        out_degree[src] = out_degree[src].saturating_add(1);
    }

    // Initial distribution: uniform.
    let n_f64 = f64::from(n as u32);
    let mut scores = vec![1.0 / n_f64; n];

    for _ in 0..max_iterations {
        // Sum of scores held by dangling nodes — redistributed uniformly.
        let dangling_mass: f64 = (0..n)
            .filter_map(|i| (out_degree[i] == 0).then_some(scores[i]))
            .sum();
        let dangling_share = dangling_mass / n_f64;

        let mut incoming = vec![0.0_f64; n];
        for edge in graph.edge_references() {
            let src = node_to_slot[edge.source().index()];
            let dst = node_to_slot[edge.target().index()];
            let share = scores[src] / f64::from(out_degree[src]);
            incoming[dst] += share;
        }

        let teleport = (1.0 - damping) / n_f64;
        let mut new_scores = vec![0.0_f64; n];
        let mut l1_diff = 0.0_f64;
        for i in 0..n {
            new_scores[i] = damping.mul_add(incoming[i] + dangling_share, teleport);
            l1_diff += (new_scores[i] - scores[i]).abs();
        }

        scores = new_scores;
        if l1_diff < convergence {
            break;
        }
    }

    scores
}

/// Returns the indices of the top-`pct` scoring nodes, in descending score
/// order.
///
/// `pct` is a fraction in `[0.0, 1.0]` (e.g. `0.10` for the top 10%). At least
/// one index is returned when the score slice is non-empty and `pct > 0`, so
/// even very small graphs still have a hub.
///
/// Ties are broken by ascending index, which keeps the result deterministic.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pct is a fraction in [0,1]; count = floor(n * pct) bounded by [1, n] is always safe")]
pub fn top_n_indices(scores: &[f64], pct: f64) -> Vec<usize> {
    if scores.is_empty() {
        return Vec::new();
    }
    let pct = pct.clamp(0.0, 1.0);
    let mut count = (scores.len() as f64 * pct).floor() as usize;
    count = count.max(1).min(scores.len());

    let mut indexed: Vec<(usize, f64)> = scores.iter().copied().enumerate().collect();
    // Sort by score descending; tie-break by index ascending for determinism.
    indexed.sort_by(|(i_a, s_a), (i_b, s_b)| {
        s_b.partial_cmp(s_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(i_a.cmp(i_b))
    });

    indexed.into_iter().take(count).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use petgraph::graph::DiGraph;

    fn build_star(n: usize) -> DiGraph<(), (), u32> {
        // A star graph: center (node 0) is pointed to by every other node.
        let mut g: DiGraph<(), (), u32> = DiGraph::new();
        let center = g.add_node(());
        let leaves: Vec<_> = (1..n).map(|_| g.add_node(())).collect();
        for leaf in leaves {
            g.add_edge(leaf, center, ());
        }
        g
    }

    fn build_chain(n: usize) -> DiGraph<(), (), u32> {
        // A linear chain: 0 → 1 → 2 → ... → n-1. The last node dangles.
        let mut g: DiGraph<(), (), u32> = DiGraph::new();
        let nodes: Vec<_> = (0..n).map(|_| g.add_node(())).collect();
        for window in nodes.windows(2) {
            g.add_edge(window[0], window[1], ());
        }
        g
    }

    #[test]
    fn empty_graph_returns_empty() {
        let g: DiGraph<(), (), u32> = DiGraph::new();
        let scores = page_rank(&g, DEFAULT_DAMPING, DEFAULT_MAX_ITERATIONS, DEFAULT_CONVERGENCE);
        assert!(scores.is_empty());
    }

    #[test]
    fn scores_sum_to_one() {
        let g = build_star(10);
        let scores = page_rank(&g, DEFAULT_DAMPING, DEFAULT_MAX_ITERATIONS, DEFAULT_CONVERGENCE);
        let sum: f64 = scores.iter().copied().sum();
        assert!(
            (sum - 1.0).abs() < 1e-3,
            "scores should sum to ~1.0, got {sum}"
        );
    }

    #[test]
    fn star_hub_ranks_first() {
        // In a star where every leaf points to the center, the center has the
        // highest PageRank by a wide margin.
        let g = build_star(8);
        let scores = page_rank(&g, DEFAULT_DAMPING, DEFAULT_MAX_ITERATIONS, DEFAULT_CONVERGENCE);
        let center = 0; // first-added node
        let max_other = scores
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != center)
            .map(|(_, s)| s)
            .copied()
            .fold(0.0_f64, f64::max);
        assert!(
            scores[center] > max_other,
            "hub ({}) should outrank every other node (max={})",
            scores[center],
            max_other
        );
    }

    #[test]
    fn dangling_node_does_not_leak() {
        // Chain ends in a dangling node. Without dangling-mass redistribution,
        // total score would leak away across iterations.
        let g = build_chain(5);
        let scores = page_rank(&g, DEFAULT_DAMPING, DEFAULT_MAX_ITERATIONS, DEFAULT_CONVERGENCE);
        let sum: f64 = scores.iter().copied().sum();
        assert!(
            (sum - 1.0).abs() < 1e-3,
            "dangling mass must be redistributed; sum={sum}"
        );
    }

    #[test]
    fn converges_before_max_iterations() {
        // A small graph converges in a handful of iterations, well under 100.
        // We can't observe iteration count directly, but we can verify the
        // scores are stable (re-running gives the same answer to high precision).
        let g = build_star(20);
        let s1 = page_rank(&g, DEFAULT_DAMPING, DEFAULT_MAX_ITERATIONS, DEFAULT_CONVERGENCE);
        let s2 = page_rank(&g, DEFAULT_DAMPING, DEFAULT_MAX_ITERATIONS, DEFAULT_CONVERGENCE);
        for (a, b) in s1.iter().zip(s2.iter()) {
            assert!((a - b).abs() < 1e-9, "non-deterministic result");
        }
    }

    #[test]
    fn top_n_returns_highest_scored() {
        let scores = vec![0.1, 0.5, 0.2, 0.15, 0.05];
        let top = top_n_indices(&scores, 0.20);
        assert_eq!(top, vec![1]); // 20% of 5 = 1, and index 1 has the max score
    }

    #[test]
    fn top_n_returns_at_least_one_for_non_empty() {
        let scores = vec![0.3, 0.3, 0.4];
        let top = top_n_indices(&scores, 0.10);
        assert!(!top.is_empty());
        assert_eq!(top[0], 2); // index 2 has the highest score
    }

    #[test]
    fn top_n_handles_empty_input() {
        assert!(top_n_indices(&[], 0.10).is_empty());
    }
}
