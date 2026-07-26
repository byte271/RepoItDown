//! BM25 intent matching.
//!
//! Indexes a repository's files by their symbol names, docstrings, and paths,
//! then ranks them against a free-text query. The top-k results become the
//! "target files" for Mode 2 (Task-Guided) slicing: these are the files the LLM
//! most likely needs in full source to answer the user's question.
//!
//! ## Algorithm
//!
//! Standard Okapi BM25 with `k1 = 1.2` and `b = 0.75` (the values from the
//! original 1994 paper and the de-facto default in every search library).
//!
//! ```text
//! score(D, Q) = Σ_{t ∈ Q} IDF(t) · (f(t, D) · (k1 + 1))
//!                                 / (f(t, D) + k1 · (1 - b + b · |D| / avgdl))
//!
//! IDF(t) = ln(1 + (N - n(t) + 0.5) / (n(t) + 0.5))
//! ```
//!
//! The `+1` inside `ln` keeps IDF non-negative even when a term appears in
//! every document, which is a known robustness fix over the textbook formula.
//!
//! ## Tokenisation
//!
//! Identifiers are split on `snake_case` and `camelCase` boundaries so that
//! `parseUserRequest`, `parse_user_request`, and `ParseUserRequest` all
//! produce the same token sequence `["parse", "user", "request"]`. Path
//! separators and other punctuation are also split points.

use std::collections::HashMap;

use crate::types::{FileNode, Symbol};

/// Standard BM25 `k1` parameter: controls term-frequency saturation.
pub const DEFAULT_K1: f64 = 1.2;

/// Standard BM25 `b` parameter: controls document-length normalisation.
pub const DEFAULT_B: f64 = 0.75;

/// In-memory BM25 index over a corpus of documents.
///
/// One document = one file's combined text (symbol names + docstrings + path).
/// Construction is `O(corpus_size)`; queries are `O(query_terms · matches)`.
pub struct BM25Index {
    docs: Vec<Document>,
    /// Average document length in tokens. Clamped to 1.0 to avoid divide-by-zero.
    avgdl: f64,
    /// Inverse document frequency per term.
    idf: HashMap<String, f64>,
}

struct Document {
    /// Tokenised text of the file (name + docstrings + path).
    tokens: Vec<String>,
    /// Term frequencies within this document.
    tf: HashMap<String, u32>,
    /// Original file index in the input slice.
    file_index: usize,
}

impl BM25Index {
    /// Builds an index from a slice of file nodes. Each file contributes its
    /// path, every symbol's name, and every symbol's docstring.
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn from_files(nodes: &[FileNode]) -> Self {
        let docs: Vec<Document> = nodes
            .iter()
            .enumerate()
            .map(|(i, node)| {
                let mut text = String::new();
                text.push_str(&node.path.to_string_lossy());
                for symbol in &node.symbols {
                    text.push(' ');
                    // signature_text includes the symbol name (e.g. "fn foo()"
                    // or "struct Bar"), so we don't push symbol.name()
                    // separately — that would double-count the name token.
                    text.push_str(&signature_text(symbol));
                    if let Some(doc) = symbol.docstring() {
                        text.push(' ');
                        text.push_str(doc);
                    }
                }
                let tokens = tokenize(&text);
                let mut tf: HashMap<String, u32> = HashMap::new();
                for token in &tokens {
                    *tf.entry(token.clone()).or_insert(0) += 1;
                }
                Document {
                    tokens,
                    tf,
                    file_index: i,
                }
            })
            .collect();

        let n = docs.len();
        let total_len: usize = docs.iter().map(|d| d.tokens.len()).sum();
        let avgdl = if n == 0 {
            1.0
        } else {
            (total_len as f64 / n as f64).max(1.0)
        };

        // Document frequency per term.
        let mut df: HashMap<String, u32> = HashMap::new();
        for doc in &docs {
            for term in doc.tf.keys() {
                *df.entry(term.clone()).or_insert(0) += 1;
            }
        }

        let n_f64 = n as f64;
        let idf: HashMap<String, f64> = df
            .iter()
            .map(|(term, &freq)| {
                let freq_f64 = f64::from(freq);
                // The +1 inside ln keeps IDF non-negative when freq == n.
                let idf = (1.0 + ((n_f64 - freq_f64 + 0.5) / (freq_f64 + 0.5))).ln();
                (term.clone(), idf)
            })
            .collect();

        Self { docs, avgdl, idf }
    }

