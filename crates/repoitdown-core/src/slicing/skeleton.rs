//! AST-based body skeletonization.
//!
//! Replaces function and method bodies with a language-appropriate placeholder,
//! keeping signatures, doc comments, imports, and structural declarations
//! intact. This is the token-saving workhorse of Mode 1 (Architectural): the
//! LLM still sees every signature, so it can reason about the API surface, but
//! the implementation noise is gone.
//!
//! ## How it works
//!
//! 1. Re-parse the file's source with the appropriate tree-sitter grammar.
//! 2. Walk the tree and collect byte ranges of every function/method body.
//! 3. Merge overlapping or adjacent ranges into non-overlapping intervals.
//! 4. Sort by **descending start offset**.
//! 5. Splice each body's byte range, replacing it with the placeholder.
//!
//! Descending-order splicing is critical: if we splice in ascending order,
//! earlier insertions shift the byte offsets of later ranges, invalidating
//! them. Descending order means each splice only affects offsets below the
//! range we're about to process, which we've already handled.
//!
//! ## Language-specific placeholders
//!
//! - **Rust, TypeScript, Go**: `/* ... */` (C-style block comment, valid in all three).
//! - **Python**: `...` (the Ellipsis literal — `def foo(): ...` is valid Python).
//!   `/* ... */` would be a syntax error in Python.
//!
//! ## UTF-8 safety
//!
//! Tree-sitter byte offsets are always on UTF-8 boundaries (it operates on
//! bytes, not characters), so direct string slicing at those offsets is
//! UTF-8-safe. We use `str::replace_range` which requires char boundaries —
//! the tree-sitter offsets satisfy this.

use std::path::Path;

use crate::ast::common::parse_source;
use crate::ast::common::visit_nodes;
use crate::error::Result;
use crate::types::Language;

/// The placeholder substituted for each stripped body in C-syntax languages
/// (Rust, TypeScript, Go). Valid as a block comment in all three.
const BODY_PLACEHOLDER_C: &str = "/* ... */";

/// The placeholder substituted for each stripped body in Python. `...` is the
/// Ellipsis literal and is a valid Python statement, so `def foo(): ...` is
/// syntactically correct.
const BODY_PLACEHOLDER_PYTHON: &str = "...";

/// Returns the language-appropriate body placeholder.
#[must_use]
fn body_placeholder(language: &Language) -> &'static str {
    match language {
        Language::Python => BODY_PLACEHOLDER_PYTHON,
        _ => BODY_PLACEHOLDER_C,
    }
}

/// Skeletonizes a source file by replacing every function and method body
/// with a language-appropriate placeholder.
///
/// Signatures, doc comments, imports, struct definitions, and top-level
/// declarations are preserved unchanged. On parse failure, the original source
/// is returned unmodified (skeletonization is best-effort: a parse failure
/// should never prevent the file from appearing in output).
#[must_use]
pub fn skeletonize(source: &str, language: &Language, path: &Path) -> String {
    let ranges = match collect_body_ranges(source, language, path) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(
                path = %path.display(),
                error = %e,
                "skeletonize parse failed, returning source unchanged"
            );
            return source.to_owned();
        }
    };

    if ranges.is_empty() {
        return source.to_owned();
    }

    // Merge overlapping or nested ranges into a set of non-overlapping
    // intervals. This prevents garbled output when two body ranges partially
    // overlap (which shouldn't happen with well-formed tree-sitter output,
    // but is a safety net).
    let merged = merge_overlapping(ranges);

    // Sort by descending start so splicing earlier ranges doesn't invalidate
    // the offsets of ranges we haven't processed yet.
    let mut sorted = merged;
    sorted.sort_unstable_by_key(|&(start, _)| std::cmp::Reverse(start));

    let placeholder = body_placeholder(language);
    let mut out = source.to_owned();
    for (start, end) in sorted {
        // Tree-sitter guarantees UTF-8-boundary offsets, so this is safe.
        // `replace_range` panics on non-char-boundaries, but those never occur
        // here because ts byte offsets land on UTF-8 boundaries.
        if start <= end && end <= out.len() {
            out.replace_range(start..end, placeholder);
        }
    }
    out
}

/// Merges overlapping or nested ranges into non-overlapping intervals.
///
/// Given `[(0, 10), (5, 15), (20, 30)]`, produces `[(0, 15), (20, 30)]`.
/// This is a standard interval-union: sort by start, then merge when the next
/// range's start is `<=` the current range's end.
fn merge_overlapping(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    let mut current = ranges[0];
    for &(start, end) in &ranges[1..] {
        if start <= current.1 {
            // Overlapping or adjacent — extend the current range.
            current.1 = current.1.max(end);
        } else {
            merged.push(current);
            current = (start, end);
        }
    }
    merged.push(current);
    merged
}

