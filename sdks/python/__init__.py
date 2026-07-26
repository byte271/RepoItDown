"""
RepoItDown — AST-aware codebase topology for LLM context windows.

Transforms repositories into token-optimized Markdown topologies using
tree-sitter AST parsing, PageRank centrality, and fractional knapsack
token budget allocation.

Usage:
    from repoitdown import RepoItDown

    rd = RepoItDown()
    output = rd.run(".", mode="architect", max_tokens=8000)
    print(output)

Modes:
    - dump:       Full source concatenation (Phase 1 behaviour)
    - explore:    Full source + Contract View of exported symbols
    - architect:  Skeletonized files with PageRank hubs in full source
    - task:       BM25 query targets in full, k-hop deps skeletonized

The native module is built with maturin + PyO3 from the Rust core.
"""

# The native extension module is built and installed by maturin.
try:
    from .repoitdown import RepoItDown
except ImportError:
    # During development (before `maturin develop`), provide a helpful error.
    import sys

    class RepoItDown:  # type: ignore[no-redef]
        def __init__(self) -> None:
            raise ImportError(
                "The native repoitdown module is not built. "
                "Run `pip install maturin && maturin develop --release` "
                "from the sdks/python/ directory to build it."
            )

__all__ = ["RepoItDown"]
__version__ = "0.1.0"