    /// Returns the top-k `(file_index, score)` pairs for the query, sorted by
    /// descending score. Files with zero score are excluded.
    ///
    /// If `k` is 0 or the corpus is empty, returns an empty vec.
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn search(&self, query: &str, k: usize) -> Vec<(usize, f64)> {
        if k == 0 || self.docs.is_empty() || query.is_empty() {
            return Vec::new();
        }

        let query_terms = tokenize(query);
        if query_terms.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(usize, f64)> = self
            .docs
            .iter()
            .map(|doc| {
                let mut score = 0.0_f64;
                let dl = doc.tokens.len() as f64;
                for term in &query_terms {
                    let Some(&idf) = self.idf.get(term) else {
                        continue;
                    };
                    let freq = doc.tf.get(term).copied().unwrap_or(0);
                    if freq == 0 {
                        continue;
                    }
                    let freq_f64 = f64::from(freq);
                    let denom =
                        freq_f64 + DEFAULT_K1 * (1.0 - DEFAULT_B + DEFAULT_B * dl / self.avgdl);
                    score += idf * (freq_f64 * (DEFAULT_K1 + 1.0)) / denom;
                }
                (doc.file_index, score)
            })
            .filter(|&(_, s)| s > 0.0)
            .collect();

        // Sort by score descending; tie-break by file_index ascending for determinism.
        scored.sort_by(|(i_a, s_a), (i_b, s_b)| {
            s_b.partial_cmp(s_a)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(i_a.cmp(i_b))
        });

        scored.truncate(k);
        scored
    }
}

/// Extracts a signature-like text representation for a symbol, used purely to
/// feed additional context to the BM25 index.
fn signature_text(symbol: &Symbol) -> String {
    match symbol {
        Symbol::Function(f) => f.signature.clone(),
        Symbol::Struct(s) => format!("struct {}", s.name),
        Symbol::Class(c) => format!("class {}", c.name),
        Symbol::Interface(i) => format!("interface {}", i.name),
        Symbol::Enum(e) => format!("enum {}", e.name),
        Symbol::TypeAlias(t) => format!("type {}", t.name),
        Symbol::Module(m) => format!("mod {}", m.name),
    }
}

/// Splits text into BM25 tokens.
///
/// Rules:
/// - Non-alphanumeric characters are split points.
/// - `snake_case` is split on underscores.
/// - `camelCase` is split on lowercase→uppercase boundaries.
/// - `PascalCase` is split on uppercase-runs.
/// - `URLParser` becomes `["url", "parser"]` (acronym handling).
/// - All tokens are lowercased.
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            current.push(ch);
        } else {
            flush(&mut current, &mut out);
        }
    }
    flush(&mut current, &mut out);

    // Deduplicate consecutive identical tokens to keep doc lengths sane.
    out.dedup();
    out
}

/// Flushes the current buffer, splitting it further if it contains
/// `camelCase` or `PascalCase` boundaries, then lowercasing the result.
fn flush(current: &mut String, out: &mut Vec<String>) {
    if current.is_empty() {
        return;
    }
    // IMPORTANT: split BEFORE lowercasing — the boundary detector relies
    // on the original case to find uppercase markers.
    for token in split_camel_case(current) {
        out.push(token.to_lowercase());
    }
    current.clear();
}

