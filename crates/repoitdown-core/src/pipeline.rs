use std::path::Path;

use crate::ast::ParserPool;
use crate::error::Result;
use crate::graph::{
    CodeGraph, DEFAULT_CONVERGENCE, DEFAULT_DAMPING, DEFAULT_MAX_ITERATIONS, page_rank,
};
use crate::ingestion::IngestionConfig;
use crate::ingestion::walker::RepoWalker;
use crate::output::{RenderConfig, render};
use crate::slicing::{ArchitecturalMode, SliceLevel, SlicePlan, SlicingStrategy, TaskGuidedMode};
use crate::tokenizer::count_tokens;
use crate::types::FileNode;

/// Selects which slicing strategy (if any) the pipeline should run between
/// token counting and rendering.
///
/// `None` (the default) preserves the Phase 1 behaviour: full source for
/// every file, no skeletonization, no graph.
#[derive(Clone)]
#[non_exhaustive]
pub enum SliceMode {
    /// No slicing — Phase 1 behaviour. Used by `dump` and `explore` CLI modes.
    None,
    /// Mode 1 (Architectural): every file skeletonized, PageRank hubs in full.
    Architectural,
    /// Mode 2 (Task-Guided): BM25 query selects targets (full source),
    /// k-hop dependencies skeletonized, rest degraded to signatures.
    Task {
        /// The free-text user query.
        query: String,
    },
}

impl SliceMode {
    /// Parse a mode string (with optional query) into a `SliceMode`.
    ///
    /// Centralizes the mode parsing logic used by the CLI, MCP server,
    /// REST API, and Python SDK so mode strings are handled consistently.
    ///
    /// # Errors
    ///
    /// Returns an error string suitable for user display if:
    /// - The mode string is not one of `dump`, `explore`, `architect`, or `task`.
    /// - The mode is `task` but `query` is `None` or empty.
    pub fn from_str(
        mode: &str,
        query: Option<&str>,
    ) -> std::result::Result<(Self, bool), &'static str> {
        match mode {
            "dump" | "explore" => Ok((Self::None, false)),
            "architect" => Ok((Self::Architectural, true)),
            "task" => query.filter(|s| !s.trim().is_empty()).map_or(
                Err("mode 'task' requires a non-empty 'query' parameter"),
                |q| {
                    Ok((
                        Self::Task {
                            query: q.to_owned(),
                        },
                        true,
                    ))
                },
            ),
            _ => Err("unknown mode. Valid modes: dump, explore, architect, task"),
        }
    }
}

#[derive(Clone)]
pub struct Pipeline {
    ingestion_config: IngestionConfig,
    parser_pool: ParserPool,
    render_config: RenderConfig,
    slice_mode: SliceMode,
    /// Optional token budget for sliced output. If `None`, the renderer's
    /// `max_tokens` (if any) is used as the budget.
    slice_budget: Option<usize>,
}

