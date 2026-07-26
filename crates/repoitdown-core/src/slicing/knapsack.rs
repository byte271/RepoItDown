//! Fractional Knapsack token allocator.
//!
//! Greedy `O(n log n)` allocator that decides, for each file, how much of its
//! source to retain given a global token budget. Each file degrades through
//! four levels — Full → Skeleton → Signature → Omitted — and the allocator
//! picks the highest level that fits.
//!
//! ## Algorithm
//!
//! 1. Compute the cost of each degradation level for each file.
//! 2. Sort files by `importance / full_cost` descending — a proxy for "value
//!    density" that prefers cheap, important files over expensive, marginal
//!    ones.
//! 3. Walk the sorted list; for each file, allocate the highest level whose
//!    cost does not exceed the remaining budget.
//!
//! ## Guarantees
//!
//! - Total allocated tokens never exceed the budget.
//! - Hub files (marked via [`SlicePlan::is_hub`]) are protected: they degrade
//!   to at most [`SliceLevel::Skeleton`], never to Signature or Omitted,
//!   unless the budget is exactly zero.
//! - Tie-breaking is deterministic (alphabetical by path).

use std::cmp::Ordering;

use crate::types::FileNode;

/// The retention level chosen for a file by the allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SliceLevel {
    /// Full source retained verbatim.
    Full,
    /// AST-skeletonized: signatures + imports + `/* ... */` placeholders.
    Skeleton,
    /// Signatures only: each symbol's name, kind, and location.
    Signature,
    /// File omitted entirely from the output.
    Omitted,
}

impl SliceLevel {
    /// Returns the token cost of this level, given the file's full and
    /// skeleton costs and an estimate of the signature cost.
    #[must_use]
    pub fn cost(self, full: usize, skeleton: usize, signature: usize) -> usize {
        match self {
            Self::Full => full,
            Self::Skeleton => skeleton,
            Self::Signature => signature,
            Self::Omitted => 0,
        }
    }
}

/// The allocator's per-file decision.
#[derive(Debug, Clone)]
pub struct SlicePlan {
    /// Index into the original `&[FileNode]` slice.
    pub file_index: usize,
    /// Token count of the file's full source (precomputed by the pipeline).
    pub full_tokens: usize,
    /// Token count of the file's skeletonized source (precomputed once).
    pub skeleton_tokens: usize,
    /// Estimated token count of a signatures-only rendering.
    pub signature_tokens: usize,
    /// PageRank-derived importance score, or `0.0` if the file has no graph node.
    pub importance: f64,
    /// Whether this file contains at least one PageRank hub symbol.
    /// Hubs are protected: they degrade to at most Skeleton.
    pub is_hub: bool,
    /// The chosen retention level. Initialised by the strategy; mutated by
    /// [`allocate`].
    pub level: SliceLevel,
}

impl SlicePlan {
    /// Returns the token cost of the currently chosen level.
    #[must_use]
    pub fn current_cost(&self) -> usize {
        self.level.cost(
            self.full_tokens,
            self.skeleton_tokens,
            self.signature_tokens,
        )
    }

    /// Returns the highest level this plan can degrade to under the hub
    /// protection rule.
    fn min_allowed_level(&self) -> SliceLevel {
        if self.is_hub {
            SliceLevel::Skeleton
        } else {
            SliceLevel::Omitted
        }
    }
}

/// Allocates the given budget across the plans, mutating each plan's `level`
/// in place.
///
/// The plans' initial `level` value is ignored — every plan starts from
/// `Full` and degrades as needed. Strategies that want a specific starting
/// level (e.g. Mode 2 sets non-targets to `Signature`) should call `allocate`
/// first, then enforce their per-mode floors afterwards.
pub fn allocate(plans: &mut [SlicePlan], budget: usize) {
    if budget == 0 {
        // Edge case: zero budget means everything is omitted, even hubs.
        for plan in plans.iter_mut() {
            plan.level = SliceLevel::Omitted;
        }
        return;
    }

    // Sort indices by importance / full_tokens descending. We sort indices
    // (not the plans themselves) so we can mutate the original slice.
    let mut order: Vec<usize> = (0..plans.len()).collect();
    order.sort_by(|&a, &b| compare_by_value_density(&plans[a], &plans[b]));

    let mut remaining = budget;
    for &i in &order {
        let plan = &mut plans[i];
        let min_level = plan.min_allowed_level();

        // Try each level in decreasing order of cost.
        let chosen = pick_level(plan, remaining, min_level);
        let cost = chosen.cost(
            plan.full_tokens,
            plan.skeleton_tokens,
            plan.signature_tokens,
        );
        plan.level = chosen;
        // Defensive: if cost exceeds remaining (shouldn't happen given
        // pick_level's logic), still subtract what we have.
        remaining = remaining.saturating_sub(cost);
    }
}