/// Collects the `(start_byte, end_byte)` ranges of every function/method body
/// in the source.
fn collect_body_ranges(
    source: &str,
    language: &Language,
    path: &Path,
) -> Result<Vec<(usize, usize)>> {
    let mut ranges = Vec::new();

    match language {
        Language::Rust => {
            let tree = parse_source(&tree_sitter_rust::LANGUAGE.into(), source, path)?;
            visit_nodes(tree.root_node(), &mut |node| {
                if node.kind() == "function_item" {
                    push_body(node, &mut ranges);
                }
                true
            });
        }
        Language::TypeScript | Language::JavaScript => {
            let lang = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
            let tree = parse_source(&lang, source, path)?;
            visit_nodes(tree.root_node(), &mut |node| {
                if matches!(node.kind(), "function_declaration" | "method_definition") {
                    push_body(node, &mut ranges);
                }
                true
            });
        }
        Language::Python => {
            let tree = parse_source(&tree_sitter_python::LANGUAGE.into(), source, path)?;
            visit_nodes(tree.root_node(), &mut |node| {
                if node.kind() == "function_definition" {
                    push_body(node, &mut ranges);
                }
                true
            });
        }
        Language::Go => {
            let tree = parse_source(&tree_sitter_go::LANGUAGE.into(), source, path)?;
            visit_nodes(tree.root_node(), &mut |node| {
                if matches!(node.kind(), "function_declaration" | "method_declaration") {
                    push_body(node, &mut ranges);
                }
                true
            });
        }
        _ => {
            // Unknown language: nothing to skeletonize.
            return Ok(Vec::new());
        }
    }

    Ok(ranges)
}

