use criterion::{black_box, criterion_group, criterion_main, Criterion};
use petgraph::graph::DiGraph;
use repoitdown_core::ast::ParserPool;
use repoitdown_core::graph::{page_rank, CodeGraph, DEFAULT_CONVERGENCE, DEFAULT_DAMPING, DEFAULT_MAX_ITERATIONS};
use repoitdown_core::ingestion::FileEntry;
use repoitdown_core::output::{render, RenderConfig};
use repoitdown_core::tokenizer::count_tokens;
use repoitdown_core::types::{FileNode, Language};
use std::path::PathBuf;

// ── Fixtures ──────────────────────────────────────────────────────────────

/// Build a star graph: center (node 0) is pointed to by every other node.
/// This mimics a real-world "utility module" dependency pattern.
fn build_star(n: usize) -> DiGraph<(), (), u32> {
    let mut g: DiGraph<(), (), u32> = DiGraph::with_capacity(n, n - 1);
    let center = g.add_node(());
    for _ in 1..n {
        let leaf = g.add_node(());
        g.add_edge(leaf, center, ());
    }
    g
}

/// Build a chain with a cross-link, creating a realistic mix of linear and
/// cyclic dependencies.
fn build_diamond(n: usize) -> DiGraph<(), (), u32> {
    let mut g: DiGraph<(), (), u32> = DiGraph::with_capacity(n, n + n / 2);
    let nodes: Vec<_> = (0..n).map(|_| g.add_node(())).collect();
    // Forward chain edges.
    for w in nodes.windows(2) {
        g.add_edge(w[0], w[1], ());
    }
    // Cross-links: every 4th node points to a random earlier one, creating cycles.
    for i in (3..n).step_by(4) {
        g.add_edge(nodes[i], nodes[i - 3], ());
    }
    g
}

/// Generate synthetic Rust source files that produce real AST symbols and calls.
fn synthetic_rust_files(count: usize) -> Vec<FileEntry> {
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let path = PathBuf::from(format!("src/module_{i}.rs"));
        let source = format!(
            "pub fn func_{i}(x: i32) -> i32 {{\n    let y = helper_{i}(x);\n    y + 1\n}}\n\n\
             fn helper_{i}(val: i32) -> i32 {{\n    val * 2\n}}\n"
        );
        entries.push(FileEntry::new(path, Language::Rust, source, 10_000));
    }
    entries
}

/// Build realistic FileNodes (parsed, with symbols) for render benchmarks.
fn realistic_file_nodes(count: usize) -> Vec<FileNode> {
    let entries = synthetic_rust_files(count);
    ParserPool::new().parse_all(&entries)
}

const RENDER_SAMPLE: &str = "\
pub fn authenticate(token: &str) -> Result<User, AuthError> {\n\
    let claims = decode_token(token)?;\n\
    let user = db::users::find_by_id(claims.sub)\n\
        .ok_or(AuthError::UserNotFound)?;\n\
    if user.is_locked() {\n\
        return Err(AuthError::AccountLocked);\n\
    }\n\
    Ok(user)\n\
}\n\
\n\
pub struct AuthError {\n\
    pub kind: ErrorKind,\n\
    pub message: String,\n\
    pub source: Option<Box<dyn std::error::Error>>,\n\
}\n";

// ── Benchmarks ─────────────────────────────────────────────────────────────

fn bench_page_rank(c: &mut Criterion) {
    let mut group = c.benchmark_group("page_rank");

    // Small graph (~100 nodes) — typical single-crate repo.
    let small_star = build_star(100);
    group.bench_function("small_star_100_nodes", |b| {
        b.iter(|| {
            page_rank(
                black_box(&small_star),
                DEFAULT_DAMPING,
                DEFAULT_MAX_ITERATIONS,
                DEFAULT_CONVERGENCE,
            )
        });
    });

    let small_diamond = build_diamond(100);
    group.bench_function("small_diamond_100_nodes", |b| {
        b.iter(|| {
            page_rank(
                black_box(&small_diamond),
                DEFAULT_DAMPING,
                DEFAULT_MAX_ITERATIONS,
                DEFAULT_CONVERGENCE,
            )
        });
    });

    // Medium graph (~1 000 nodes) — medium monorepo / larger crate.
    let medium = build_diamond(1_000);
    group.bench_function("medium_graph_1k_nodes", |b| {
        b.iter(|| {
            page_rank(
                black_box(&medium),
                DEFAULT_DAMPING,
                DEFAULT_MAX_ITERATIONS,
                DEFAULT_CONVERGENCE,
            )
        });
    });

    // Large graph (~10 000 nodes) — stress test.
    let large = build_diamond(10_000);
    group.bench_function("large_graph_10k_nodes", |b| {
        b.iter(|| {
            page_rank(
                black_box(&large),
                DEFAULT_DAMPING,
                DEFAULT_MAX_ITERATIONS,
                DEFAULT_CONVERGENCE,
            )
        });
    });

    group.finish();
}

fn bench_build_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_graph");

    // Pre-parse the files once (parsing is NOT what we're benchmarking here).
    let pool = ParserPool::new();
    let entries_small = synthetic_rust_files(10);
    let nodes_small = pool.parse_all(&entries_small);

    let entries_medium = synthetic_rust_files(50);
    let nodes_medium = pool.parse_all(&entries_medium);

    group.bench_function("10_files", |b| {
        b.iter(|| CodeGraph::build(black_box(&nodes_small)));
    });

    group.bench_function("50_files", |b| {
        b.iter(|| CodeGraph::build(black_box(&nodes_medium)));
    });

    group.finish();
}

fn bench_render(c: &mut Criterion) {
    let config = RenderConfig::default();

    let nodes_10 = realistic_file_nodes(10);
    let nodes_50 = realistic_file_nodes(50);

    let mut group = c.benchmark_group("render");
    group.bench_function("10_files", |b| {
        b.iter(|| render(black_box(&nodes_10), black_box(&config)));
    });
    group.bench_function("50_files", |b| {
        b.iter(|| render(black_box(&nodes_50), black_box(&config)));
    });
    group.finish();
}

fn bench_count_tokens(c: &mut Criterion) {
    let short = "fn main() { println!(\"hello\"); }";
    let medium = RENDER_SAMPLE;
    let long = RENDER_SAMPLE.repeat(10);

    let mut group = c.benchmark_group("count_tokens");
    group.bench_function("short_30_chars", |b| {
        b.iter(|| count_tokens(black_box(short)));
    });
    group.bench_function("medium_500_chars", |b| {
        b.iter(|| count_tokens(black_box(medium)));
    });
    group.bench_function("long_5k_chars", |b| {
        b.iter(|| count_tokens(black_box(&long)));
    });
    group.finish();
}

criterion_group!(benches, bench_page_rank, bench_build_graph, bench_render, bench_count_tokens);
criterion_main!(benches);