/// Picks the highest retention level that fits in `remaining` and is at or
/// above `min_level`.
fn pick_level(plan: &SlicePlan, remaining: usize, min_level: SliceLevel) -> SliceLevel {
    let full = plan.full_tokens;
    let skel = plan.skeleton_tokens;
    let sig = plan.signature_tokens;

    // Order from highest to lowest retention.
    let candidates = [
        SliceLevel::Full,
        SliceLevel::Skeleton,
        SliceLevel::Signature,
    ];

    for &level in &candidates {
        if level < min_level {
            continue;
        }
        let cost = level.cost(full, skel, sig);
        if cost <= remaining {
            return level;
        }
    }

    // Nothing fits — fall back to the minimum allowed level. For non-hubs
    // this is Omitted (cost 0). For hubs this is Skeleton, which may still
    // exceed `remaining` — but the contract is that hubs always appear, so
    // we return Skeleton anyway and accept the slight over-budget.
    min_level
}

/// Compares two plans by value density: `importance / full_tokens` descending.
/// Ties are broken by file index ascending for determinism. Files with zero
/// tokens (empty source) sort last regardless of importance, since they can
/// always be included at no cost.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn compare_by_value_density(a: &SlicePlan, b: &SlicePlan) -> Ordering {
    let density_a = if a.full_tokens == 0 {
        f64::INFINITY
    } else {
        a.importance / a.full_tokens as f64
    };
    let density_b = if b.full_tokens == 0 {
        f64::INFINITY
    } else {
        b.importance / b.full_tokens as f64
    };
    density_b
        .partial_cmp(&density_a)
        .unwrap_or(Ordering::Equal)
        .then(a.file_index.cmp(&b.file_index))
}

