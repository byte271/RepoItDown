//! Stage 4 — Adaptive Slicing.
//!
//! Decides, for each file in the repository, how much of its source to retain
//! given a global token budget. The decision is driven by:
//!
//! - **PageRank centrality** (Stage 3): high-centrality "hub" files are
//!   preserved in full source.
//! - **BM25 query relevance** (when a user query is supplied): files matching
//!   the query are kept in full source as "targets".
//! - **A fractional knapsack allocator**: picks the highest retention level
//!   per file that fits the remaining budget.
//!
//! ## Module layout
//!
//! - [`query`] — `BM25Index` and `tokenize` for intent matching.
//! - [`skeleton`] — `skeletonize` for AST-based body stripping.
//! - [`knapsack`] — `SlicePlan`, `SliceLevel`, and `allocate`.
//! - [`modes`] — `SlicingStrategy` trait, `ArchitecturalMode`, `TaskGuidedMode`.

pub mod knapsack;
pub mod modes;
pub mod query;
pub mod skeleton;

pub(crate) use knapsack::{SliceLevel, SlicePlan};
pub(crate) use modes::{ArchitecturalMode, SlicingStrategy, TaskGuidedMode};
pub(crate) use skeleton::skeletonize;