impl Pipeline {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ingestion_config: IngestionConfig::default(),
            parser_pool: ParserPool::new(),
            render_config: RenderConfig::default(),
            slice_mode: SliceMode::None,
            slice_budget: None,
        }
    }

    #[must_use]
    pub fn with_ingestion_config(mut self, config: IngestionConfig) -> Self {
        self.ingestion_config = config;
        self
    }

    #[must_use]
    pub fn with_render_config(mut self, config: RenderConfig) -> Self {
        self.render_config = config;
        self
    }

    /// Sets the slicing mode. When set to anything other than [`SliceMode::None`],
    /// the pipeline runs the CDG + PageRank + slicing strategy between token
    /// counting and rendering.
    #[must_use]
    pub fn with_slice_mode(mut self, mode: SliceMode) -> Self {
        self.slice_mode = mode;
        self
    }

    /// Sets an explicit token budget for the slicing allocator. If unset, the
    /// pipeline falls back to `RenderConfig::max_tokens`, or no budget if that
    /// is also `None`.
    #[must_use]
    pub fn with_slice_budget(mut self, budget: usize) -> Self {
        self.slice_budget = Some(budget);
        self
    }

    /// Configure the pipeline from raw mode and display parameters.
    ///
    /// Centralizes the repeated pattern found in every API crate (CLI, MCP,
    /// REST, Python): parse the mode string, validate `max_tokens`, construct
    /// `RenderConfig`, and enable slicing if appropriate.
    ///
    /// # Errors
    ///
    /// Returns a static error string if the mode is invalid or `max_tokens`
    /// is zero. Callers convert this to their own error type.
    pub fn configure(
        &mut self,
        mode: &str,
        query: Option<&str>,
        max_tokens: Option<usize>,
        collapse: bool,
    ) -> std::result::Result<(), &'static str> {
        if let Some(0) = max_tokens {
            return Err("max_tokens must be at least 1");
        }

        let (slice_mode, use_slicing) = SliceMode::from_str(mode, query)?;

        self.render_config = RenderConfig {
            collapse,
            contract_view: matches!(mode, "explore" | "architect" | "task"),
            max_tokens,
        };

        self.slice_mode = slice_mode;
        if use_slicing {
            if let Some(budget) = max_tokens {
                self.slice_budget = Some(budget);
            }
        } else {
            self.slice_budget = None;
        }

        Ok(())
    }

    pub fn run(&self, root: &Path) -> Result<String> {
        let walker = RepoWalker::new(self.ingestion_config.clone());
        let result = walker.walk(root)?;

        let mut files: Vec<FileNode> = self
            .parser_pool
            .parse_all(&result.files)
            .into_iter()
            .map(|node| {
                // Token counting is fallible; on failure, fall back to the
                // parser pool's heuristic estimate rather than sinking the
                // whole pipeline.
                let token_count = count_tokens(&node.source).unwrap_or(node.token_count);
                FileNode {
                    token_count,
                    ..node
                }
            })
            .collect();

        if !matches!(self.slice_mode, SliceMode::None) {
            apply_slicing(
                &mut files,
                &self.slice_mode,
                self.slice_budget.or(self.render_config.max_tokens),
            );
        }

        files.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(render(&files, &self.render_config))
    }
}

/// Runs the CDG, PageRank, and the selected slicing strategy against `files`,
/// mutating each file in place according to its [`SlicePlan`].
fn apply_slicing(files: &mut Vec<FileNode>, mode: &SliceMode, budget: Option<usize>) {
    // No budget means "no upper bound" — use a sentinel that the allocator
    // treats as effectively infinite (every plan stays at its starting level).
    let budget = budget.unwrap_or(usize::MAX);

    let graph = CodeGraph::build(files);
    let scores = page_rank(
        graph.inner(),
        DEFAULT_DAMPING,
        DEFAULT_MAX_ITERATIONS,
        DEFAULT_CONVERGENCE,
    );

    let plans: Vec<SlicePlan> = match mode {
        SliceMode::Architectural => ArchitecturalMode.plan(files, &graph, &scores, budget),
        SliceMode::Task { query } => {
            TaskGuidedMode::new(query.clone()).plan(files, &graph, &scores, budget)
        }
        SliceMode::None => return,
    };

    // Apply the plans in reverse so removing files doesn't shift indices.
    for plan in plans.iter().rev() {
        let Some(node) = files.get_mut(plan.file_index) else {
            continue;
        };
        match plan.level {
            SliceLevel::Full => {
                // No change.
            }
            SliceLevel::Skeleton => {
                node.source = crate::slicing::skeletonize(&node.source, &node.language, &node.path);
                node.token_count = count_tokens(&node.source).unwrap_or(node.token_count / 2);
            }
            SliceLevel::Signature => {
                node.source = render_signatures(node);
                node.token_count = count_tokens(&node.source).unwrap_or(0);
            }
            SliceLevel::Omitted => {
                // Mark for removal by clearing the source; we filter below.
                node.source.clear();
                node.token_count = 0;
            }
        }
    }

    // Drop omitted files in place.
    files.retain(|n| !n.source.is_empty());
}