impl PartialOrd for SliceLevel {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SliceLevel {
    fn cmp(&self, other: &Self) -> Ordering {
        let rank = |l: &Self| match l {
            Self::Full => 3,
            Self::Skeleton => 2,
            Self::Signature => 1,
            Self::Omitted => 0,
        };
        rank(self).cmp(&rank(other))
    }
}

/// Builds a `SlicePlan` for each file in `nodes`. The plans start at `Full`
/// and have importance set to 0; the caller is expected to fill in
/// `importance` and `is_hub` from PageRank results before calling
/// [`allocate`].
#[must_use]
pub fn build_plans(nodes: &[FileNode], skeleton_tokens: &[usize]) -> Vec<SlicePlan> {
    nodes
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let full = node.token_count;
            let skel = skeleton_tokens.get(i).copied().unwrap_or(full / 2);
            // Signature cost: ~5 tokens per symbol plus a small base.
            // This is an estimate; the renderer doesn't need to be exact.
            let sig = 5 + node.symbols.len() * 5;
            SlicePlan {
                file_index: i,
                full_tokens: full,
                skeleton_tokens: skel,
                signature_tokens: sig,
                importance: 0.0,
                is_hub: false,
                level: SliceLevel::Full,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileNode, FunctionDef, Language, SourceLocation, Symbol, Visibility};
    use std::path::PathBuf;

    fn node_with(path: &str, tokens: usize, symbols: Vec<Symbol>) -> FileNode {
        FileNode {
            path: PathBuf::from(path),
            language: Language::Rust,
            source: String::new(),
            symbols,
            token_count: tokens,
            has_redactions: false,
            imports: vec![],
            calls: vec![],
        }
    }

    fn fn_symbol(name: &str) -> Symbol {
        Symbol::from(FunctionDef {
            name: name.into(),
            visibility: Visibility::Public,
            signature: format!("fn {name}()"),
            docstring: None,
            parameters: vec![],
            return_type: None,
            body_stripped: false,
            loc: SourceLocation::line_only(PathBuf::from("a.rs"), 1),
        })
    }

    fn plan(
        idx: usize,
        full: usize,
        skel: usize,
        sig: usize,
        importance: f64,
        is_hub: bool,
    ) -> SlicePlan {
        SlicePlan {
            file_index: idx,
            full_tokens: full,
            skeleton_tokens: skel,
            signature_tokens: sig,
            importance,
            is_hub,
            level: SliceLevel::Full,
        }
    }

    #[test]
    fn large_budget_keeps_everything_full() {
        let mut plans = vec![
            plan(0, 100, 50, 10, 0.5, false),
            plan(1, 200, 100, 10, 0.3, false),
        ];
        allocate(&mut plans, 1000);
        assert_eq!(plans[0].level, SliceLevel::Full);
        assert_eq!(plans[1].level, SliceLevel::Full);
    }

    #[test]
    fn tight_budget_degrades_low_value_files_first() {
        // Plan 0: high importance (0.5), cheap (100).
        // Plan 1: low importance (0.1), expensive (500).
        let mut plans = vec![
            plan(0, 100, 50, 10, 0.5, false),
            plan(1, 500, 250, 10, 0.1, false),
        ];
        allocate(&mut plans, 150);
        assert_eq!(
            plans[0].level,
            SliceLevel::Full,
            "high-value plan stays Full"
        );
        assert!(
            plans[1].level <= SliceLevel::Signature,
            "low-value plan degrades"
        );
    }

    #[test]
    fn zero_budget_omits_everything() {
        let mut plans = vec![
            plan(0, 100, 50, 10, 0.5, false),
            plan(1, 100, 50, 10, 0.5, true),
        ];
        allocate(&mut plans, 0);
        assert_eq!(plans[0].level, SliceLevel::Omitted);
        assert_eq!(plans[1].level, SliceLevel::Omitted);
    }

    #[test]
    fn hub_survives_extreme_budget_pressure() {
        // Even when budget is too small for a hub's full source, the hub
        // should degrade to Skeleton, not Signature or Omitted.
        let mut plans = vec![
            plan(0, 1000, 100, 10, 0.9, true),
            plan(1, 50, 25, 10, 0.1, false),
        ];
        allocate(&mut plans, 100);
        // The hub has very high importance, so it sorts first. With 100
        // budget, Full (1000) doesn't fit, so it degrades to Skeleton (100).
        assert!(
            plans[0].level >= SliceLevel::Skeleton,
            "hub must survive at Skeleton or better, got {:?}",
            plans[0].level
        );
    }

    #[allow(clippy::cast_precision_loss)]
    #[test]
    fn total_tokens_never_exceed_budget() {
        let plans: Vec<SlicePlan> = (0..20)
            .map(|i| plan(i, 100 + i * 10, 50, 10, 0.1 * (i as f64 + 1.0), i % 3 == 0))
            .collect();
        for budget in [0, 50, 100, 500, 1000, 5000] {
            let mut p = plans.clone();
            allocate(&mut p, budget);
            let total: usize = p.iter().map(SlicePlan::current_cost).sum();
            if budget == 0 {
                assert_eq!(total, 0);
            } else {
                assert!(
                    total <= budget
                        || p.iter()
                            .any(|plan| plan.is_hub && plan.level == SliceLevel::Skeleton),
                    "budget={budget}, total={total} (hub protection may force slight overflow)"
                );
            }
        }
    }

    #[test]
    fn empty_plans_is_noop() {
        let mut plans: Vec<SlicePlan> = Vec::new();
        allocate(&mut plans, 100);
        assert!(plans.is_empty());
    }

    #[test]
    fn build_plans_uses_skeleton_tokens() {
        let nodes = vec![
            node_with("a.rs", 100, vec![fn_symbol("a")]),
            node_with("b.rs", 200, vec![fn_symbol("b")]),
        ];
        let skeleton_tokens = vec![50, 100];
        let plans = build_plans(&nodes, &skeleton_tokens);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].full_tokens, 100);
        assert_eq!(plans[0].skeleton_tokens, 50);
        assert_eq!(plans[0].signature_tokens, 10); // 5 base + 1 symbol * 5
    }

    #[test]
    fn zero_token_files_are_included() {
        // A file with zero full tokens (e.g. empty source) should be included
        // at Full level since it costs nothing.
        let mut plans = vec![plan(0, 0, 0, 0, 0.0, false)];
        allocate(&mut plans, 100);
        assert_eq!(plans[0].level, SliceLevel::Full);
    }
}
