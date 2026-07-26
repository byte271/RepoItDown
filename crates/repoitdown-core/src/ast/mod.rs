pub mod common;
pub mod languages;

use rayon::prelude::*;
use std::path::Path;
use std::sync::Arc;

use crate::error::Result;
use crate::ingestion::FileEntry;
use crate::types::{FileNode, FileRefs, Language, Symbol};

use languages::LanguageRegistry;

/// Extracts symbols and cross-file references from source text.
///
/// Implementations exist for Rust, Python, TypeScript, and Go via tree-sitter;
/// a regex-based fallback handles everything else.
pub trait SymbolExtractor: Send + Sync {
    fn language(&self) -> Language;

    fn extract(&self, source: &str, path: &Path) -> Result<Vec<Symbol>>;

    /// Extracts cross-file references (imports and call sites) from a source file.
    ///
    /// Defaults to none so extractors that cannot resolve references — the regex
    /// fallback, for instance — keep working unchanged. Languages with a
    /// tree-sitter grammar override this.
    fn extract_refs(&self, _source: &str, _path: &Path) -> Result<FileRefs> {
        Ok(FileRefs::default())
    }

    fn is_available(&self) -> bool {
        true
    }
}

#[derive(Clone)]
pub struct ParserPool {
    registry: Arc<LanguageRegistry>,
}

impl ParserPool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            registry: Arc::new(LanguageRegistry::new()),
        }
    }

    #[must_use]
    pub fn parse_all(&self, entries: &[FileEntry]) -> Vec<FileNode> {
        entries
            .par_iter()
            .map(|entry| self.parse_one(entry))
            .collect()
    }

    fn parse_one(&self, entry: &FileEntry) -> FileNode {
        let extractor = self.registry.get(&entry.language);
        let symbols = extractor
            .extract(&entry.source, &entry.path)
            .unwrap_or_else(|e| {
                tracing::warn!(
                    path = %entry.path.display(),
                    error = %e,
                    "primary parser failed, falling back to regex skeletonizer"
                );
                self.registry
                    .get(&Language::Other(Language::UNKNOWN.into()))
                    .extract(&entry.source, &entry.path)
                    .unwrap_or_default()
            });

        // References are best-effort: a file whose symbols parsed fine can still
        // fail reference extraction, and that must not sink the whole file.
        let refs = extractor
            .extract_refs(&entry.source, &entry.path)
            .unwrap_or_else(|e| {
                tracing::debug!(
                    path = %entry.path.display(),
                    error = %e,
                    "reference extraction failed, file will have no graph edges"
                );
                FileRefs::default()
            });

        FileNode {
            path: entry.path.clone(),
            language: entry.language.clone(),
            source: entry.source.clone(),
            symbols,
            token_count: estimate_tokens(&entry.source),
            has_redactions: entry.has_redactions,
            imports: refs.imports,
            calls: refs.calls,
        }
    }
}

impl Default for ParserPool {
    fn default() -> Self {
        Self::new()
    }
}

/// Rough token estimate via word-count heuristic. The precise count is
/// computed later by `count_tokens`; this is a fast pre-pass for parallelism.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "word-count heuristic inherently uses approximate floating ops"
)]
fn estimate_tokens(source: &str) -> usize {
    let word_count = source.split_whitespace().count();
    (word_count as f64 * 1.3) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingestion::FileEntry;
    use crate::types::Language;

    #[test]
    fn parses_rust_file() {
        let entry = FileEntry::new(
            std::path::PathBuf::from("src/main.rs"),
            Language::Rust,
            "fn main() {\n    println!(\"hello\");\n}\n\npub fn helper() -> i32 { 42 }".into(),
            100,
        );
        let pool = ParserPool::new();
        let nodes = pool.parse_all(&[entry]);
        assert_eq!(nodes.len(), 1);
        assert!(!nodes[0].symbols.is_empty());
    }

    #[test]
    fn falls_back_for_unknown_language() {
        let entry = FileEntry::new(
            std::path::PathBuf::from("script.sh"),
            Language::Other("shell".into()),
            "#!/bin/bash\nfunction hello() {\n  echo \"hi\"\n}".into(),
            100,
        );
        let pool = ParserPool::new();
        let nodes = pool.parse_all(&[entry]);
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn parses_multiple_files_in_parallel() {
        let entries: Vec<FileEntry> = (0..10)
            .map(|i| {
                FileEntry::new(
                    std::path::PathBuf::from(format!("file_{i}.rs")),
                    Language::Rust,
                    format!("fn func_{i}() {{}}"),
                    50,
                )
            })
            .collect();

        let pool = ParserPool::new();
        let nodes = pool.parse_all(&entries);
        assert_eq!(nodes.len(), 10);
        for node in &nodes {
            assert!(!node.symbols.is_empty());
        }
    }
}