/// Records the byte range of a function/method body node.
fn push_body(node: tree_sitter::Node<'_>, ranges: &mut Vec<(usize, usize)>) {
    if let Some(body) = node.child_by_field_name("body") {
        let start = body.start_byte();
        let end = body.end_byte();
        if end > start {
            ranges.push((start, end));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeletonizes_rust_function_body() {
        let src = "pub fn greet(name: &str) -> String {\n    let msg = format!(\"hi {}\", name);\n    msg\n}\n";
        let out = skeletonize(src, &Language::Rust, Path::new("a.rs"));
        assert!(out.contains("pub fn greet"));
        assert!(out.contains("/* ... */"));
        assert!(!out.contains("format!"));
    }

    #[test]
    fn preserves_rust_imports_and_structs() {
        let src = "use std::collections::HashMap;\n\npub struct Config {\n    pub host: String,\n}\n\npub fn load() -> Config {\n    Config { host: \"localhost\".into() }\n}\n";
        let out = skeletonize(src, &Language::Rust, Path::new("a.rs"));
        assert!(out.contains("use std::collections::HashMap;"));
        assert!(out.contains("pub struct Config"));
        assert!(out.contains("pub host: String"));
        assert!(out.contains("/* ... */"));
    }

    #[test]
    fn skeletonizes_typescript_function() {
        let src = "export function greet(name: string): string {\n  return `Hello, ${name}`;\n}\n";
        let out = skeletonize(src, &Language::TypeScript, Path::new("a.ts"));
        assert!(out.contains("export function greet"));
        assert!(out.contains("/* ... */"));
        assert!(!out.contains("Hello"));
    }

    #[test]
    fn skeletonizes_python_function() {
        let src = "def greet(name):\n    msg = f\"hi {name}\"\n    return msg\n";
        let out = skeletonize(src, &Language::Python, Path::new("a.py"));
        assert!(out.contains("def greet"));
        // Python uses `...` (Ellipsis), NOT `/* ... */` which would be a
        // syntax error.
        assert!(out.contains("..."));
        assert!(!out.contains("/* ... */"));
        assert!(!out.contains("hi {name}"));
    }

    #[test]
    fn python_skeleton_is_valid_syntax() {
        // The skeletonized output must be parseable Python. We verify by
        // re-parsing it with tree-sitter — if the placeholder is wrong
        // (e.g. `/* ... */`), the re-parse would fail or produce errors.
        let src = "def greet(name):\n    msg = f\"hi {name}\"\n    return msg\n";
        let out = skeletonize(src, &Language::Python, Path::new("a.py"));
        let reparse = parse_source(
            &tree_sitter_python::LANGUAGE.into(),
            &out,
            Path::new("a.py"),
        );
        assert!(
            reparse.is_ok(),
            "skeletonized Python must be valid syntax, got: {out}"
        );
    }

    #[test]
    fn rust_skeleton_is_valid_syntax() {
        let src = "pub fn greet(name: &str) -> String {\n    format!(\"hi {}\", name)\n}\n";
        let out = skeletonize(src, &Language::Rust, Path::new("a.rs"));
        let reparse = parse_source(&tree_sitter_rust::LANGUAGE.into(), &out, Path::new("a.rs"));
        assert!(
            reparse.is_ok(),
            "skeletonized Rust must be valid syntax, got: {out}"
        );
    }

    #[test]
    fn typescript_skeleton_is_valid_syntax() {
        let src = "export function greet(name: string): string {\n  return `Hello, ${name}`;\n}\n";
        let out = skeletonize(src, &Language::TypeScript, Path::new("a.ts"));
        let lang = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let reparse = parse_source(&lang, &out, Path::new("a.ts"));
        assert!(
            reparse.is_ok(),
            "skeletonized TypeScript must be valid syntax, got: {out}"
        );
    }

    #[test]
    fn go_skeleton_is_valid_syntax() {
        let src = "package main\n\nfunc greet(name string) string {\n\treturn \"hi \" + name\n}\n";
        let out = skeletonize(src, &Language::Go, Path::new("a.go"));
        let reparse = parse_source(&tree_sitter_go::LANGUAGE.into(), &out, Path::new("a.go"));
        assert!(
            reparse.is_ok(),
            "skeletonized Go must be valid syntax, got: {out}"
        );
    }

    #[test]
    fn skeletonizes_go_function() {
        let src = "package main\n\nfunc greet(name string) string {\n\treturn \"hi \" + name\n}\n";
        let out = skeletonize(src, &Language::Go, Path::new("a.go"));
        assert!(out.contains("func greet"));
        assert!(out.contains("/* ... */"));
        assert!(!out.contains("\"hi \""));
    }

    #[test]
    fn skeletonizing_reduces_token_count() {
        // A function with a non-trivial body should shrink after skeletonization.
        let src = "pub fn compute(values: &[i32]) -> i32 {\n    let mut sum = 0;\n    for v in values {\n        sum += v * 2;\n    }\n    sum\n}\n";
        let out = skeletonize(src, &Language::Rust, Path::new("a.rs"));
        assert!(out.len() < src.len(), "skeleton should be shorter");
    }

    #[test]
    fn skeletonize_preserves_signatures_under_extreme_budget() {
        // Even with many bodies, every signature should remain.
        let src =
            "pub fn a() { let x = 1; }\npub fn b() { let y = 2; }\npub fn c() { let z = 3; }\n";
        let out = skeletonize(src, &Language::Rust, Path::new("a.rs"));
        assert!(out.contains("pub fn a"));
        assert!(out.contains("pub fn b"));
        assert!(out.contains("pub fn c"));
    }

    #[test]
    fn unknown_language_returns_source_unchanged() {
        let src = "some random text\n";
        let out = skeletonize(src, &Language::Other("txt".into()), Path::new("a.txt"));
        assert_eq!(out, src);
    }

    #[test]
    fn handles_nested_functions() {
        // Inner function body should be subsumed by the outer body's range.
        let src = "pub fn outer() {\n    let inner = || { 42 };\n    inner()\n}\n";
        let out = skeletonize(src, &Language::Rust, Path::new("a.rs"));
        // The outer body becomes /* ... */, subsuming the inner closure's body.
        let placeholder_count = out.matches("/* ... */").count();
        assert_eq!(
            placeholder_count, 1,
            "nested body should be subsumed, got {out}"
        );
    }

    #[test]
    fn empty_source_returns_empty() {
        let out = skeletonize("", &Language::Rust, Path::new("a.rs"));
        assert_eq!(out, "");
    }

    #[test]
    fn merge_overlapping_ranges() {
        let merged = merge_overlapping(vec![(0, 10), (5, 15), (20, 30)]);
        assert_eq!(merged, vec![(0, 15), (20, 30)]);
    }

    #[test]
    fn merge_nested_ranges() {
        let merged = merge_overlapping(vec![(0, 100), (10, 20), (30, 40)]);
        assert_eq!(merged, vec![(0, 100)]);
    }

    #[test]
    fn merge_disjoint_ranges() {
        let merged = merge_overlapping(vec![(0, 10), (20, 30), (40, 50)]);
        assert_eq!(merged, vec![(0, 10), (20, 30), (40, 50)]);
    }

    #[test]
    fn merge_empty() {
        let merged = merge_overlapping(Vec::new());
        assert!(merged.is_empty());
    }

    #[test]
    fn python_function_with_docstring() {
        // Python docstrings live INSIDE the body block. Skeletonization
        // replaces the entire body (including the docstring) with `...`.
        // The docstring is preserved separately in FunctionDef.docstring by
        // the Python extractor, so this is acceptable.
        let src = "def greet(name):\n    \"\"\"Say hi.\"\"\"\n    return f\"hi {name}\"\n";
        let out = skeletonize(src, &Language::Python, Path::new("a.py"));
        assert!(out.contains("def greet"));
        assert!(out.contains("..."));
        assert!(!out.contains("Say hi"));
    }
}