/// Renders only the signature lines of a file's symbols. Used for the
/// `Signature` slice level, where the LLM needs to know what symbols exist
/// but doesn't need their bodies or field details.
///
/// Uses the language-appropriate line-comment prefix so the output is valid
/// syntax in the file's language (e.g. `#` for Python, `//` for Rust/Go/TS).
fn render_signatures(node: &FileNode) -> String {
    use std::fmt::Write;
    let comment = comment_prefix(&node.language);
    let mut out = String::new();
    let _ = writeln!(out, "{comment} {}", node.path.display());
    for symbol in &node.symbols {
        let loc = symbol.location();
        let _ = writeln!(
            out,
            "{comment} L{line}: {kind} {name}",
            comment = comment,
            line = loc.line_start,
            kind = symbol.kind_label(),
            name = symbol.name(),
        );
    }
    out
}

/// Returns the line-comment prefix for the given language.
///
/// - Python, Ruby: `#`
/// - Rust, Go, TypeScript, JavaScript, C, C++, Java, C#, Swift, Kotlin: `//`
/// - Unknown: `#` (the most universally safe default for text files)
#[must_use]
fn comment_prefix(language: &crate::types::Language) -> &'static str {
    match language {
        crate::types::Language::Rust
        | crate::types::Language::Go
        | crate::types::Language::TypeScript
        | crate::types::Language::JavaScript
        | crate::types::Language::C
        | crate::types::Language::Cpp
        | crate::types::Language::Java
        | crate::types::Language::CSharp
        | crate::types::Language::Swift
        | crate::types::Language::Kotlin => "//",
        // Python, Ruby, and all unknown languages use `#`.
        _ => "#",
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use tempfile::TempDir;

    fn write_repo(files: &[(&str, &str)]) -> TempDir {
        let tmp = TempDir::new().unwrap();
        for (path, contents) in files {
            let full = tmp.path().join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full, contents).unwrap();
        }
        tmp
    }

    #[test]
    fn runs_on_temp_dir() {
        let tmp = std::env::temp_dir().join("repoitdown_pipeline_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let pipeline = Pipeline::new();
        let output = pipeline.run(&tmp).unwrap();

        assert!(output.contains("main.rs"));
        assert!(output.contains("```rs"));
        assert!(output.contains("fn main"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn runs_on_empty_dir() {
        let tmp = std::env::temp_dir().join("repoitdown_empty_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let pipeline = Pipeline::new();
        let output = pipeline.run(&tmp).unwrap();

        assert!(output.contains("0 files"));
        assert!(output.contains("0 tokens"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn builder_pattern_works() {
        let config = IngestionConfig {
            max_file_size: 512,
            ..IngestionConfig::default()
        };
        let pipeline = Pipeline::new().with_ingestion_config(config);
        assert_eq!(pipeline.ingestion_config.max_file_size, 512);
    }

    #[test]
    fn dump_mode_unchanged_by_phase_2() {
        // dump mode (SliceMode::None) must produce identical output to Phase 1:
        // full source for every file, no skeletonization, no `/* ... */`.
        let tmp = write_repo(&[("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }")]);
        let pipeline = Pipeline::new();
        let output = pipeline.run(tmp.path()).unwrap();
        assert!(output.contains("pub fn add"));
        assert!(output.contains("a + b"));
        // No skeletonization placeholders should appear in dump mode.
        assert!(!output.contains("/* ... */"));
        assert!(!output.contains("..."));
    }

    #[test]
    fn dump_and_architect_produce_different_output() {
        // Architect mode skeletonizes non-hubs; dump mode does not.
        // Their outputs must differ.
        let tmp = write_repo(&[
            (
                "src/lib.rs",
                "pub fn main() { helper(); }\npub fn helper() { main(); }",
            ),
            ("src/util.rs", "pub fn util_fn() { let x = 1; x + 1 }"),
        ]);

        let dump_output = Pipeline::new().run(tmp.path()).unwrap();
        let arch_output = Pipeline::new()
            .with_slice_mode(SliceMode::Architectural)
            .with_slice_budget(10_000)
            .run(tmp.path())
            .unwrap();

        // Dump mode has the full body; architect mode has a placeholder.
        assert!(dump_output.contains("let x = 1"));
        assert!(arch_output.contains("/* ... */"));
    }

    #[test]
    fn architectural_mode_skeletonizes_non_hub_files() {
        // Two files: lib.rs contains a mutual-recursion pair (main <-> helper)
        // which gives them high PageRank. util.rs is an isolated, dangling
        // node with low PageRank — it should be skeletonized.
        let tmp = write_repo(&[
            (
                "src/lib.rs",
                "pub fn main() { helper(); }\npub fn helper() { main(); }",
            ),
            (
                "src/util.rs",
                "pub fn helper_two() { let x = 1; let y = 2; x + y }",
            ),
        ]);
        let pipeline = Pipeline::new()
            .with_slice_mode(SliceMode::Architectural)
            .with_slice_budget(10_000);
        let output = pipeline.run(tmp.path()).unwrap();
        // util.rs is not a hub; it should be skeletonized.
        assert!(
            output.contains("/* ... */"),
            "architectural mode should skeletonize at least one file"
        );
    }

    #[test]
    fn task_mode_requires_query_but_pipeline_accepts_it() {
        // The CLI is responsible for validating that --mode task has --query.
        // The pipeline just trusts the SliceMode::Task variant.
        let tmp = write_repo(&[
            (
                "src/auth.rs",
                "pub fn login(user: &str) -> bool { !user.is_empty() }",
            ),
            (
                "src/util.rs",
                "pub fn format_bytes(b: usize) -> String { format!(\"{}B\", b) }",
            ),
        ]);
        let pipeline = Pipeline::new()
            .with_slice_mode(SliceMode::Task {
                query: "login authentication".into(),
            })
            .with_slice_budget(10_000);
        let output = pipeline.run(tmp.path()).unwrap();
        // auth.rs is the BM25 target — its body should appear verbatim.
        assert!(output.contains("!user.is_empty()"));
    }

    #[test]
    fn omitted_files_do_not_appear_in_output() {
        // With a tiny budget, low-importance files should be omitted entirely.
        // throwaway.rs has no incoming edges and isn't a hub; with budget 1
        // it should be omitted (not even its path should appear in output).
        let tmp = write_repo(&[
            (
                "src/important.rs",
                "pub fn important() { helper(); }\npub fn helper() { important(); }",
            ),
            (
                "src/throwaway.rs",
                "pub fn unused() { let x = 1; let y = 2; let z = 3; }",
            ),
        ]);
        let pipeline = Pipeline::new()
            .with_slice_mode(SliceMode::Architectural)
            .with_slice_budget(1);
        let output = pipeline.run(tmp.path()).unwrap();
        // throwaway.rs should be omitted — its path should NOT appear.
        assert!(
            !output.contains("throwaway"),
            "omitted file should not appear in output, but got: {output}"
        );
    }

    #[test]
    fn all_files_appear_without_language_filtering() {
        // Without a language filter, all files appear regardless of extension.
        // (Language filtering via --languages is a CLI-level concern; the
        // pipeline itself doesn't filter.)
        let tmp = write_repo(&[("src/a.rs", "pub fn a() {}"), ("src/b.py", "def b(): pass")]);
        let pipeline = Pipeline::new().with_render_config(RenderConfig {
            collapse: false,
            contract_view: false,
            max_tokens: None,
        });
        let output = pipeline.run(tmp.path()).unwrap();
        assert!(output.contains("a.rs"));
        assert!(output.contains("b.py"));
    }

    #[test]
    fn python_file_skeletonized_correctly() {
        // Python skeletonization must produce valid Python (using `...`, not
        // `/* ... */` which would be a syntax error).
        //
        // We need two files: the Rust file has mutual recursion (high PageRank,
        // becomes a hub → stays Full). The Python file is a dangling node
        // (low PageRank → skeletonized to `...`).
        let tmp = write_repo(&[
            (
                "src/lib.rs",
                "pub fn main() { helper(); }\npub fn helper() { main(); }",
            ),
            (
                "src/app.py",
                "def greet(name):\n    msg = f'hi {name}'\n    return msg\n",
            ),
        ]);
        let pipeline = Pipeline::new()
            .with_slice_mode(SliceMode::Architectural)
            .with_slice_budget(10_000);
        let output = pipeline.run(tmp.path()).unwrap();
        // The Python file should be skeletonized with `...` (Ellipsis),
        // NOT `/* ... */` which would be a Python syntax error.
        assert!(
            output.contains("..."),
            "Python skeleton should use `...`, got: {output}"
        );
        assert!(
            !output.contains("/* ... */"),
            "Python skeleton must not use C-style comment, got: {output}"
        );
    }
}
