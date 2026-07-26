//! Python bindings for RepoItDown.
//!
//! Exposes `repoitdown-core::Pipeline` as a Python class `RepoItDown` via PyO3.
//!
//! ## Building
//!
//! ```bash
//! pip install maturin
//! maturin develop --release
//! ```
//!
//! ## Usage
//!
//! ```python
//! from repoitdown import RepoItDown
//!
//! rd = RepoItDown()
//! output = rd.run(".", mode="architect", max_tokens=8000)
//! print(output)
//!
//! # Task-guided mode
//! output = rd.run(".", mode="task", query="fix auth token expiration")
//! ```
//!
//! ## Graph export
//!
//! ```python
//! graph_json = rd.export_graph(".")
//! # Returns JSON string with { nodes: [...], edges: [...] }
//! ```

use pyo3::prelude::*;
use repoitdown_core::{export_graph_json, Pipeline};

/// Python wrapper around `repoitdown-core::Pipeline`.
///
/// A single `RepoItDown` instance can be reused for multiple `run()` calls.
/// Each call is stateless and thread-safe.
#[pyclass(name = "RepoItDown")]
pub struct RepoItDown {
    pipeline: Pipeline,
}

#[pymethods]
impl RepoItDown {
    /// Create a new `RepoItDown` instance with default configuration.
    #[new]
    fn new() -> Self {
        Self {
            pipeline: Pipeline::new(),
        }
    }

    /// Run the pipeline on a repository and return the Markdown topology.
    ///
    /// Args:
    ///     repo_path: Absolute or relative path to the repository root.
    ///     mode: Processing mode — `"dump"`, `"explore"`, `"architect"`, or `"task"`.
    ///     max_tokens: Maximum output tokens. Serves as slicing budget for
    ///         `architect` and `task` modes.
    ///     query: Natural-language query for `task` mode. Required when `mode`
    ///         is `"task"`.
    ///     no_collapse: If `True`, produce plain Markdown without `<details>`.
    ///
    /// Returns:
    ///     The rendered Markdown topology as a string.
    ///
    /// Raises:
    ///     `ValueError`: If the mode or parameters are invalid.
    ///     `RuntimeError`: If the pipeline encounters an unrecoverable error.
    #[pyo3(signature = (repo_path, *, mode="dump", max_tokens=None, query=None, no_collapse=false))]
    fn run(
        &self,
        repo_path: &str,
        mode: &str,
        max_tokens: Option<usize>,
        query: Option<&str>,
        no_collapse: bool,
    ) -> PyResult<String> {
        let repo_path = std::path::PathBuf::from(repo_path);

        let mut pipeline = self.pipeline.clone();
        pipeline
            .configure(mode, query, max_tokens, !no_collapse)
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)?;

        pipeline
            .run(&repo_path)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))
    }

    /// Export the Code Dependency Graph as a JSON string.
    ///
    /// Returns a JSON object with `{ nodes: [...], edges: [...], node_count, edge_count }`
    /// suitable for visualization tools like the RepoItDown D3.js visualizer.
    ///
    /// Args:
    ///     repo_path: Absolute or relative path to the repository root.
    ///
    /// Returns:
    ///     A JSON string, or `None` if the graph is empty (no exported symbols).
    #[pyo3(signature = (repo_path))]
    fn export_graph(&self, repo_path: &str) -> PyResult<Option<String>> {
        use repoitdown_core::ast::ParserPool;
        use repoitdown_core::ingestion::walker::RepoWalker;
        use repoitdown_core::ingestion::IngestionConfig;

        let repo_path = std::path::PathBuf::from(repo_path);
        let walker = RepoWalker::new(IngestionConfig::default());
        let result = walker.walk(&repo_path).map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string())
        })?;

        let pool = ParserPool::new();
        let files = pool.parse_all(&result.files);

        match export_graph_json(&files) {
            Some(export) => {
                let json = serde_json::to_string(&export).map_err(|e| {
                    PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string())
                })?;
                Ok(Some(json))
            }
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        "RepoItDown()".to_string()
    }
}

/// Python module entry point.
#[pymodule]
fn repoitdown(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<RepoItDown>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("__doc__", "AST-aware codebase topology for LLM context windows.\n\nUsage:\n    from repoitdown import RepoItDown\n    rd = RepoItDown()\n    output = rd.run('.', mode='architect', max_tokens=8000)")?;
    Ok(())
}