/// Splits a single alphanumeric word on `camelCase` / `PascalCase` / `ACRONYM`
/// boundaries. Returns tokens with their original case preserved — callers
/// are responsible for lowercasing if they want case-insensitive matching.
fn split_camel_case(word: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = word.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let prev_upper = current.chars().last().is_some_and(char::is_uppercase);
        let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();

        if c.is_uppercase() && prev_upper && next_lower && !current.is_empty() {
            // Boundary inside an acronym run: `URLParser` -> split before `P`.
            out.push(std::mem::take(&mut current));
            current.push(c);
        } else if c.is_uppercase()
            && !current.is_empty()
            && current.chars().last().is_some_and(char::is_lowercase)
        {
            // Boundary at lower→upper: `parseUser` -> split before `U`.
            out.push(std::mem::take(&mut current));
            current.push(c);
        } else if c == '_' || c == '-' {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
        i += 1;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileNode, FunctionDef, Language, SourceLocation, Symbol, Visibility};
    use std::path::PathBuf;

    fn node_with(path: &str, symbols: Vec<Symbol>) -> FileNode {
        FileNode {
            path: PathBuf::from(path),
            language: Language::Rust,
            source: String::new(),
            symbols,
            token_count: 0,
            has_redactions: false,
            imports: vec![],
            calls: vec![],
        }
    }

    fn fn_symbol(name: &str, doc: Option<&str>) -> Symbol {
        Symbol::from(FunctionDef {
            name: name.into(),
            visibility: Visibility::Public,
            signature: format!("fn {name}()"),
            docstring: doc.map(str::to_owned),
            parameters: vec![],
            return_type: None,
            body_stripped: false,
            loc: SourceLocation::line_only(PathBuf::from("a.rs"), 1),
        })
    }

    #[test]
    fn tokenize_handles_snake_case() {
        assert_eq!(
            tokenize("parse_user_request"),
            vec!["parse", "user", "request"]
        );
    }

    #[test]
    fn tokenize_handles_camel_case() {
        assert_eq!(
            tokenize("parseUserRequest"),
            vec!["parse", "user", "request"]
        );
    }

    #[test]
    fn tokenize_handles_acronym() {
        // `URLParser` should split into `["url", "parser"]`, not `["u", "r", "l", "parser"]`.
        assert_eq!(tokenize("URLParser"), vec!["url", "parser"]);
    }

    #[test]
    fn tokenize_handles_pascal_case() {
        assert_eq!(tokenize("UserService"), vec!["user", "service"]);
    }

    #[test]
    fn tokenize_splits_on_punctuation() {
        assert_eq!(
            tokenize("src/auth/login.rs"),
            vec!["src", "auth", "login", "rs"]
        );
    }

    #[test]
    fn empty_corpus_returns_empty_index() {
        let idx = BM25Index::from_files(&[]);
        assert!(idx.search("anything", 5).is_empty());
    }

    #[test]
    fn empty_query_returns_empty() {
        let nodes = [node_with("auth.rs", vec![fn_symbol("login", None)])];
        let idx = BM25Index::from_files(&nodes);
        assert!(idx.search("", 5).is_empty());
    }

    #[test]
    fn ranks_relevant_file_first() {
        let nodes = [
            node_with(
                "src/auth.rs",
                vec![fn_symbol(
                    "login",
                    Some("Authenticate a user by credentials"),
                )],
            ),
            node_with(
                "src/util.rs",
                vec![fn_symbol("format_bytes", Some("Pretty-print byte counts"))],
            ),
            node_with(
                "src/db.rs",
                vec![fn_symbol(
                    "query_users",
                    Some("Fetch users from the database"),
                )],
            ),
        ];
        let idx = BM25Index::from_files(&nodes);
        let results = idx.search("user authentication login", 3);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, 0, "auth.rs should rank first");
    }

    #[test]
    fn top_k_truncates_results() {
        let mut nodes = Vec::new();
        for i in 0..10 {
            nodes.push(node_with(
                &format!("src/file_{i}.rs"),
                vec![fn_symbol(&format!("common_fn_{i}"), None)],
            ));
        }
        let idx = BM25Index::from_files(&nodes);
        let results = idx.search("common", 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn zero_k_returns_empty() {
        let nodes = [node_with("auth.rs", vec![fn_symbol("login", None)])];
        let idx = BM25Index::from_files(&nodes);
        assert!(idx.search("login", 0).is_empty());
    }

    #[test]
    fn term_not_in_corpus_returns_empty() {
        let nodes = [node_with("auth.rs", vec![fn_symbol("login", None)])];
        let idx = BM25Index::from_files(&nodes);
        assert!(idx.search("nonexistent_term_xyzzy", 5).is_empty());
    }
}
